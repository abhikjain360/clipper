//! Shared types for the clipper daemon IPC protocol.
//!
//! Contains state types (AppState, etc.) and protocol types
//! shared between clipper-daemon and the app bridge.

mod protocol;
mod state;

pub use protocol::*;
pub use state::*;
