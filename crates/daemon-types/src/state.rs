//! Client state types shared across the app, daemon, and bridge.

use serde::{Deserialize, Serialize};

/// A decrypted clipboard item for display.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecryptedClipboardItem {
    pub id: String,
    pub text: String,
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

/// Connection status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    /// The daemon process is not running (bridge-only state).
    DaemonNotRunning,
}

impl Default for ConnectionStatus {
    fn default() -> Self {
        Self::Disconnected
    }
}

/// The full UI state exposed to the app.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppState {
    pub logged_in: bool,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub connection_status: ConnectionStatus,
    pub clipboard_items: Vec<DecryptedClipboardItem>,
    pub files: Vec<DecryptedFileItem>,
    pub error: Option<String>,
}
