//! Platform runtime boundary for the Flutter bridge.
//!
//! macOS uses the installed daemon over a Unix socket. Android runs the shared
//! sync engine in-process behind the same FRB-facing API.

pub(crate) type RuntimeResult<T> = Result<T, RuntimeError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeError {
    #[cfg(target_os = "macos")]
    #[error(transparent)]
    Transport(#[from] crate::transport::TransportError),
    #[cfg(any(target_os = "android", target_family = "wasm"))]
    #[error(transparent)]
    Client(#[from] clipper_client::api_client::ClientError),
    #[cfg(any(target_os = "android", target_family = "wasm"))]
    #[error("runtime result encode failed: {0}")]
    ResultEncode(#[from] serde_json::Error),
    #[error("unsupported operation: {0}")]
    UnsupportedOperation(&'static str),
    #[cfg(not(any(target_os = "macos", target_os = "android", target_family = "wasm")))]
    #[error("unsupported platform")]
    UnsupportedPlatform,
}

#[cfg(target_os = "macos")]
mod imp {
    use clipper_daemon_types::AppState;

    use super::{RuntimeError, RuntimeResult};
    use crate::transport;

    pub(crate) async fn connect() -> RuntimeResult<()> {
        Ok(transport::connect().await?)
    }

    pub(crate) async fn send_command(
        command: clipper_daemon_types::DaemonCommand,
    ) -> RuntimeResult<Option<serde_json::Value>> {
        Ok(transport::send_command(command).await?)
    }

    pub(crate) async fn current_state() -> AppState {
        transport::current_state()
    }

    pub(crate) async fn wait_for_change() {
        transport::wait_for_change().await;
    }

    pub(crate) async fn upload_file_bytes(
        _filename: &str,
        _mime_type: &str,
        _bytes: Vec<u8>,
    ) -> RuntimeResult<String> {
        Err(RuntimeError::UnsupportedOperation(
            "byte-based file upload is only available in-process",
        ))
    }

    pub(crate) async fn download_file_bytes(_file_id: &str) -> RuntimeResult<Vec<u8>> {
        Err(RuntimeError::UnsupportedOperation(
            "byte-based file download is only available in-process",
        ))
    }
}

#[cfg(any(target_os = "android", target_family = "wasm"))]
mod imp {
    use std::{
        path::PathBuf,
        sync::{Arc, LazyLock},
    };

    use clipper_client::engine::SyncEngine;
    #[cfg(not(target_family = "wasm"))]
    use clipper_daemon_types::UploadFileResult;
    use clipper_daemon_types::{
        AppState, ClipboardPayloadResult, CopyToLocalResult, DaemonCommand, RegisterResult,
    };

    use super::RuntimeResult;

    static ENGINE: LazyLock<Arc<SyncEngine>> =
        LazyLock::new(|| SyncEngine::new_with_data_dir(default_base_url(), client_data_dir()));

    #[cfg(target_os = "android")]
    fn default_base_url() -> &'static str {
        "http://10.0.2.2:8787"
    }

    #[cfg(target_family = "wasm")]
    fn default_base_url() -> &'static str {
        "http://127.0.0.1:8787"
    }

    #[cfg(target_os = "android")]
    fn default_device_name() -> &'static str {
        "Android"
    }

    #[cfg(target_family = "wasm")]
    fn default_device_name() -> &'static str {
        "Web"
    }

    #[cfg(target_os = "android")]
    fn platform() -> &'static str {
        "android"
    }

    #[cfg(target_family = "wasm")]
    fn platform() -> &'static str {
        "web"
    }

    #[cfg(target_os = "android")]
    fn android_data_dir() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("Clipper")
    }

    #[cfg(target_os = "android")]
    fn client_data_dir() -> PathBuf {
        android_data_dir().join("client")
    }

    #[cfg(target_family = "wasm")]
    fn client_data_dir() -> PathBuf {
        PathBuf::from("web")
    }

    fn engine() -> Arc<SyncEngine> {
        Arc::clone(&ENGINE)
    }

    pub(crate) async fn connect() -> RuntimeResult<()> {
        Ok(())
    }

    pub(crate) async fn send_command(
        command: DaemonCommand,
    ) -> RuntimeResult<Option<serde_json::Value>> {
        let engine = engine();

        match command {
            DaemonCommand::Authenticate(_) => Err(super::RuntimeError::UnsupportedOperation(
                "IPC authentication is only used by the macOS daemon transport",
            )),
            DaemonCommand::Login(params) => {
                if let Some(server_url) = params.server_url {
                    engine.set_base_url(&server_url).await;
                }
                engine
                    .login_with_platform_and_user(
                        &params.passphrase,
                        params.user_id.as_deref(),
                        params
                            .device_name
                            .as_deref()
                            .unwrap_or(default_device_name()),
                        platform(),
                    )
                    .await?;
                Ok(None)
            }
            DaemonCommand::Register(params) => {
                if let Some(server_url) = params.server_url {
                    engine.set_base_url(&server_url).await;
                }
                let user_id = engine
                    .register_with_platform(
                        &params.access_key,
                        &params.passphrase,
                        params
                            .device_name
                            .as_deref()
                            .unwrap_or(default_device_name()),
                        platform(),
                    )
                    .await?;
                Ok(Some(serde_json::to_value(RegisterResult { user_id })?))
            }
            DaemonCommand::Logout => {
                engine.logout().await?;
                Ok(None)
            }
            DaemonCommand::SendClipboard(params) => {
                engine.send_clipboard(&params.text).await?;
                Ok(None)
            }
            DaemonCommand::SendClipboardPayload(params) => {
                let id = engine
                    .send_clipboard_payload(&params.mime_type, &params.bytes)
                    .await?;
                Ok(Some(serde_json::json!({ "id": id })))
            }
            DaemonCommand::CopyToLocal(params) => {
                let text = engine.copy_to_local(&params.item_id).await?;
                Ok(Some(serde_json::to_value(CopyToLocalResult { text })?))
            }
            DaemonCommand::ClipboardPayload(params) => {
                let payload = engine.clipboard_payload(&params.item_id).await?;
                Ok(Some(serde_json::to_value(ClipboardPayloadResult {
                    mime_type: payload.mime_type,
                    bytes: payload.bytes,
                    text: payload.text,
                })?))
            }
            DaemonCommand::UploadFile(params) => {
                #[cfg(target_family = "wasm")]
                {
                    let _ = params;
                    Err(super::RuntimeError::UnsupportedOperation(
                        "path-based file upload",
                    ))
                }

                #[cfg(not(target_family = "wasm"))]
                {
                    let file_id = engine.upload_file(&params.file_path).await?;
                    Ok(Some(serde_json::to_value(UploadFileResult { file_id })?))
                }
            }
            DaemonCommand::DownloadFile(params) => {
                #[cfg(target_family = "wasm")]
                {
                    let _ = params;
                    Err(super::RuntimeError::UnsupportedOperation(
                        "path-based file download",
                    ))
                }

                #[cfg(not(target_family = "wasm"))]
                {
                    engine
                        .download_file(&params.file_id, &params.target_path)
                        .await?;
                    Ok(None)
                }
            }
            DaemonCommand::DeleteFile(params) => {
                engine.delete_file(&params.file_id).await?;
                Ok(None)
            }
            DaemonCommand::Refresh => {
                engine.refresh().await?;
                Ok(None)
            }
            DaemonCommand::GetState => Ok(Some(serde_json::to_value(engine.get_state().await)?)),
        }
    }

    pub(crate) async fn current_state() -> AppState {
        engine().get_state().await
    }

    pub(crate) async fn wait_for_change() {
        let mut rx = engine().subscribe();
        let _ = rx.changed().await;
    }

    pub(crate) async fn upload_file_bytes(
        filename: &str,
        mime_type: &str,
        bytes: Vec<u8>,
    ) -> RuntimeResult<String> {
        Ok(engine()
            .upload_file_bytes(filename, Some(mime_type), &bytes)
            .await?)
    }

    pub(crate) async fn download_file_bytes(file_id: &str) -> RuntimeResult<Vec<u8>> {
        Ok(engine().download_file_bytes(file_id).await?)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "android", target_family = "wasm")))]
mod imp {
    use clipper_daemon_types::{AppState, ConnectionStatus};

    use super::{RuntimeError, RuntimeResult};

    pub(crate) async fn connect() -> RuntimeResult<()> {
        Err(RuntimeError::UnsupportedPlatform)
    }

    pub(crate) async fn send_command(
        _command: clipper_daemon_types::DaemonCommand,
    ) -> RuntimeResult<Option<serde_json::Value>> {
        Err(RuntimeError::UnsupportedPlatform)
    }

    pub(crate) async fn current_state() -> AppState {
        AppState {
            connection_status: ConnectionStatus::DaemonNotRunning,
            error: Some("Unsupported platform".into()),
            ..Default::default()
        }
    }

    pub(crate) async fn wait_for_change() {
        std::future::pending::<()>().await;
    }

    pub(crate) async fn upload_file_bytes(
        _filename: &str,
        _mime_type: &str,
        _bytes: Vec<u8>,
    ) -> RuntimeResult<String> {
        Err(RuntimeError::UnsupportedPlatform)
    }

    pub(crate) async fn download_file_bytes(_file_id: &str) -> RuntimeResult<Vec<u8>> {
        Err(RuntimeError::UnsupportedPlatform)
    }
}

pub(crate) use imp::*;
