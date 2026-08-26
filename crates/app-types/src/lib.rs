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

/// A collab document for display. Unlike clipboard/file items, a collab doc is
/// server-visible (its Y.Doc content is not end-to-end encrypted), so its
/// metadata arrives as plaintext with nothing to decrypt.
///
/// `title` is empty for a doc that has never been renamed — clients render their
/// own placeholder. `share_url` is the server-built public link, absent when the
/// server has no `public_web_url` configured; clients cannot construct it
/// themselves because the web frontend and the API are separate origins (and the
/// desktop/mobile shells have no web origin at all).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CollabItem {
    pub id: String,
    pub title: String,
    pub share_token: String,
    pub share_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One of the user's registered devices, for the device-management screen.
/// `is_current` marks the device this client is logged in on, so the UI can
/// steer the user to "Log Out" instead of revoking the session they are using.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub created_at: String,
    pub last_seen_at: String,
    pub is_current: bool,
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
    pub collab_docs: Vec<CollabItem>,
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
    /// The server this session is with. Every shell reaches the API through the
    /// engine, so nothing needed this until the collab Y-sync WebSocket — which
    /// the UI layer opens itself and therefore has to address by hand. The
    /// compiled-in default is not a substitute: the user picks a server at login.
    pub server_url: String,
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
