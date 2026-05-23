//! FRB API surface — public types and functions for Dart codegen.
//!
//! All transport and daemon lifecycle logic lives in sibling modules;
//! this file is intentionally kept thin so FRB codegen has a clean target.

use clipper_daemon_types as dt;

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

// ── From conversions (daemon-types → bridge types) ──

impl From<dt::AppState> for BridgeAppState {
    fn from(s: dt::AppState) -> Self {
        Self {
            logged_in: s.logged_in,
            device_id: s.device_id,
            device_name: s.device_name,
            connection_status: s.connection_status.into(),
            clipboard_items: s.clipboard_items.into_iter().map(Into::into).collect(),
            files: s.files.into_iter().map(Into::into).collect(),
            error: s.error,
        }
    }
}

impl From<dt::ConnectionStatus> for BridgeConnectionStatus {
    fn from(s: dt::ConnectionStatus) -> Self {
        match s {
            dt::ConnectionStatus::Disconnected => Self::Disconnected,
            dt::ConnectionStatus::Connecting => Self::Connecting,
            dt::ConnectionStatus::Connected => Self::Connected,
            dt::ConnectionStatus::DaemonNotRunning => Self::DaemonNotRunning,
        }
    }
}

impl From<dt::DecryptedClipboardItem> for BridgeClipboardItem {
    fn from(i: dt::DecryptedClipboardItem) -> Self {
        Self {
            id: i.id,
            text: i.text,
            created_at: i.created_at,
            source_device_id: i.source_device_id,
        }
    }
}

impl From<dt::DecryptedFileItem> for BridgeFileItem {
    fn from(f: dt::DecryptedFileItem) -> Self {
        Self {
            id: f.id,
            filename: f.filename,
            mime_type: f.mime_type,
            blob_size: f.blob_size,
            created_at: f.created_at,
            source_device_id: f.source_device_id,
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
    runtime::send_request(
        "login",
        Some(serde_json::json!({
            "passphrase": passphrase,
            "device_name": device_name,
            "server_url": server_url,
        })),
    )
    .await?;
    Ok(())
}

pub async fn logout() -> anyhow::Result<()> {
    runtime::send_request("logout", None).await?;
    Ok(())
}

pub async fn get_state() -> BridgeAppState {
    runtime::current_state().into()
}

pub async fn send_clipboard(text: String) -> anyhow::Result<()> {
    runtime::send_request("send_clipboard", Some(serde_json::json!({ "text": text }))).await?;
    Ok(())
}

pub async fn copy_to_local(id: String) -> anyhow::Result<String> {
    let result =
        runtime::send_request("copy_to_local", Some(serde_json::json!({ "item_id": id }))).await?;
    let text = result
        .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(String::from))
        .ok_or_else(|| anyhow::anyhow!("No text in response"))?;
    Ok(text)
}

pub async fn upload_file(file_path: String) -> anyhow::Result<String> {
    let result = runtime::send_request(
        "upload_file",
        Some(serde_json::json!({ "file_path": file_path })),
    )
    .await?;
    let file_id = result
        .and_then(|v| v.get("file_id").and_then(|f| f.as_str()).map(String::from))
        .ok_or_else(|| anyhow::anyhow!("No file_id in response"))?;
    Ok(file_id)
}

pub async fn download_file(file_id: String, target_path: String) -> anyhow::Result<()> {
    runtime::send_request(
        "download_file",
        Some(serde_json::json!({ "file_id": file_id, "target_path": target_path })),
    )
    .await?;
    Ok(())
}

pub async fn delete_file(file_id: String) -> anyhow::Result<()> {
    runtime::send_request(
        "delete_file",
        Some(serde_json::json!({ "file_id": file_id })),
    )
    .await?;
    Ok(())
}

pub async fn refresh() -> anyhow::Result<()> {
    runtime::send_request("refresh", None).await?;
    Ok(())
}

pub async fn wait_for_state_change() {
    runtime::wait_for_change().await;
}
