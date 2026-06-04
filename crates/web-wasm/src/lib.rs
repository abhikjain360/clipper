use std::sync::{Arc, LazyLock};

use clipper_client::engine::{ClipboardPayload, SyncEngine, TEXT_CLIPBOARD_MIME_TYPE};
use js_sys::{Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8787";
const DEFAULT_DEVICE_NAME: &str = "Web";
const PLATFORM: &str = "web";

static ENGINE: LazyLock<Arc<SyncEngine>> =
    LazyLock::new(|| SyncEngine::new_with_data_dir(DEFAULT_BASE_URL, "web"));

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
        let engine = engine();
        ensure_requested_base_url(&engine, &server_url).await?;
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
        let engine = engine();
        ensure_requested_base_url(&engine, &server_url).await?;
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
        engine().logout().await.map_err(js_error)?;
        Ok(JsValue::UNDEFINED)
    })
}

#[wasm_bindgen(js_name = getState)]
pub fn get_state() -> Promise {
    ok_promise(async {
        let state = engine().get_state().await;
        let value = serde_wasm_bindgen::to_value(&state).map_err(js_error)?;
        Ok(value)
    })
}

#[wasm_bindgen(js_name = stateVersion)]
pub fn state_version() -> f64 {
    engine().state_version() as f64
}

#[wasm_bindgen(js_name = waitForStateChange)]
pub fn wait_for_state_change(seen_version: f64) -> Promise {
    let seen_version = js_number_to_version(seen_version);
    ok_promise(async move {
        let version = engine()
            .wait_for_state_change_after(seen_version)
            .await
            .map_err(js_error)?;
        Ok(JsValue::from_f64(version as f64))
    })
}

#[wasm_bindgen(js_name = refresh)]
pub fn refresh() -> Promise {
    ok_promise(async {
        engine().refresh().await.map_err(js_error)?;
        Ok(JsValue::UNDEFINED)
    })
}

#[wasm_bindgen(js_name = sendClipboardText)]
pub fn send_clipboard_text(text: String) -> Promise {
    ok_promise(async move {
        let id = engine()
            .send_clipboard_payload(TEXT_CLIPBOARD_MIME_TYPE, text.as_bytes())
            .await
            .map_err(js_error)?;
        Ok(JsValue::from(id))
    })
}

#[wasm_bindgen(js_name = sendClipboardPayload)]
pub fn send_clipboard_payload(mime_type: String, bytes: Uint8Array) -> Promise {
    ok_promise(async move {
        let id = engine()
            .send_clipboard_payload(&mime_type, &bytes.to_vec())
            .await
            .map_err(js_error)?;
        Ok(JsValue::from(id))
    })
}

#[wasm_bindgen(js_name = clipboardPayload)]
pub fn clipboard_payload(id: String) -> Promise {
    ok_promise(async move {
        let payload = engine().clipboard_payload(&id).await.map_err(js_error)?;
        clipboard_payload_value(payload)
    })
}

#[wasm_bindgen(js_name = uploadFileBytes)]
pub fn upload_file_bytes(filename: String, mime_type: String, bytes: Uint8Array) -> Promise {
    ok_promise(async move {
        let id = engine()
            .upload_file_bytes(&filename, Some(&mime_type), &bytes.to_vec())
            .await
            .map_err(js_error)?;
        Ok(JsValue::from(id))
    })
}

#[wasm_bindgen(js_name = downloadFileBytes)]
pub fn download_file_bytes(file_id: String) -> Promise {
    ok_promise(async move {
        let bytes = engine()
            .download_file_bytes(&file_id)
            .await
            .map_err(js_error)?;
        Ok(Uint8Array::from(bytes.as_slice()).into())
    })
}

#[wasm_bindgen(js_name = deleteFile)]
pub fn delete_file(file_id: String) -> Promise {
    ok_promise(async move {
        engine().delete_file(&file_id).await.map_err(js_error)?;
        Ok(JsValue::UNDEFINED)
    })
}

fn engine() -> Arc<SyncEngine> {
    Arc::clone(&ENGINE)
}

async fn ensure_requested_base_url(
    engine: &Arc<SyncEngine>,
    requested: &str,
) -> Result<(), JsValue> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Ok(());
    }
    let configured = engine.base_url();
    if normalize_server_url(requested) == normalize_server_url(&configured) {
        return Ok(());
    }
    Err(js_error(format!(
        "Server URL is fixed at client init: configured {configured}, requested {requested}"
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
