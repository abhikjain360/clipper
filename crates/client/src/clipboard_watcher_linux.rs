//! Wayland clipboard watcher.
//!
//! Uses Wayland data-control through wl-clipboard-rs so the daemon can read the
//! clipboard from the user session without a focused window.

use std::{
    io::Read,
    os::fd::AsRawFd,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use clipper_core::crypto;
use tracing::{debug, warn};
use wl_clipboard_rs::paste::{self, ClipboardType, Error as PasteError, MimeType, Seat};

use crate::{
    clipboard_privacy,
    engine::{MAX_CLIPBOARD_PAYLOAD_BYTES, SyncEngine},
};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Abort a clipboard pipe read that makes no progress for this long. The
/// deadline resets on every chunk, so a large but steadily-streamed payload
/// still completes; only a source that stalls (accepts the receive fd but never
/// writes/closes it) is cut off, so it cannot wedge the watcher thread forever.
const READ_STALL_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Result of a bounded, timeout-guarded clipboard pipe read.
enum ReadOutcome {
    /// The source finished writing within the no-progress deadline. The buffer
    /// holds up to `max_bytes + 1` bytes (one byte past the cap is read so an
    /// over-cap payload is detectable).
    Complete(Vec<u8>),
    /// The source stalled (no bytes for `READ_STALL_TIMEOUT`); the read was
    /// abandoned so it cannot wedge the watcher thread.
    TimedOut,
}

/// Read from a clipboard pipe with both a size cap and a no-progress timeout.
///
/// The write end is held by an untrusted local clipboard owner: it can accept
/// the receive fd and then never write or close it, which would block a plain
/// `read_to_end` forever and silently kill all future clipboard capture. We
/// drive the read over `poll(2)` with a deadline that resets on every chunk of
/// progress, so legitimate large/slow transfers still complete while a stalled
/// source is cut off. The total is bounded to `max_bytes + 1` so a hostile or
/// buggy huge selection cannot balloon daemon memory.
fn read_pipe_bounded<R: Read + AsRawFd>(
    pipe: &mut R,
    max_bytes: usize,
    stall_timeout: Duration,
) -> std::io::Result<ReadOutcome> {
    let raw_fd = pipe.as_raw_fd();
    set_nonblocking(raw_fd)?;

    let limit = max_bytes.saturating_add(1);
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 64 * 1024];
    let mut deadline = Instant::now() + stall_timeout;

    loop {
        if bytes.len() >= limit {
            return Ok(ReadOutcome::Complete(bytes));
        }

        let now = Instant::now();
        if now >= deadline {
            return Ok(ReadOutcome::TimedOut);
        }
        let remaining = deadline - now;
        let timeout_ms = i32::try_from(remaining.as_millis())
            .unwrap_or(i32::MAX)
            .max(1);

        let mut poll_fd = libc::pollfd {
            fd: raw_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `poll_fd` points to one valid `pollfd` for the duration of the
        // call and `raw_fd` is owned by `pipe`, which outlives this call.
        let rc = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if rc == 0 {
            return Ok(ReadOutcome::TimedOut);
        }

        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(ReadOutcome::Complete(bytes)),
            Ok(n) => {
                let take = n.min(limit - bytes.len());
                bytes.extend_from_slice(&chunk[..take]);
                deadline = Instant::now() + stall_timeout;
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::Interrupted =>
            {
                continue;
            }
            Err(err) => return Err(err),
        }
    }
}

fn set_nonblocking(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    // SAFETY: `fcntl` with these commands takes/returns an int and has no
    // memory-safety preconditions; `fd` is a valid open descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
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
                state.is_logged_in()
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

    if clipboard_has_password_manager_marker(&mime_types)? {
        *last_digest = None;
        debug!("Ignoring Wayland clipboard payload with password-manager marker");
        return Ok(None);
    }

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

    // Read with both a size cap and a no-progress timeout: the Wayland source
    // app fully controls this pipe, so it could stream gigabytes (memory) or
    // accept the fd and never write/close it (wedge the watcher thread forever).
    let bytes = match read_pipe_bounded(&mut pipe, MAX_CLIPBOARD_PAYLOAD_BYTES, READ_STALL_TIMEOUT)?
    {
        ReadOutcome::Complete(bytes) => bytes,
        ReadOutcome::TimedOut => {
            *last_digest = None;
            warn!("Wayland clipboard read stalled; skipping payload");
            return Ok(None);
        }
    };
    if bytes.is_empty() {
        *last_digest = None;
        return Ok(None);
    }
    if bytes.len() > MAX_CLIPBOARD_PAYLOAD_BYTES {
        *last_digest = None;
        warn!(
            limit = MAX_CLIPBOARD_PAYLOAD_BYTES,
            "Ignoring oversized Wayland clipboard payload"
        );
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

pub fn current_clipboard_has_password_manager_marker() -> Result<bool, String> {
    let mime_types = match paste::get_mime_types_ordered(ClipboardType::Regular, Seat::Unspecified)
    {
        Ok(mime_types) => mime_types,
        Err(error) if is_empty_clipboard_error(&error) => return Ok(false),
        Err(error) => {
            debug!(
                "Wayland clipboard privacy marker check unavailable: {}",
                error
            );
            return Ok(false);
        }
    };

    clipboard_has_password_manager_marker(&mime_types).map_err(|error| error.to_string())
}

fn clipboard_has_password_manager_marker(
    mime_types: &[String],
) -> Result<bool, ClipboardWatcherError> {
    let Some(hint_mime_type) = clipboard_privacy::linux_password_manager_hint_mime_type(mime_types)
    else {
        return Ok(false);
    };

    let (mut pipe, _) = match paste::get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        MimeType::Specific(hint_mime_type),
    ) {
        Ok(result) => result,
        Err(error) if is_empty_clipboard_error(&error) => return Ok(true),
        Err(error) => return Err(error.into()),
    };

    // Bound this read too: the hint payload is also supplied by the clipboard
    // owner, so a hostile source must not be able to balloon memory or wedge the
    // watcher here (this read happens before MIME selection, so it is the
    // easiest place to stall the thread). A stalled/over-cap hint is treated as
    // "no marker present" so the read cannot block the loop.
    let bytes = match read_pipe_bounded(&mut pipe, MAX_CLIPBOARD_PAYLOAD_BYTES, READ_STALL_TIMEOUT)?
    {
        ReadOutcome::Complete(bytes) => bytes,
        ReadOutcome::TimedOut => {
            warn!("Wayland clipboard privacy-marker read stalled; ignoring");
            return Ok(false);
        }
    };
    Ok(clipboard_privacy::is_linux_password_manager_secret_hint(
        &bytes,
    ))
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
