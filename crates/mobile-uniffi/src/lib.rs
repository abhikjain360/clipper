use std::{path::PathBuf, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clipper_app_types::{AlarmView, AppState, ClipboardPayload, CollabItem, DeviceInfo};
use clipper_client::{
    api_client::ClientError,
    engine::{SyncEngine, TEXT_CLIPBOARD_MIME_TYPE},
};
use zeroize::Zeroizing;

uniffi::setup_scaffolding!();
clipper_app_types::uniffi_reexport_scaffolding!();

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8787";

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum MobileError {
    #[error("{0}")]
    Client(String),
    #[error("server URL is fixed at client init: configured {configured}, requested {requested}")]
    ServerUrlMismatch {
        configured: String,
        requested: String,
    },
    #[error("mobile data directory is unavailable")]
    DataDirUnavailable,
    #[error("invalid session resume key")]
    InvalidResumeKey,
    #[error("saved session is no longer valid; sign in again")]
    SessionResumeRejected,
}

/// Revocable session material for biometric-gated storage. Never contains the
/// passphrase, and deliberately has no Debug implementation.
#[derive(uniffi::Record)]
pub struct MobileSessionResumeMaterial {
    pub token: String,
    pub data_key: String,
    pub wrapping_key: String,
}

fn decode_resume_key(value: &str) -> Result<Zeroizing<[u8; 32]>, MobileError> {
    let bytes = Zeroizing::new(
        STANDARD
            .decode(value)
            .map_err(|_| MobileError::InvalidResumeKey)?,
    );
    let key = bytes
        .as_slice()
        .try_into()
        .map_err(|_| MobileError::InvalidResumeKey)?;
    Ok(Zeroizing::new(key))
}

impl From<ClientError> for MobileError {
    fn from(error: ClientError) -> Self {
        Self::Client(error.to_string())
    }
}

#[derive(uniffi::Object)]
pub struct MobileClipperClient {
    engine: Arc<SyncEngine>,
    default_device_name: String,
    platform: String,
}

// The exported methods are `async` and mapped to JS Promises by
// uniffi-bindgen-react-native, so no networked call ever blocks the React
// Native JS thread. `async_runtime = "tokio"` drives each future on uniffi's
// process-global Tokio runtime (via `async-compat`), which also hosts the
// engine's detached background tasks (WebSocket loop, reconciliation) spawned
// with `tokio::spawn` during `login`/`register`.
#[uniffi::export(async_runtime = "tokio")]
impl MobileClipperClient {
    #[uniffi::constructor]
    pub fn new(
        base_url: String,
        data_dir: String,
        default_device_name: String,
        platform: String,
    ) -> Result<Arc<Self>, MobileError> {
        let base_url = non_empty_or_default(&base_url, DEFAULT_BASE_URL).to_string();
        let data_dir = resolve_data_dir(non_empty_or_default(&data_dir, "clipper-mobile"))?;
        let default_device_name =
            non_empty_or_default(&default_device_name, "Mobile-Clipper").to_string();
        let platform = non_empty_or_default(&platform, "mobile").to_string();

        Ok(Arc::new(Self {
            engine: SyncEngine::try_new_with_data_dir(&base_url, data_dir)?,
            default_device_name,
            platform,
        }))
    }

    #[uniffi::constructor]
    pub fn new_with_default_server() -> Result<Arc<Self>, MobileError> {
        Self::new(
            DEFAULT_BASE_URL.to_string(),
            "clipper-mobile".to_string(),
            "Android-Clipper".to_string(),
            "android".to_string(),
        )
    }

    pub fn connect(&self) {}

    pub fn default_server_url(&self) -> String {
        DEFAULT_BASE_URL.to_string()
    }

    pub async fn login(
        &self,
        passphrase: String,
        username: String,
        device_name: String,
        server_url: String,
    ) -> Result<(), MobileError> {
        let passphrase = Zeroizing::new(passphrase);
        self.ensure_requested_base_url(&server_url)?;
        let device_name = self.device_name(device_name);
        self.engine
            .login_with_platform(&passphrase, &username, &device_name, &self.platform)
            .await?;
        Ok(())
    }

    pub async fn register(
        &self,
        access_key: String,
        username: String,
        passphrase: String,
        device_name: String,
        server_url: String,
    ) -> Result<String, MobileError> {
        let access_key = Zeroizing::new(access_key);
        let passphrase = Zeroizing::new(passphrase);
        self.ensure_requested_base_url(&server_url)?;
        let device_name = self.device_name(device_name);
        Ok(self
            .engine
            .register_with_platform(
                &access_key,
                &username,
                &passphrase,
                &device_name,
                &self.platform,
            )
            .await?)
    }

    pub async fn logout(&self) -> Result<(), MobileError> {
        self.engine.logout().await?;
        Ok(())
    }

    pub async fn session_resume_material(&self) -> Option<MobileSessionResumeMaterial> {
        self.engine
            .session_resume_material()
            .await
            .map(|material| MobileSessionResumeMaterial {
                token: material.token,
                data_key: STANDARD.encode(material.data_key.as_slice()),
                wrapping_key: STANDARD.encode(material.device_identity_wrapping_key.as_slice()),
            })
    }

    pub async fn resume(
        &self,
        token: String,
        data_key: String,
        wrapping_key: String,
        username: String,
        device_name: String,
        server_url: String,
    ) -> Result<(), MobileError> {
        self.ensure_requested_base_url(&server_url)?;
        let data_key = Zeroizing::new(data_key);
        let wrapping_key = Zeroizing::new(wrapping_key);
        self.engine
            .resume_with_platform(
                token,
                decode_resume_key(&data_key)?,
                decode_resume_key(&wrapping_key)?,
                &username,
                &self.device_name(device_name),
            )
            .await
            .map_err(|error| match error {
                ClientError::Api {
                    status: 401 | 403, ..
                }
                | ClientError::NotAuthenticated
                | ClientError::NoResumableDeviceIdentity => MobileError::SessionResumeRejected,
                other => MobileError::from(other),
            })?;
        Ok(())
    }

    pub async fn get_state(&self) -> AppState {
        self.engine.get_state().await
    }

    pub fn state_version(&self) -> f64 {
        self.engine.state_version() as f64
    }

    /// Suspend until the engine's state version advances past `seen_version`,
    /// then return the new version. Backed by the engine's `watch` channel, so
    /// the JS side awaits a single Promise instead of busy-polling
    /// `state_version` — the FFI call yields the JS thread for the whole wait.
    pub async fn wait_for_state_change(&self, seen_version: f64) -> Result<f64, MobileError> {
        let seen_version = js_number_to_version(seen_version);
        let version = self
            .engine
            .wait_for_state_change_after(seen_version)
            .await?;
        Ok(version as f64)
    }

    /// Alarms due within `within_hours`, soonest first.
    ///
    /// The Android layer registers each as a one-shot exact alarm and mirrors
    /// the list to device-protected storage so a reboot can re-register them
    /// before the user unlocks — at which point nothing encrypted is readable.
    pub async fn next_alarms(
        &self,
        within_hours: u32,
        observer_zone: String,
    ) -> Result<Vec<AlarmView>, MobileError> {
        Ok(self
            .engine
            .next_alarms(within_hours, &observer_zone)
            .await?)
    }

    pub async fn refresh(&self) -> Result<(), MobileError> {
        self.engine.refresh().await?;
        Ok(())
    }

    pub async fn send_clipboard_text(&self, text: String) -> Result<String, MobileError> {
        Ok(self
            .engine
            .send_clipboard_payload(TEXT_CLIPBOARD_MIME_TYPE, text.as_bytes())
            .await?)
    }

    pub async fn send_clipboard_payload(
        &self,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> Result<String, MobileError> {
        Ok(self
            .engine
            .send_clipboard_payload(&mime_type, &bytes)
            .await?)
    }

    pub async fn clipboard_payload(&self, id: String) -> Result<ClipboardPayload, MobileError> {
        Ok(self.engine.clipboard_payload(&id).await?)
    }

    pub async fn upload_file_bytes(
        &self,
        filename: String,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> Result<String, MobileError> {
        Ok(self
            .engine
            .upload_file_bytes(&filename, Some(&mime_type), &bytes)
            .await?)
    }

    pub async fn download_file_bytes(&self, file_id: String) -> Result<Vec<u8>, MobileError> {
        Ok(self.engine.download_file_bytes(&file_id).await?)
    }

    pub async fn delete_file(&self, file_id: String) -> Result<(), MobileError> {
        self.engine.delete_file(&file_id).await?;
        Ok(())
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceInfo>, MobileError> {
        Ok(self.engine.list_devices().await?)
    }

    pub async fn remove_device(&self, device_id: String) -> Result<(), MobileError> {
        self.engine.remove_device(&device_id).await?;
        Ok(())
    }

    pub async fn create_collab_doc(&self) -> Result<CollabItem, MobileError> {
        Ok(self.engine.create_collab_doc().await?)
    }

    pub async fn delete_collab_doc(&self, object_id: String) -> Result<(), MobileError> {
        self.engine.delete_collab_doc(&object_id).await?;
        Ok(())
    }

    pub async fn rename_collab_doc(
        &self,
        object_id: String,
        title: String,
    ) -> Result<CollabItem, MobileError> {
        Ok(self.engine.rename_collab_doc(&object_id, &title).await?)
    }

    pub async fn get_collab_doc_meta(&self, object_id: String) -> Result<CollabItem, MobileError> {
        Ok(self.engine.get_collab_doc_meta(&object_id).await?)
    }
}

impl MobileClipperClient {
    fn ensure_requested_base_url(&self, requested: &str) -> Result<(), MobileError> {
        let requested = requested.trim();
        if requested.is_empty() {
            return Ok(());
        }
        let configured = self.engine.base_url();
        if normalize_server_url(requested) == normalize_server_url(&configured) {
            return Ok(());
        }
        Err(MobileError::ServerUrlMismatch {
            configured,
            requested: requested.to_string(),
        })
    }

    fn device_name(&self, requested: String) -> String {
        non_empty_or_default(&requested, &self.default_device_name).to_string()
    }
}

fn non_empty_or_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.trim().is_empty() {
        default
    } else {
        value
    }
}

fn resolve_data_dir(value: &str) -> Result<PathBuf, MobileError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Ok(path);
    }

    let base = dirs::data_dir().ok_or(MobileError::DataDirUnavailable)?;
    Ok(base.join(path))
}

fn normalize_server_url(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

/// Clamp a JS `number` carrying a state version back into the engine's `u64`
/// space. JS has no integer type, so versions cross the FFI as `f64`; reject
/// non-finite/negative inputs to `0` so a malformed seen-version cannot skip
/// the wait. Mirrors the browser wasm adapter.
fn js_number_to_version(version: f64) -> u64 {
    if version.is_finite() && version > 0.0 {
        version.floor() as u64
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_keys_require_exactly_32_decoded_bytes() {
        assert!(decode_resume_key("not base64").is_err());
        assert!(decode_resume_key(&STANDARD.encode([1_u8; 31])).is_err());
        assert!(decode_resume_key(&STANDARD.encode([1_u8; 33])).is_err());
        assert_eq!(
            *decode_resume_key(&STANDARD.encode([7_u8; 32])).expect("key"),
            [7_u8; 32]
        );
    }

    #[test]
    fn creates_client_with_explicit_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let client = MobileClipperClient::new(
            DEFAULT_BASE_URL.to_string(),
            dir.path().to_string_lossy().into_owned(),
            "test-device".to_string(),
            "android".to_string(),
        )
        .unwrap();

        assert_eq!(client.default_server_url(), DEFAULT_BASE_URL);
        assert_eq!(client.state_version(), 0.0);
    }

    #[test]
    fn empty_values_fall_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(non_empty_or_default("", "fallback"), "fallback");
        assert_eq!(non_empty_or_default("value", "fallback"), "value");

        let client = MobileClipperClient::new(
            String::new(),
            dir.path().to_string_lossy().into_owned(),
            String::new(),
            String::new(),
        )
        .unwrap();

        assert_eq!(client.default_device_name, "Mobile-Clipper");
        assert_eq!(client.platform, "mobile");
    }

    #[test]
    fn relative_data_dir_resolves_under_platform_data_dir() {
        let path = resolve_data_dir("clipper-mobile").unwrap();
        assert!(path.is_absolute());
        assert!(path.ends_with("clipper-mobile"));
    }

    #[tokio::test]
    async fn state_uses_uniffi_record_payload() {
        let dir = tempfile::tempdir().unwrap();
        let client = MobileClipperClient::new(
            DEFAULT_BASE_URL.to_string(),
            dir.path().to_string_lossy().into_owned(),
            "test-device".to_string(),
            "android".to_string(),
        )
        .unwrap();

        let decoded = client.get_state().await;

        assert!(decoded.session.is_none());
        assert_eq!(decoded.connection_status, Default::default());
    }

    #[test]
    fn js_number_to_version_rejects_non_finite_and_negative() {
        assert_eq!(js_number_to_version(0.0), 0);
        assert_eq!(js_number_to_version(-1.0), 0);
        assert_eq!(js_number_to_version(f64::NAN), 0);
        assert_eq!(js_number_to_version(f64::INFINITY), 0);
        assert_eq!(js_number_to_version(42.9), 42);
    }
}
