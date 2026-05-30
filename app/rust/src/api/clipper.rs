//! FRB API surface — public types and functions for Dart codegen.
//!
//! All transport and daemon lifecycle logic lives in sibling modules;
//! this file is intentionally kept thin so FRB codegen has a clean target.
//! `clipper-app-types` owns app-visible state. The `Bridge*` structs below are
//! codegen adapters for Dart and use exhaustive destructuring in `From`
//! conversions so source-state changes fail compilation until this boundary is
//! updated.

use std::future::Future;

use clipper_app_types as app_types;
use clipper_daemon_types as daemon;
use strum::{AsRefStr, Display, EnumString};

use crate::{error::AppError, runtime};

// ── FRB-facing types (thin wrappers required by codegen) ──

#[derive(Clone, Default)]
pub struct BridgeClipboardItem {
    pub id: String,
    pub text: String,
    pub mime_type: String,
    pub payload_size: i64,
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

#[derive(Clone, Default, AsRefStr, Display, EnumString)]
#[strum(serialize_all = "PascalCase")]
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
    pub username: Option<String>,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub connection_status: BridgeConnectionStatus,
    pub clipboard_items: Vec<BridgeClipboardItem>,
    pub files: Vec<BridgeFileItem>,
    pub error: Option<String>,
}

#[derive(Clone, Default)]
pub struct BridgeClipboardPayload {
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub text: Option<String>,
}

// ── From conversions (app-types → bridge types) ──

impl From<app_types::AppState> for BridgeAppState {
    fn from(s: app_types::AppState) -> Self {
        let app_types::AppState {
            logged_in,
            username,
            device_id,
            device_name,
            connection_status,
            clipboard_items,
            files,
            error,
        } = s;
        Self {
            logged_in,
            username,
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
            mime_type,
            payload_size,
            created_at,
            source_device_id,
        } = i;
        Self {
            id,
            text,
            mime_type,
            payload_size,
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

async fn bridge_result<T>(future: impl Future<Output = Result<T, AppError>>) -> Result<T, String> {
    future.await.map_err(|e| e.to_string())
}

pub async fn connect_to_daemon() -> Result<(), String> {
    bridge_result(async { Ok(runtime::connect().await?) }).await
}

pub async fn login(
    passphrase: String,
    username: String,
    device_name: String,
    server_url: String,
) -> Result<(), String> {
    bridge_result(async move {
        runtime::send_command(daemon::DaemonCommand::Login(daemon::LoginParams {
            passphrase,
            username,
            device_name: Some(device_name),
            server_url: Some(server_url),
        }))
        .await?;
        Ok(())
    })
    .await
}

pub async fn register(
    access_key: String,
    username: String,
    passphrase: String,
    device_name: String,
    server_url: String,
) -> Result<String, String> {
    bridge_result(async move {
        let result =
            runtime::send_command(daemon::DaemonCommand::Register(daemon::RegisterParams {
                access_key,
                username,
                passphrase,
                device_name: Some(device_name),
                server_url: Some(server_url),
            }))
            .await?;
        Ok(serde_json::from_value::<daemon::RegisterResult>(
            result.ok_or_else(|| AppError::missing_daemon_result("username"))?,
        )?
        .username)
    })
    .await
}

pub async fn logout() -> Result<(), String> {
    bridge_result(async {
        runtime::send_command(daemon::DaemonCommand::Logout).await?;
        Ok(())
    })
    .await
}

pub async fn get_state() -> BridgeAppState {
    runtime::current_state().await.into()
}

pub async fn send_clipboard(text: String) -> Result<(), String> {
    bridge_result(async move {
        runtime::send_command(daemon::DaemonCommand::SendClipboard(
            daemon::SendClipboardParams { text },
        ))
        .await?;
        Ok(())
    })
    .await
}

pub async fn send_clipboard_payload(mime_type: String, bytes: Vec<u8>) -> Result<String, String> {
    bridge_result(async move {
        let result = runtime::send_command(daemon::DaemonCommand::SendClipboardPayload(
            daemon::SendClipboardPayloadParams { mime_type, bytes },
        ))
        .await?;
        result
            .and_then(|value| {
                value
                    .get("id")
                    .and_then(|id| id.as_str())
                    .map(ToOwned::to_owned)
            })
            .ok_or_else(|| AppError::missing_daemon_result("id"))
    })
    .await
}

pub async fn copy_to_local(id: String) -> Result<String, String> {
    bridge_result(async move {
        let result = runtime::send_command(daemon::DaemonCommand::CopyToLocal(
            daemon::CopyToLocalParams { item_id: id },
        ))
        .await?;
        Ok(serde_json::from_value::<daemon::CopyToLocalResult>(
            result.ok_or_else(|| AppError::missing_daemon_result("text"))?,
        )?
        .text)
    })
    .await
}

pub async fn clipboard_payload(id: String) -> Result<BridgeClipboardPayload, String> {
    bridge_result(async move {
        let result = runtime::send_command(daemon::DaemonCommand::ClipboardPayload(
            daemon::ClipboardPayloadParams { item_id: id },
        ))
        .await?;
        let payload = serde_json::from_value::<daemon::ClipboardPayloadResult>(
            result.ok_or_else(|| AppError::missing_daemon_result("clipboard_payload"))?,
        )?;
        Ok(BridgeClipboardPayload {
            mime_type: payload.mime_type,
            bytes: payload.bytes,
            text: payload.text,
        })
    })
    .await
}

pub async fn upload_file(file_path: String) -> Result<String, String> {
    bridge_result(async move {
        let result = runtime::send_command(daemon::DaemonCommand::UploadFile(
            daemon::UploadFileParams { file_path },
        ))
        .await?;
        Ok(serde_json::from_value::<daemon::UploadFileResult>(
            result.ok_or_else(|| AppError::missing_daemon_result("file_id"))?,
        )?
        .file_id)
    })
    .await
}

pub async fn upload_file_bytes(
    filename: String,
    mime_type: String,
    bytes: Vec<u8>,
) -> Result<String, String> {
    bridge_result(
        async move { Ok(runtime::upload_file_bytes(&filename, &mime_type, bytes).await?) },
    )
    .await
}

pub async fn download_file(file_id: String, target_path: String) -> Result<(), String> {
    bridge_result(async move {
        runtime::send_command(daemon::DaemonCommand::DownloadFile(
            daemon::DownloadFileParams {
                file_id,
                target_path,
            },
        ))
        .await?;
        Ok(())
    })
    .await
}

pub async fn download_file_bytes(file_id: String) -> Result<Vec<u8>, String> {
    bridge_result(async move { Ok(runtime::download_file_bytes(&file_id).await?) }).await
}

pub async fn delete_file(file_id: String) -> Result<(), String> {
    bridge_result(async move {
        runtime::send_command(daemon::DaemonCommand::DeleteFile(
            daemon::DeleteFileParams { file_id },
        ))
        .await?;
        Ok(())
    })
    .await
}

pub async fn refresh() -> Result<(), String> {
    bridge_result(async {
        runtime::send_command(daemon::DaemonCommand::Refresh).await?;
        Ok(())
    })
    .await
}

pub async fn wait_for_state_change() {
    runtime::wait_for_change().await;
}
