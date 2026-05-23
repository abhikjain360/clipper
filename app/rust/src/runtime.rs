//! Platform runtime boundary for the Flutter bridge.
//!
//! macOS uses the installed daemon over a Unix socket. Android should not reuse
//! that path; its future runtime can live behind this module without changing
//! the FRB-facing API.

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

    pub(crate) fn current_state() -> AppState {
        transport::current_state()
    }

    pub(crate) async fn wait_for_change() {
        transport::wait_for_change().await;
    }
}

#[cfg(target_os = "android")]
mod imp {
    use clipper_daemon_types::{AppState, ConnectionStatus};

    pub(crate) async fn connect() -> anyhow::Result<()> {
        anyhow::bail!(
            "Android runtime scaffold is present, but the mobile backend is not wired yet"
        )
    }

    pub(crate) async fn send_request(
        _cmd: &str,
        _params: Option<serde_json::Value>,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        anyhow::bail!(
            "Android runtime scaffold is present, but the mobile backend is not wired yet"
        )
    }

    pub(crate) fn current_state() -> AppState {
        AppState {
            connection_status: ConnectionStatus::DaemonNotRunning,
            error: Some("Android runtime is not wired yet".into()),
            ..Default::default()
        }
    }

    pub(crate) async fn wait_for_change() {
        std::future::pending::<()>().await;
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

    pub(crate) fn current_state() -> AppState {
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
