//! Wayland clipboard watcher.
//!
//! Uses Wayland data-control through wl-clipboard-rs so the daemon can read the
//! clipboard from the user session without a focused window.

use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use clipper_core::crypto;
use tracing::{debug, warn};
use wl_clipboard_rs::paste::{self, ClipboardType, Error as PasteError, MimeType, Seat};

use crate::engine::SyncEngine;

const POLL_INTERVAL: Duration = Duration::from_millis(500);

static WATCHER_STARTED: AtomicBool = AtomicBool::new(false);

struct ClipboardRead {
    mime_type: String,
    bytes: Vec<u8>,
}

struct SelectedMimeType {
    request_mime_type: String,
    clipper_mime_type: String,
}

#[derive(Debug, thiserror::Error)]
enum ClipboardWatcherError {
    #[error(transparent)]
    Paste(#[from] PasteError),
    #[error("clipboard payload read failed: {0}")]
    Read(#[from] std::io::Error),
}

/// Start watching the Wayland clipboard in a background thread.
///
/// The watcher is process-global. It keeps running while the daemon is alive
/// and only uploads when the sync engine is logged in.
pub fn start_clipboard_watcher(engine: Arc<SyncEngine>) {
    if WATCHER_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }

    let rt = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let mut last_digest = None;
        let mut last_error = None::<String>;

        loop {
            std::thread::sleep(POLL_INTERVAL);

            let logged_in = rt.block_on(async {
                let state = engine.get_state().await;
                state.logged_in
            });
            if !logged_in {
                last_digest = None;
                continue;
            }

            match read_clipboard(&mut last_digest) {
                Ok(Some(payload)) => {
                    last_error = None;
                    let engine = Arc::clone(&engine);
                    rt.block_on(async {
                        if let Err(e) = engine
                            .send_clipboard_payload(&payload.mime_type, &payload.bytes)
                            .await
                        {
                            warn!("Clipboard upload failed: {}", e);
                        }
                    });
                }
                Ok(None) => {
                    last_error = None;
                }
                Err(error) => {
                    let error = error.to_string();
                    if last_error.as_deref() != Some(error.as_str()) {
                        warn!("Wayland clipboard read failed: {}", error);
                        last_error = Some(error);
                    }
                }
            }
        }
    });
}

fn read_clipboard(
    last_digest: &mut Option<[u8; crypto::SHA256_BYTES]>,
) -> Result<Option<ClipboardRead>, ClipboardWatcherError> {
    let mime_types = match paste::get_mime_types_ordered(ClipboardType::Regular, Seat::Unspecified)
    {
        Ok(mime_types) => mime_types,
        Err(error) if is_empty_clipboard_error(&error) => {
            *last_digest = None;
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };

    let Some(selected) = select_mime_type(&mime_types) else {
        *last_digest = None;
        debug!(
            ?mime_types,
            "Ignoring unsupported Wayland clipboard payload"
        );
        return Ok(None);
    };

    let (mut pipe, actual_mime_type) = match paste::get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        MimeType::Specific(&selected.request_mime_type),
    ) {
        Ok(result) => result,
        Err(error) if is_empty_clipboard_error(&error) => {
            *last_digest = None;
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };

    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        *last_digest = None;
        return Ok(None);
    }

    let mime_type =
        normalize_clipper_mime_type(&actual_mime_type).unwrap_or(selected.clipper_mime_type);
    let digest = clipboard_payload_digest(&mime_type, &bytes);
    if last_digest.as_ref().is_some_and(|last| *last == digest) {
        return Ok(None);
    }
    *last_digest = Some(digest);

    Ok(Some(ClipboardRead { mime_type, bytes }))
}

fn select_mime_type(mime_types: &[String]) -> Option<SelectedMimeType> {
    const IMAGE_PRIORITY: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];
    const TEXT_PRIORITY: &[&str] = &[
        "text/plain;charset=utf-8",
        "text/plain",
        "utf8_string",
        "text",
        "string",
    ];

    for wanted in IMAGE_PRIORITY {
        if let Some(mime_type) = find_mime_type(mime_types, wanted) {
            return selected_mime_type(mime_type);
        }
    }

    for wanted in TEXT_PRIORITY {
        if let Some(mime_type) = find_mime_type(mime_types, wanted) {
            return selected_mime_type(mime_type);
        }
    }

    mime_types
        .iter()
        .find(|mime_type| {
            let normalized = normalized_mime_type(mime_type);
            normalized.starts_with("image/") || normalized.starts_with("text/")
        })
        .and_then(|mime_type| selected_mime_type(mime_type))
}

fn find_mime_type<'a>(mime_types: &'a [String], wanted: &str) -> Option<&'a str> {
    mime_types
        .iter()
        .find(|mime_type| normalized_mime_type(mime_type) == wanted)
        .map(String::as_str)
}

fn selected_mime_type(mime_type: &str) -> Option<SelectedMimeType> {
    normalize_clipper_mime_type(mime_type).map(|clipper_mime_type| SelectedMimeType {
        request_mime_type: mime_type.to_string(),
        clipper_mime_type,
    })
}

fn normalize_clipper_mime_type(mime_type: &str) -> Option<String> {
    match normalized_mime_type(mime_type).as_str() {
        "image/png" => Some("image/png".to_string()),
        "image/jpeg" | "image/jpg" => Some("image/jpeg".to_string()),
        "image/gif" => Some("image/gif".to_string()),
        "image/webp" => Some("image/webp".to_string()),
        "text/plain" | "utf8_string" | "text" | "string" => Some("text/plain".to_string()),
        other if other.starts_with("image/") => Some(other.to_string()),
        other if other.starts_with("text/") => Some("text/plain".to_string()),
        _ => None,
    }
}

fn normalized_mime_type(mime_type: &str) -> String {
    mime_type
        .trim()
        .split(';')
        .next()
        .unwrap_or(mime_type)
        .trim()
        .to_ascii_lowercase()
}

fn is_empty_clipboard_error(error: &PasteError) -> bool {
    matches!(
        error,
        PasteError::NoSeats
            | PasteError::ClipboardEmpty
            | PasteError::NoMimeType
            | PasteError::SeatNotFound
    )
}

fn clipboard_payload_digest(mime_type: &str, data: &[u8]) -> [u8; crypto::SHA256_BYTES] {
    let mut bytes = Vec::with_capacity(mime_type.len() + 1 + data.len());
    bytes.extend_from_slice(normalized_mime_type(mime_type).as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(data);
    crypto::sha256(&bytes)
}
