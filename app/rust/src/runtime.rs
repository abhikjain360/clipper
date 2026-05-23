//! Platform runtime boundary for the Flutter bridge.
//!
//! macOS uses the installed daemon over a Unix socket. Android runs the shared
//! sync engine in-process behind the same FRB-facing API.

#[cfg(target_os = "macos")]
mod imp {
    use clipper_daemon_types::AppState;

    use crate::transport;

    pub(crate) async fn connect() -> anyhow::Result<()> {
        transport::connect().await
    }

    pub(crate) async fn send_command(
        command: clipper_daemon_types::DaemonCommand,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        transport::send_command(command).await
    }

    pub(crate) async fn current_state() -> AppState {
        transport::current_state()
    }

    pub(crate) async fn wait_for_change() {
        transport::wait_for_change().await;
    }
}

#[cfg(target_os = "android")]
mod imp {
    use std::sync::{Arc, LazyLock};

    use clipper_client::engine::SyncEngine;
    use clipper_daemon_types::{AppState, CopyToLocalResult, DaemonCommand, UploadFileResult};

    static ENGINE: LazyLock<Arc<SyncEngine>> =
        LazyLock::new(|| SyncEngine::new("http://10.0.2.2:8787"));

    fn engine() -> Arc<SyncEngine> {
        Arc::clone(&ENGINE)
    }

    pub(crate) async fn connect() -> anyhow::Result<()> {
        Ok(())
    }

    pub(crate) async fn send_command(
        command: DaemonCommand,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let engine = engine();

        match command {
            DaemonCommand::Login(params) => {
                if let Some(server_url) = params.server_url {
                    engine.set_base_url(&server_url).await;
                }
                engine
                    .login_with_platform(
                        &params.passphrase,
                        params.device_name.as_deref().unwrap_or("Android"),
                        "android",
                    )
                    .await?;
                Ok(None)
            }
            DaemonCommand::Logout => {
                engine.logout().await?;
                Ok(None)
            }
            DaemonCommand::SendClipboard(params) => {
                engine.send_clipboard(&params.text).await?;
                Ok(None)
            }
            DaemonCommand::CopyToLocal(params) => {
                let text = engine.copy_to_local(&params.item_id).await?;
                Ok(Some(serde_json::to_value(CopyToLocalResult { text })?))
            }
            DaemonCommand::UploadFile(params) => {
                let file_id = engine.upload_file(&params.file_path).await?;
                Ok(Some(serde_json::to_value(UploadFileResult { file_id })?))
            }
            DaemonCommand::DownloadFile(params) => {
                engine
                    .download_file(&params.file_id, &params.target_path)
                    .await?;
                Ok(None)
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
}

#[cfg(not(any(target_os = "macos", target_os = "android")))]
mod imp {
    use clipper_daemon_types::{AppState, ConnectionStatus};

    pub(crate) async fn connect() -> anyhow::Result<()> {
        anyhow::bail!("Unsupported platform")
    }

    pub(crate) async fn send_command(
        _command: clipper_daemon_types::DaemonCommand,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        anyhow::bail!("Unsupported platform")
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
}

pub(crate) use imp::*;
