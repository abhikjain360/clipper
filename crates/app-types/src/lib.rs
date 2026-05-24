//! App-visible decrypted state.
//!
//! This crate is the single source of truth for state shared between the sync
//! engine, daemon IPC state events, and the Flutter bridge. It deliberately
//! contains decrypted/display-ready data, not encrypted server API payloads.

use serde::{Deserialize, Serialize};

/// A decrypted clipboard item for display.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecryptedClipboardItem {
    pub id: String,
    pub text: String,
    pub mime_type: String,
    pub payload_size: i64,
    pub created_at: String,
    pub source_device_id: String,
}

/// A decrypted file item for display.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecryptedFileItem {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub blob_size: i64,
    pub created_at: String,
    pub source_device_id: String,
}

/// Connection status visible to the UI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    /// The daemon process is not running (bridge-only state).
    DaemonNotRunning,
}

/// The full UI state exposed to the app.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppState {
    pub logged_in: bool,
    #[serde(default)]
    pub user_id: Option<String>,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub connection_status: ConnectionStatus,
    pub clipboard_items: Vec<DecryptedClipboardItem>,
    pub files: Vec<DecryptedFileItem>,
    pub error: Option<String>,
}
