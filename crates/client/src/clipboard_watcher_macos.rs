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

use crate::{
    clipboard_privacy,
    engine::{MAX_CLIPBOARD_PAYLOAD_BYTES, SyncEngine},
};

static WATCHER_STARTED: AtomicBool = AtomicBool::new(false);

struct ClipboardRead {
    count: isize,
    mime_type: &'static str,
    bytes: Vec<u8>,
}

pub(crate) fn read_current_payload() -> Option<(isize, &'static str, Vec<u8>)> {
    read_clipboard(&mut -1).map(|payload| (payload.count, payload.mime_type, payload.bytes))
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
            // Stay alive but idle rather than stopping. The watcher thread runs
            // for the lifetime of the process and is login-gated, which removes a
            // start/stop race on WATCHER_STARTED: a login landing between this
            // check and a `store(false)` could otherwise observe the flag still
            // set, skip starting a new watcher, and then the exiting thread would
            // clear it — leaving the user logged in with no watcher. Reset the
            // change counter so a later login re-reads the pasteboard fresh.
            last_change_count = -1;
            continue;
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
                        .capture_macos_clipboard_payload(
                            payload.count,
                            payload.mime_type,
                            &payload.bytes,
                        )
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
    read_pasteboard(&pasteboard, last_change_count)
}

fn read_pasteboard(
    pasteboard: &NSPasteboard,
    last_change_count: &mut isize,
) -> Option<ClipboardRead> {
    let current_count = pasteboard.changeCount();

    if current_count == *last_change_count {
        return None;
    }
    *last_change_count = current_count;

    if pasteboard_has_private_marker(pasteboard) {
        debug!("Ignoring macOS clipboard payload with private pasteboard marker");
        return None;
    }

    let candidate = read_clipboard_candidate(pasteboard)?;

    // Re-validate after reading the payload: the marker check and the payload
    // read are separate, non-atomic ObjC calls, so a concealed write that lands
    // in the gap could otherwise have its content captured even though the
    // pasteboard is now marked private. If the marker is now present, or the
    // pasteboard changed under us, drop this capture and let the next tick
    // re-read the settled pasteboard.
    let final_count = pasteboard.changeCount();
    if final_count != current_count {
        *last_change_count = final_count;
        debug!("Discarding macOS clipboard payload that changed mid-read");
        return None;
    }
    if pasteboard_has_private_marker(pasteboard) {
        debug!("Discarding macOS clipboard payload concealed after read");
        return None;
    }

    Some(candidate)
}

/// Read the first supported payload (PNG image, then text) from the pasteboard,
/// dropping anything over the size ceiling.
fn read_clipboard_candidate(pasteboard: &NSPasteboard) -> Option<ClipboardRead> {
    let png_type = unsafe { NSPasteboardTypePNG };
    if let Some(data) = pasteboard.dataForType(png_type) {
        // Check the length before `to_vec` so an oversized image is dropped
        // without the extra full-size copy (and before the engine encrypts it).
        if data.len() > MAX_CLIPBOARD_PAYLOAD_BYTES {
            warn!(
                limit = MAX_CLIPBOARD_PAYLOAD_BYTES,
                "Ignoring oversized macOS clipboard image"
            );
            return None;
        }
        return Some(ClipboardRead {
            count: pasteboard.changeCount(),
            mime_type: "image/png",
            bytes: data.to_vec(),
        });
    }

    if let Some(content) = read_pasteboard_text(pasteboard) {
        if content.len() > MAX_CLIPBOARD_PAYLOAD_BYTES {
            warn!(
                limit = MAX_CLIPBOARD_PAYLOAD_BYTES,
                "Ignoring oversized macOS clipboard text"
            );
            return None;
        }
        return Some(ClipboardRead {
            count: pasteboard.changeCount(),
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

pub(crate) fn change_count() -> isize {
    NSPasteboard::generalPasteboard().changeCount()
}

#[cfg(not(test))]
pub(crate) fn clear_if_unchanged(count: isize, boot: i64) {
    clear_pasteboard_if_unchanged(&NSPasteboard::generalPasteboard(), count, boot);
}

fn clear_pasteboard_if_unchanged(pasteboard: &NSPasteboard, count: isize, boot: i64) {
    if boot_time() != Some(boot) {
        return;
    }
    if pasteboard.changeCount() == count {
        pasteboard.clearContents();
    }
}

pub(crate) fn boot_time() -> Option<i64> {
    let mut boot = std::mem::MaybeUninit::<libc::timeval>::uninit();
    let mut size = std::mem::size_of::<libc::timeval>();
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            boot.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && size == std::mem::size_of::<libc::timeval>())
        .then(|| unsafe { boot.assume_init() }.tv_sec)
}

#[cfg(not(test))]
pub(crate) fn write_text(text: &str) -> Option<isize> {
    let pasteboard = NSPasteboard::generalPasteboard();
    write_pasteboard_text(&pasteboard, text)
}

fn write_pasteboard_text(pasteboard: &NSPasteboard, text: &str) -> Option<isize> {
    pasteboard.clearContents();
    let written =
        pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
    written.then(|| pasteboard.changeCount())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_purge_clears_owned_text_and_the_watcher_cannot_capture_it_again() {
        let name = NSString::from_str(&format!("clipper-test-{}", uuid::Uuid::now_v7()));
        let pasteboard = NSPasteboard::pasteboardWithName(&name);
        let count = write_pasteboard_text(&pasteboard, "mistake").unwrap();
        let mut seen = -1;
        assert_eq!(
            read_pasteboard(&pasteboard, &mut seen).unwrap().bytes,
            b"mistake"
        );
        clear_pasteboard_if_unchanged(&pasteboard, count, boot_time().unwrap());
        assert!(read_pasteboard_text(&pasteboard).is_none());
        assert!(read_pasteboard(&pasteboard, &mut seen).is_none());
        pasteboard.clearContents();
    }

    #[test]
    fn clipboard_purge_keeps_a_later_copy_even_when_the_text_is_identical() {
        let name = NSString::from_str(&format!("clipper-test-{}", uuid::Uuid::now_v7()));
        let pasteboard = NSPasteboard::pasteboardWithName(&name);
        for later in ["new text", "mistake"] {
            let count = write_pasteboard_text(&pasteboard, "mistake").unwrap();
            write_pasteboard_text(&pasteboard, later).unwrap();
            clear_pasteboard_if_unchanged(&pasteboard, count, boot_time().unwrap());
            assert_eq!(read_pasteboard_text(&pasteboard).as_deref(), Some(later));
        }
        pasteboard.clearContents();
    }

    #[test]
    fn clipboard_ownership_from_an_earlier_boot_cannot_clear_a_new_copy() {
        let name = NSString::from_str(&format!("clipper-test-{}", uuid::Uuid::now_v7()));
        let pasteboard = NSPasteboard::pasteboardWithName(&name);
        let count = write_pasteboard_text(&pasteboard, "new text").unwrap();
        clear_pasteboard_if_unchanged(&pasteboard, count, boot_time().unwrap() - 1);
        assert_eq!(
            read_pasteboard_text(&pasteboard).as_deref(),
            Some("new text")
        );
        pasteboard.clearContents();
    }
}
