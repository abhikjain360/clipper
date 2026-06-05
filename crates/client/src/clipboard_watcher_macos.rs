//! macOS clipboard watcher.
//!
//! Polls NSPasteboard.generalPasteboard every 500ms for changes.
//! Uses the changeCount property to detect new clipboard content.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString};
use objc2_foundation::NSString;
use tracing::{debug, info, warn};

use crate::{clipboard_privacy, engine::SyncEngine};

static WATCHER_STARTED: AtomicBool = AtomicBool::new(false);

struct ClipboardRead {
    mime_type: &'static str,
    bytes: Vec<u8>,
}

/// Start watching the macOS clipboard in a background task.
/// Sends new clipboard text or PNG image data to the server via the SyncEngine.
pub fn start_clipboard_watcher(engine: Arc<SyncEngine>) {
    if WATCHER_STARTED.swap(true, Ordering::AcqRel) {
        debug!("macOS clipboard watcher already running");
        return;
    }

    let rt = tokio::runtime::Handle::current();
    if let Err(error) = std::thread::Builder::new()
        .name("clipper-clipboard-watcher".to_string())
        .spawn(move || {
            info!("Started macOS clipboard watcher");
            run_clipboard_watcher(rt, engine);
        })
    {
        WATCHER_STARTED.store(false, Ordering::Release);
        warn!("Failed to start macOS clipboard watcher: {}", error);
    }
}

fn run_clipboard_watcher(rt: tokio::runtime::Handle, engine: Arc<SyncEngine>) {
    let mut last_change_count: isize = -1;

    loop {
        std::thread::sleep(Duration::from_millis(500));

        // Check if still logged in
        let logged_in = rt.block_on(async {
            let state = engine.get_state().await;
            state.is_logged_in()
        });
        if !logged_in {
            // Stop watching when logged out
            debug!("Clipboard watcher stopping: not logged in");
            WATCHER_STARTED.store(false, Ordering::Release);
            return;
        }

        match read_clipboard(&mut last_change_count) {
            Some(payload) if !payload.bytes.is_empty() => {
                debug!(
                    mime_type = payload.mime_type,
                    bytes = payload.bytes.len(),
                    "Detected macOS clipboard change",
                );
                let engine = engine.clone();
                rt.block_on(async {
                    match engine
                        .send_clipboard_payload(payload.mime_type, &payload.bytes)
                        .await
                    {
                        Ok(id) => info!(clipboard_id = %id, "Uploaded macOS clipboard change"),
                        Err(e) => warn!("Clipboard upload failed: {}", e),
                    }
                });
            }
            _ => {}
        }
    }
}

/// Read the clipboard payload if it has changed since last check.
/// Returns Some(payload) if clipboard changed, None if no change.
fn read_clipboard(last_change_count: &mut isize) -> Option<ClipboardRead> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let current_count = pasteboard.changeCount();

    if current_count == *last_change_count {
        return None;
    }
    *last_change_count = current_count;

    if pasteboard_has_private_marker(&pasteboard) {
        debug!("Ignoring macOS clipboard payload with private pasteboard marker");
        return None;
    }

    let png_type = unsafe { NSPasteboardTypePNG };
    if let Some(data) = pasteboard.dataForType(png_type) {
        return Some(ClipboardRead {
            mime_type: "image/png",
            bytes: data.to_vec(),
        });
    }

    if let Some(content) = read_pasteboard_text(&pasteboard) {
        return Some(ClipboardRead {
            mime_type: "text/plain",
            bytes: content.into_bytes(),
        });
    }

    None
}

pub fn read_current_unconcealed_clipboard_text() -> Option<String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    if pasteboard_has_private_marker(&pasteboard) {
        debug!("Ignoring macOS clipboard text with private pasteboard marker");
        return None;
    }

    read_pasteboard_text(&pasteboard)
}

fn read_pasteboard_text(pasteboard: &NSPasteboard) -> Option<String> {
    let string_type = unsafe { NSPasteboardTypeString };
    if let Some(content) = pasteboard.stringForType(string_type) {
        return Some(content.to_string());
    }

    let utf8_plain_text_type = NSString::from_str("public.utf8-plain-text");
    pasteboard
        .stringForType(&utf8_plain_text_type)
        .map(|content| content.to_string())
}

fn pasteboard_has_private_marker(pasteboard: &NSPasteboard) -> bool {
    pasteboard.types().is_some_and(|types| {
        types.iter().any(|pasteboard_type| {
            clipboard_privacy::is_macos_private_pasteboard_type(&pasteboard_type.to_string())
        })
    })
}
