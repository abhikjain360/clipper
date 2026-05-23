//! Compatibility re-export for app-visible decrypted state.
//!
//! `clipper-app-types` owns these state types. This crate re-exports them so
//! existing daemon IPC users can keep importing from `clipper-daemon-types`.

pub use clipper_app_types::*;
