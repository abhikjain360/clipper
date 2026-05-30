//! Linux Wayland clipboard writes owned by the daemon process.

use wl_clipboard_rs::copy::{ClipboardType, Error as CopyError, MimeType, Options, Seat, Source};

pub fn set_text(text: &str) -> Result<(), CopyError> {
    let mut options = Options::new();
    options
        .clipboard(ClipboardType::Regular)
        .seat(Seat::All)
        .foreground(false);

    options.copy(
        Source::Bytes(text.as_bytes().to_vec().into_boxed_slice()),
        MimeType::Text,
    )
}
