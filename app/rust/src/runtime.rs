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

    pub(crate) async fn send_request(
        cmd: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        transport::send_request(cmd, params).await
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
    use clipper_daemon_types::AppState;

    static ENGINE: LazyLock<Arc<SyncEngine>> =
        LazyLock::new(|| SyncEngine::new("http://10.0.2.2:8787"));

    fn engine() -> Arc<SyncEngine> {
        Arc::clone(&ENGINE)
    }

    fn required_string(params: &Option<serde_json::Value>, key: &str) -> anyhow::Result<String> {
        params
            .as_ref()
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("Missing string parameter: {}", key))
    }

    pub(crate) async fn connect() -> anyhow::Result<()> {
        Ok(())
    }

    pub(crate) async fn send_request(
        cmd: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let engine = engine();

        match cmd {
            "login" => {
                let passphrase = required_string(&params, "passphrase")?;
                let device_name = required_string(&params, "device_name")?;
                let server_url = required_string(&params, "server_url")?;

                engine.set_base_url(&server_url).await;
                engine
                    .login_with_platform(&passphrase, &device_name, "android")
                    .await?;
                Ok(None)
            }
            "logout" => {
                engine.logout().await?;
                Ok(None)
            }
            "send_clipboard" => {
                let text = required_string(&params, "text")?;
                engine.send_clipboard(&text).await?;
                Ok(None)
            }
            "copy_to_local" => {
                let item_id = required_string(&params, "item_id")?;
                let text = engine.copy_to_local(&item_id).await?;
                Ok(Some(serde_json::json!({ "text": text })))
            }
            "upload_file" => {
                let file_path = required_string(&params, "file_path")?;
                let file_id = engine.upload_file(&file_path).await?;
                Ok(Some(serde_json::json!({ "file_id": file_id })))
            }
            "download_file" => {
                let file_id = required_string(&params, "file_id")?;
                let target_path = required_string(&params, "target_path")?;
                engine.download_file(&file_id, &target_path).await?;
                Ok(None)
            }
            "delete_file" => {
                let file_id = required_string(&params, "file_id")?;
                engine.delete_file(&file_id).await?;
                Ok(None)
            }
            "refresh" => {
                engine.refresh().await?;
                Ok(None)
            }
            _ => anyhow::bail!("Unknown command: {}", cmd),
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

    pub(crate) async fn send_request(
        _cmd: &str,
        _params: Option<serde_json::Value>,
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
