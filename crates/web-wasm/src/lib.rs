use std::sync::{Arc, LazyLock, RwLock};

use clipper_client::engine::{AppState, ClipboardPayload, SyncEngine, TEXT_CLIPBOARD_MIME_TYPE};
use js_sys::{Object, Promise, Reflect, Uint8Array};
use tokio::sync::watch;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;
use zeroize::Zeroizing;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8787";
const DEFAULT_DEVICE_NAME: &str = "Web";
const PLATFORM: &str = "web";

/// Holds the single [`SyncEngine`] for the page's lifetime.
///
/// Unlike the daemon — a long-lived process that rebuilds its engine across many
/// login sessions — the browser client lives for one page load, so it builds the
/// engine once, lazily, on the first login/register using the server URL that
/// request carries (the login form sources it from `VITE_SERVER_URL`). This
/// mirrors the mobile UniFFI client, which is constructed with a runtime base
/// URL rather than a compile-time constant; `DEFAULT_BASE_URL` is only the
/// dev/localhost fallback when no URL is supplied. Once built, the engine's URL
/// is fixed (see [`ensure_requested_base_url`]); a page reload starts fresh.
struct EngineHolder {
    slot: RwLock<Option<Arc<SyncEngine>>>,
    // Bumped when the engine is installed so a `wait_for_state_change` that began
    // before login (no engine yet) wakes and begins tracking the new engine.
    installed: watch::Sender<u64>,
}

static HOLDER: LazyLock<EngineHolder> = LazyLock::new(|| {
    let (installed, _) = watch::channel(0);
    EngineHolder {
        slot: RwLock::new(None),
        installed,
    }
});

impl EngineHolder {
    fn engine(&self) -> Option<Arc<SyncEngine>> {
        self.slot.read().expect("engine slot poisoned").clone()
    }

    /// Return the engine, building it bound to `requested` the first time. Once an
    /// engine exists, `requested` must match its base URL; an empty request
    /// imposes no constraint. The body holds no `.await`, so on the single-threaded
    /// wasm runtime the check-and-build is atomic.
    fn get_or_build(&self, requested: &str) -> Result<Arc<SyncEngine>, JsValue> {
        let mut slot = self.slot.write().expect("engine slot poisoned");
        if let Some(engine) = slot.as_ref() {
            ensure_requested_base_url(engine, requested)?;
            return Ok(Arc::clone(engine));
        }
        let trimmed = requested.trim();
        let url = if trimmed.is_empty() {
            DEFAULT_BASE_URL
        } else {
            trimmed
        };
        let engine = SyncEngine::try_new_with_data_dir(url, "web").map_err(js_error)?;
        *slot = Some(Arc::clone(&engine));
        drop(slot);
        self.installed
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        Ok(engine)
    }

    async fn current_state(&self) -> AppState {
        match self.engine() {
            Some(engine) => engine.get_state().await,
            None => AppState::default(),
        }
    }

    fn state_version(&self) -> u64 {
        self.engine().map_or(0, |engine| engine.state_version())
    }

    /// Suspend until the state version advances past `seen`. Before login there is
    /// no engine and the version is fixed at 0, so wait for an engine to be
    /// installed; afterwards delegate to the engine's own change watch. The engine
    /// lives for the rest of the page, so its version stays monotonic.
    async fn wait_for_state_change(&self, seen: u64) -> Result<u64, JsValue> {
        // Subscribe before the first check so an install that races the check is
        // not missed.
        let mut installed = self.installed.subscribe();
        loop {
            if let Some(engine) = self.engine() {
                return engine
                    .wait_for_state_change_after(seen)
                    .await
                    .map_err(js_error);
            }
            installed
                .changed()
                .await
                .map_err(|_| js_error("engine holder closed"))?;
        }
    }
}

/// Resolve the engine for an operation that requires a session, erroring if the
/// user has not logged in or registered yet this page load.
fn engine_or_error() -> Result<Arc<SyncEngine>, JsValue> {
    HOLDER.engine().ok_or_else(|| js_error("Not logged in"))
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen(js_name = initClient)]
pub fn init_client() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen(js_name = defaultServerUrl)]
pub fn default_server_url() -> String {
    DEFAULT_BASE_URL.to_string()
}

