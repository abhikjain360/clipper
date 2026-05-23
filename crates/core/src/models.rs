//! Compatibility re-export for the server API contract.
//!
//! `clipper-api-types` owns these HTTP/WebSocket payloads. Keep importing via
//! `clipper_core::models` where that is convenient, but make schema changes in
//! `crates/api-types`.

pub use clipper_api_types::*;
