//! App-visible decrypted state.
//!
//! This crate is the single source of truth for state shared between the sync
//! engine, daemon IPC state events, browser wasm bindings, and Tauri commands.
//! It deliberately contains decrypted/display-ready data, not encrypted server
//! API payloads.

use serde::{Deserialize, Serialize};
use strum::{AsRefStr, Display, EnumString};

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

/// A decrypted clipboard item for display.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
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
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DecryptedFileItem {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub blob_size: i64,
    pub created_at: String,
    pub source_device_id: String,
}

/// Connection status visible to the UI.
#[derive(
    Debug, Clone, Default, PartialEq, Serialize, Deserialize, AsRefStr, Display, EnumString,
)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[strum(serialize_all = "PascalCase")]
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
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AppState {
    #[serde(default)]
    pub session: Option<AuthenticatedSession>,
    #[serde(default)]
    pub saved_profile: Option<SavedProfile>,
    pub connection_status: ConnectionStatus,
    pub clipboard_items: Vec<DecryptedClipboardItem>,
    pub files: Vec<DecryptedFileItem>,
    pub error: Option<String>,
}

impl AppState {
    pub fn is_logged_in(&self) -> bool {
        self.session.is_some()
    }

    pub fn device_id(&self) -> Option<&str> {
        self.session
            .as_ref()
            .map(|session| session.device_id.as_str())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AuthenticatedSession {
    pub username: String,
    pub device_id: String,
    pub device_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SavedProfile {
    pub username: String,
    pub device_name: String,
}

/// A decrypted clipboard payload fetched on demand.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ClipboardPayload {
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub text: Option<String>,
}
