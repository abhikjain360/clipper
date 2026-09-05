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

/// A schedule series, rendered for a list.
///
/// Deliberately pre-formatted strings rather than structured time: this crate
/// stays free of chrono and of the schedule domain crate so its UniFFI records
/// remain primitive, and every shell renders the same text without reimplementing
/// the formatting three times.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ScheduleItemView {
    pub id: String,
    pub title: String,
    /// Human-readable cadence, e.g. "Every weekday" or "Every 2 weeks on Tue".
    pub recurrence: String,
    /// Human-readable time, e.g. "07:00 (floating)" or "09:00 Europe/Berlin".
    pub time_summary: String,
    /// Whether this series is all-day rather than timed.
    pub all_day: bool,
    pub created_at: String,
    /// The exact record, serialized.
    ///
    /// Every other field here is formatted for display and cannot be turned
    /// back into a record, but editing needs the record itself. Carrying it
    /// costs a few hundred bytes and saves a round-trip through five layers of
    /// IPC; it is a string because this crate's types must stay primitive to
    /// cross UniFFI. Consumers parse it with the `ScheduleItem` type in
    /// `packages/shared`, which mirrors it exactly.
    #[serde(default)]
    pub definition_json: String,
}

/// One computed instance of a series, ready to place on a grid.
///
/// Occurrences are never stored — a client expands the window it is showing and
/// throws the result away (`docs/schedule-plan.md`, D4 and D7).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct OccurrenceView {
    /// The series this came from.
    pub item_id: String,
    pub title: String,
    /// Absolute start, RFC 3339 in UTC.
    pub start: String,
    /// Absolute end, RFC 3339 in UTC. Half-open: an occurrence does not include
    /// its end instant.
    pub end: String,
    pub all_day: bool,
    /// True when an override moved this occurrence off its rule position, so
    /// the UI can mark it as changed.
    pub overridden: bool,
    /// Name of the calendar this came from, or `None` for a block the owner
    /// authored. D10 requires an ingested event's origin be visible, since its
    /// core fields are read-only here.
    #[serde(default)]
    pub source: Option<String>,
    /// Cancelled upstream. Shown rather than hidden, because time already
    /// logged against it survives the cancellation.
    #[serde(default)]
    pub cancelled: bool,
}

/// One alarm the platform should register.
///
/// Every field the ring screen needs is here rather than looked up, because on
/// Android this has to work before the device is unlocked — at which point the
/// encrypted store cannot be read at all.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AlarmView {
    /// The series this belongs to.
    pub item_id: String,
    /// Stable per occurrence, so a dismissal lands on the right one.
    pub occurrence_key: String,
    pub label: String,
    /// Epoch milliseconds. The platform alarm APIs take an absolute instant, so
    /// this crosses as a number rather than a formatted string.
    pub fire_at_millis: i64,
    /// When the block itself begins. Differs from `fire_at_millis` whenever the
    /// alarm has a lead time.
    pub occurrence_start_millis: i64,
}

/// A calendar source, rendered for a list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CalendarSourceView {
    /// The object id, which is what deletes and syncs address.
    pub id: String,
    pub name: String,
    /// Protocol label, e.g. "ics".
    pub protocol: String,
    /// The feed URL with its query stripped. A private iCalendar address is a
    /// credential, so the secret part never reaches a UI that might be
    /// screenshotted or logged.
    pub location: String,
    pub enabled: bool,
    /// Events currently held from this source.
    pub event_count: u32,
}

/// What one pass over a calendar feed did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct IngestReport {
    pub added: u32,
    pub updated: u32,
    pub unchanged: u32,
    /// Present locally but gone from the feed, so marked cancelled rather than
    /// erased — time logged against them has to survive.
    pub tombstoned: u32,
    /// Entries in the feed this client could not read. Reported rather than
    /// silently dropped.
    pub skipped: Vec<String>,
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
    /// Series definitions. Occurrences are not here: they depend on which
    /// window the UI is showing, so they come from a separate windowed call.
    #[serde(default)]
    pub schedule_items: Vec<ScheduleItemView>,
    /// Calendar feeds this account pulls from.
    #[serde(default)]
    pub calendar_sources: Vec<CalendarSourceView>,
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
