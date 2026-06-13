use std::{path::PathBuf, sync::Arc};

use clipper_app_types::{AppState, ClipboardPayload};
use clipper_client::{
    api_client::ClientError,
    engine::{SyncEngine, TEXT_CLIPBOARD_MIME_TYPE},
};
use tokio::runtime::{Builder, Runtime};
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
    #[error("runtime error: {0}")]
    Runtime(String),
    #[error("mobile data directory is unavailable")]
    DataDirUnavailable,
}

impl From<ClientError> for MobileError {
    fn from(error: ClientError) -> Self {
        Self::Client(error.to_string())
    }
}

#[derive(uniffi::Object)]
pub struct MobileClipperClient {
    engine: Arc<SyncEngine>,
    runtime: Runtime,
    default_device_name: String,
    platform: String,
}

#[uniffi::export]
impl MobileClipperClient {
    #[uniffi::constructor]
    pub fn new(
        base_url: String,
        data_dir: String,
        default_device_name: String,
        platform: String,
    ) -> Result<Arc<Self>, MobileError> {
        let runtime = Builder::new_multi_thread()
            .enable_all()
            .thread_name("clipper-mobile")
            .build()
            .map_err(|error| MobileError::Runtime(error.to_string()))?;
        let base_url = non_empty_or_default(&base_url, DEFAULT_BASE_URL).to_string();
        let data_dir = resolve_data_dir(non_empty_or_default(&data_dir, "clipper-mobile"))?;
        let default_device_name =
            non_empty_or_default(&default_device_name, "Mobile-Clipper").to_string();
        let platform = non_empty_or_default(&platform, "mobile").to_string();

        Ok(Arc::new(Self {
            engine: SyncEngine::try_new_with_data_dir(&base_url, data_dir)?,
            runtime,
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

    pub fn login(
        &self,
        passphrase: String,
        username: String,
        device_name: String,
        server_url: String,
    ) -> Result<(), MobileError> {
        let passphrase = Zeroizing::new(passphrase);
        self.ensure_requested_base_url(&server_url)?;
        let device_name = self.device_name(device_name);
        self.block_on({
            let engine = Arc::clone(&self.engine);
            let platform = self.platform.clone();
            async move {
                engine
                    .login_with_platform(&passphrase, &username, &device_name, &platform)
                    .await
            }
        })
    }

    pub fn register(
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
        self.block_on({
            let engine = Arc::clone(&self.engine);
            let platform = self.platform.clone();
            async move {
                engine
                    .register_with_platform(
                        &access_key,
                        &username,
                        &passphrase,
                        &device_name,
                        &platform,
                    )
                    .await
            }
        })
    }

    pub fn logout(&self) -> Result<(), MobileError> {
        self.block_on({
            let engine = Arc::clone(&self.engine);
            async move { engine.logout().await }
        })
    }

    pub fn get_state(&self) -> AppState {
        self.runtime.block_on(self.engine.get_state())
    }

    pub fn state_version(&self) -> f64 {
        self.engine.state_version() as f64
    }

    pub fn refresh(&self) -> Result<(), MobileError> {
        self.block_on({
            let engine = Arc::clone(&self.engine);
            async move { engine.refresh().await }
        })
    }

    pub fn send_clipboard_text(&self, text: String) -> Result<String, MobileError> {
        self.block_on({
            let engine = Arc::clone(&self.engine);
            async move {
                engine
                    .send_clipboard_payload(TEXT_CLIPBOARD_MIME_TYPE, text.as_bytes())
                    .await
            }
        })
    }

    pub fn send_clipboard_payload(
        &self,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> Result<String, MobileError> {
        self.block_on({
            let engine = Arc::clone(&self.engine);
            async move { engine.send_clipboard_payload(&mime_type, &bytes).await }
        })
    }

    pub fn clipboard_payload(&self, id: String) -> Result<ClipboardPayload, MobileError> {
        self.block_on({
            let engine = Arc::clone(&self.engine);
            async move { engine.clipboard_payload(&id).await }
        })
    }

    pub fn upload_file_bytes(
        &self,
        filename: String,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> Result<String, MobileError> {
        self.block_on({
            let engine = Arc::clone(&self.engine);
            async move {
                engine
                    .upload_file_bytes(&filename, Some(&mime_type), &bytes)
                    .await
            }
        })
    }

    pub fn download_file_bytes(&self, file_id: String) -> Result<Vec<u8>, MobileError> {
        self.block_on({
            let engine = Arc::clone(&self.engine);
            async move { engine.download_file_bytes(&file_id).await }
        })
    }

    pub fn delete_file(&self, file_id: String) -> Result<(), MobileError> {
        self.block_on({
            let engine = Arc::clone(&self.engine);
            async move { engine.delete_file(&file_id).await }
        })
    }
}

impl MobileClipperClient {
    fn block_on<T, F>(&self, future: F) -> Result<T, MobileError>
    where
        F: std::future::Future<Output = Result<T, ClientError>>,
    {
        Ok(self.runtime.block_on(future)?)
    }

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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn state_uses_uniffi_record_payload() {
        let dir = tempfile::tempdir().unwrap();
        let client = MobileClipperClient::new(
            DEFAULT_BASE_URL.to_string(),
            dir.path().to_string_lossy().into_owned(),
            "test-device".to_string(),
            "android".to_string(),
        )
        .unwrap();

        let decoded = client.get_state();

        assert!(decoded.session.is_none());
        assert_eq!(decoded.connection_status, Default::default());
    }
}
