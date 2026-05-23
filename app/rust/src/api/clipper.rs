//! FRB API surface — public types and functions for Dart codegen.
//!
//! All transport and daemon lifecycle logic lives in sibling modules;
//! this file is intentionally kept thin so FRB codegen has a clean target.
//! `clipper-app-types` owns app-visible state. The `Bridge*` structs below are
//! codegen adapters for Dart and use exhaustive destructuring in `From`
//! conversions so source-state changes fail compilation until this boundary is
//! updated.

use clipper_app_types as app_types;
use clipper_daemon_types as daemon;

use crate::runtime;

// ── FRB-facing types (thin wrappers required by codegen) ──

#[derive(Clone, Default)]
pub struct BridgeClipboardItem {
    pub id: String,
    pub text: String,
    pub created_at: String,
    pub source_device_id: String,
}

#[derive(Clone, Default)]
pub struct BridgeFileItem {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub blob_size: i64,
    pub created_at: String,
    pub source_device_id: String,
}

#[derive(Clone, Default)]
pub enum BridgeConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    #[default]
    DaemonNotRunning,
}

#[derive(Clone, Default)]
pub struct BridgeAppState {
    pub logged_in: bool,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub connection_status: BridgeConnectionStatus,
    pub clipboard_items: Vec<BridgeClipboardItem>,
    pub files: Vec<BridgeFileItem>,
    pub error: Option<String>,
}

// ── From conversions (app-types → bridge types) ──

impl From<app_types::AppState> for BridgeAppState {
    fn from(s: app_types::AppState) -> Self {
        let app_types::AppState {
            logged_in,
            device_id,
            device_name,
            connection_status,
            clipboard_items,
            files,
            error,
        } = s;
        Self {
            logged_in,
            device_id,
            device_name,
            connection_status: connection_status.into(),
            clipboard_items: clipboard_items.into_iter().map(Into::into).collect(),
            files: files.into_iter().map(Into::into).collect(),
            error,
        }
    }
}

impl From<app_types::ConnectionStatus> for BridgeConnectionStatus {
    fn from(s: app_types::ConnectionStatus) -> Self {
        match s {
            app_types::ConnectionStatus::Disconnected => Self::Disconnected,
            app_types::ConnectionStatus::Connecting => Self::Connecting,
            app_types::ConnectionStatus::Connected => Self::Connected,
            app_types::ConnectionStatus::DaemonNotRunning => Self::DaemonNotRunning,
        }
    }
}

impl From<app_types::DecryptedClipboardItem> for BridgeClipboardItem {
    fn from(i: app_types::DecryptedClipboardItem) -> Self {
        let app_types::DecryptedClipboardItem {
            id,
            text,
            created_at,
            source_device_id,
        } = i;
        Self {
            id,
            text,
            created_at,
            source_device_id,
        }
    }
}

impl From<app_types::DecryptedFileItem> for BridgeFileItem {
    fn from(f: app_types::DecryptedFileItem) -> Self {
        let app_types::DecryptedFileItem {
            id,
            filename,
            mime_type,
            blob_size,
            created_at,
            source_device_id,
        } = f;
        Self {
            id,
            filename,
            mime_type,
            blob_size,
            created_at,
            source_device_id,
        }
    }
}

// ── FRB entry points ──

#[flutter_rust_bridge::frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

pub async fn connect_to_daemon() -> anyhow::Result<()> {
    runtime::connect().await
}

pub async fn login(
    passphrase: String,
    device_name: String,
    server_url: String,
) -> anyhow::Result<()> {
    runtime::send_command(daemon::DaemonCommand::Login(daemon::LoginParams {
        passphrase,
        device_name: Some(device_name),
        server_url: Some(server_url),
    }))
    .await?;
    Ok(())
}

pub async fn logout() -> anyhow::Result<()> {
    runtime::send_command(daemon::DaemonCommand::Logout).await?;
    Ok(())
}

pub async fn get_state() -> BridgeAppState {
    runtime::current_state().await.into()
}

pub async fn send_clipboard(text: String) -> anyhow::Result<()> {
    runtime::send_command(daemon::DaemonCommand::SendClipboard(
        daemon::SendClipboardParams { text },
    ))
    .await?;
    Ok(())
}

pub async fn copy_to_local(id: String) -> anyhow::Result<String> {
    let result = runtime::send_command(daemon::DaemonCommand::CopyToLocal(
        daemon::CopyToLocalParams { item_id: id },
    ))
    .await?;
    Ok(serde_json::from_value::<daemon::CopyToLocalResult>(
        result.ok_or_else(|| anyhow::anyhow!("No text in response"))?,
    )?
    .text)
}

pub async fn upload_file(file_path: String) -> anyhow::Result<String> {
    let result = runtime::send_command(daemon::DaemonCommand::UploadFile(
        daemon::UploadFileParams { file_path },
    ))
    .await?;
    Ok(serde_json::from_value::<daemon::UploadFileResult>(
        result.ok_or_else(|| anyhow::anyhow!("No file_id in response"))?,
    )?
    .file_id)
}

pub async fn download_file(file_id: String, target_path: String) -> anyhow::Result<()> {
    runtime::send_command(daemon::DaemonCommand::DownloadFile(
        daemon::DownloadFileParams {
            file_id,
            target_path,
        },
    ))
    .await?;
    Ok(())
}

pub async fn delete_file(file_id: String) -> anyhow::Result<()> {
    runtime::send_command(daemon::DaemonCommand::DeleteFile(
        daemon::DeleteFileParams { file_id },
    ))
    .await?;
    Ok(())
}

pub async fn refresh() -> anyhow::Result<()> {
    runtime::send_command(daemon::DaemonCommand::Refresh).await?;
    Ok(())
}

pub async fn wait_for_state_change() {
    runtime::wait_for_change().await;
}