#[wasm_bindgen(js_name = connect)]
pub fn connect() -> Promise {
    ok_promise(async { Ok(JsValue::UNDEFINED) })
}

#[wasm_bindgen(js_name = login)]
pub fn login(
    passphrase: String,
    username: String,
    device_name: String,
    server_url: String,
) -> Promise {
    ok_promise(async move {
        let passphrase = Zeroizing::new(passphrase);
        let engine = HOLDER.get_or_build(&server_url)?;
        engine
            .login_with_platform(
                &passphrase,
                &username,
                non_empty_or_default(&device_name, DEFAULT_DEVICE_NAME),
                PLATFORM,
            )
            .await
            .map_err(js_error)?;
        Ok(JsValue::UNDEFINED)
    })
}

#[wasm_bindgen(js_name = register)]
pub fn register(
    access_key: String,
    username: String,
    passphrase: String,
    device_name: String,
    server_url: String,
) -> Promise {
    ok_promise(async move {
        let access_key = Zeroizing::new(access_key);
        let passphrase = Zeroizing::new(passphrase);
        let engine = HOLDER.get_or_build(&server_url)?;
        let username = engine
            .register_with_platform(
                &access_key,
                &username,
                &passphrase,
                non_empty_or_default(&device_name, DEFAULT_DEVICE_NAME),
                PLATFORM,
            )
            .await
            .map_err(js_error)?;
        Ok(JsValue::from(username))
    })
}

#[wasm_bindgen(js_name = logout)]
pub fn logout() -> Promise {
    ok_promise(async {
        if let Some(engine) = HOLDER.engine() {
            engine.logout().await.map_err(js_error)?;
        }
        Ok(JsValue::UNDEFINED)
    })
}

#[wasm_bindgen(js_name = getState)]
pub fn get_state() -> Promise {
    ok_promise(async {
        let state = HOLDER.current_state().await;
        let value = serde_wasm_bindgen::to_value(&state).map_err(js_error)?;
        Ok(value)
    })
}

#[wasm_bindgen(js_name = stateVersion)]
pub fn state_version() -> f64 {
    HOLDER.state_version() as f64
}

#[wasm_bindgen(js_name = waitForStateChange)]
pub fn wait_for_state_change(seen_version: f64) -> Promise {
    let seen_version = js_number_to_version(seen_version);
    ok_promise(async move {
        let version = HOLDER.wait_for_state_change(seen_version).await?;
        Ok(JsValue::from_f64(version as f64))
    })
}

#[wasm_bindgen(js_name = refresh)]
pub fn refresh() -> Promise {
    ok_promise(async {
        engine_or_error()?.refresh().await.map_err(js_error)?;
        Ok(JsValue::UNDEFINED)
    })
}

#[wasm_bindgen(js_name = sendClipboardText)]
pub fn send_clipboard_text(text: String) -> Promise {
    ok_promise(async move {
        let id = engine_or_error()?
            .send_clipboard_payload(TEXT_CLIPBOARD_MIME_TYPE, text.as_bytes())
            .await
            .map_err(js_error)?;
        Ok(JsValue::from(id))
    })
}

#[wasm_bindgen(js_name = sendClipboardPayload)]
pub fn send_clipboard_payload(mime_type: String, bytes: Uint8Array) -> Promise {
    ok_promise(async move {
        let id = engine_or_error()?
            .send_clipboard_payload(&mime_type, &bytes.to_vec())
            .await
            .map_err(js_error)?;
        Ok(JsValue::from(id))
    })
}

#[wasm_bindgen(js_name = clipboardPayload)]
pub fn clipboard_payload(id: String) -> Promise {
    ok_promise(async move {
        let payload = engine_or_error()?
            .clipboard_payload(&id)
            .await
            .map_err(js_error)?;
        clipboard_payload_value(payload)
    })
}

#[wasm_bindgen(js_name = uploadFileBytes)]
pub fn upload_file_bytes(filename: String, mime_type: String, bytes: Uint8Array) -> Promise {
    ok_promise(async move {
        let id = engine_or_error()?
            .upload_file_bytes(&filename, Some(&mime_type), &bytes.to_vec())
            .await
            .map_err(js_error)?;
        Ok(JsValue::from(id))
    })
}

