//! Shared types for the clipper daemon IPC protocol.
//!
//! Contains state types (AppState, etc.) and protocol types
//! shared between clipper-daemon and the app bridge.

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod ipc_path;
mod protocol;
mod state;

pub use protocol::*;
pub use state::*;

pub use clipper_api_types::{ApiErrorCode, ErrorResponse};
