//! macOS clipboard watcher.
//!
//! Polls NSPasteboard.generalPasteboard every 500ms for changes.
//! Uses the changeCount property to detect new clipboard content.

use std::{sync::Arc, time::Duration};

use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;
use tracing::{debug, warn};

use crate::engine::SyncEngine;

/// Start watching the macOS clipboard in a background task.
/// Sends new clipboard text to the server via the SyncEngine.
pub fn start_clipboard_watcher(engine: Arc<SyncEngine>) {
    let rt = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let mut last_change_count: isize = -1;

        loop {
            std::thread::sleep(Duration::from_millis(500));

            // Check if still logged in
            let logged_in = rt.block_on(async {
                let state = engine.get_state().await;
                state.logged_in
            });
            if !logged_in {
                // Stop watching when logged out
                debug!("Clipboard watcher stopping: not logged in");
                return;
            }

            // Read clipboard
            match read_clipboard(&mut last_change_count) {
                Some(text) if !text.is_empty() => {
                    let engine = engine.clone();
                    rt.block_on(async {
                        if let Err(e) = engine.send_clipboard(&text).await {
                            warn!("Clipboard upload failed: {}", e);
                        }
                    });
                }
                _ => {}
            }
        }
    });
}

/// Read the clipboard text if it has changed since last check.
/// Returns Some(text) if clipboard changed, None if no change.
fn read_clipboard(last_change_count: &mut isize) -> Option<String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let current_count = pasteboard.changeCount();

    if current_count == *last_change_count {
        return None;
    }
    *last_change_count = current_count;

    let ns_string_type = NSString::from_str("public.utf8-plain-text");
    let content = pasteboard.stringForType(&ns_string_type);

    content.map(|s| s.to_string())
}
