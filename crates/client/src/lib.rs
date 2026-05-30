pub mod api_client;
pub mod engine;
mod local_store;

#[cfg(target_os = "macos")]
#[path = "clipboard_watcher_macos.rs"]
pub mod clipboard_watcher;

#[cfg(target_os = "linux")]
#[path = "clipboard_watcher_linux.rs"]
pub mod clipboard_watcher;