#[wasm_bindgen(js_name = downloadFileBytes)]
pub fn download_file_bytes(file_id: String) -> Promise {
    ok_promise(async move {
        let bytes = engine_or_error()?
            .download_file_bytes(&file_id)
            .await
            .map_err(js_error)?;
        Ok(Uint8Array::from(bytes.as_slice()).into())
    })
}

#[wasm_bindgen(js_name = deleteFile)]
pub fn delete_file(file_id: String) -> Promise {
    ok_promise(async move {
        engine_or_error()?
            .delete_file(&file_id)
            .await
            .map_err(js_error)?;
        Ok(JsValue::UNDEFINED)
    })
}

#[wasm_bindgen(js_name = createCollabDoc)]
pub fn create_collab_doc() -> Promise {
    ok_promise(async {
        let item = engine_or_error()?
            .create_collab_doc()
            .await
            .map_err(js_error)?;
        let value = serde_wasm_bindgen::to_value(&item).map_err(js_error)?;
        Ok(value)
    })
}

#[wasm_bindgen(js_name = deleteCollabDoc)]
pub fn delete_collab_doc(object_id: String) -> Promise {
    ok_promise(async move {
        engine_or_error()?
            .delete_collab_doc(&object_id)
            .await
            .map_err(js_error)?;
        Ok(JsValue::UNDEFINED)
    })
}

#[wasm_bindgen(js_name = getCollabDocMeta)]
pub fn get_collab_doc_meta(object_id: String) -> Promise {
    ok_promise(async move {
        let item = engine_or_error()?
            .get_collab_doc_meta(&object_id)
            .await
            .map_err(js_error)?;
        let value = serde_wasm_bindgen::to_value(&item).map_err(js_error)?;
        Ok(value)
    })
}

#[wasm_bindgen(js_name = listDevices)]
pub fn list_devices() -> Promise {
    ok_promise(async {
        let devices = engine_or_error()?.list_devices().await.map_err(js_error)?;
        let value = serde_wasm_bindgen::to_value(&devices).map_err(js_error)?;
        Ok(value)
    })
}

#[wasm_bindgen(js_name = removeDevice)]
pub fn remove_device(device_id: String) -> Promise {
    ok_promise(async move {
        engine_or_error()?
            .remove_device(&device_id)
            .await
            .map_err(js_error)?;
        Ok(JsValue::UNDEFINED)
    })
}

/// Guard that the engine's bound URL matches a later per-request URL. An empty
/// request imposes no constraint. The engine binds to the first login/register
/// URL, so this only rejects an attempt to switch servers without a page reload.
fn ensure_requested_base_url(engine: &SyncEngine, requested: &str) -> Result<(), JsValue> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Ok(());
    }
    let configured = engine.base_url();
    if normalize_server_url(requested) == normalize_server_url(&configured) {
        return Ok(());
    }
    Err(js_error(format!(
        "Server URL is fixed for this session: configured {configured}, requested {requested}"
    )))
}

fn normalize_server_url(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

fn non_empty_or_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.trim().is_empty() {
        default
    } else {
        value
    }
}

fn js_number_to_version(version: f64) -> u64 {
    if version.is_finite() && version > 0.0 {
        version.floor() as u64
    } else {
        0
    }
}

fn ok_promise<F>(future: F) -> Promise
where
    F: std::future::Future<Output = Result<JsValue, JsValue>> + 'static,
{
    future_to_promise(future)
}

fn js_error(error: impl ToString) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

fn clipboard_payload_value(payload: ClipboardPayload) -> Result<JsValue, JsValue> {
    let object = Object::new();
    Reflect::set(
        &object,
        &JsValue::from("mimeType"),
        &JsValue::from(payload.mime_type),
    )?;
    Reflect::set(
        &object,
        &JsValue::from("bytes"),
        &Uint8Array::from(payload.bytes.as_slice()),
    )?;
    Reflect::set(
        &object,
        &JsValue::from("text"),
        &payload.text.map(JsValue::from).unwrap_or(JsValue::NULL),
    )?;
    Ok(object.into())
}
