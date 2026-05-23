//! IPC protocol types for daemon <-> app communication.
//!
//! Newline-delimited JSON over Unix socket.

use serde::{Deserialize, Serialize};

use crate::AppState;

/// Request from app to daemon.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonRequest {
    pub id: String,
    pub cmd: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Response from daemon to app.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DaemonResponse {
    pub fn success(id: String, result: Option<serde_json::Value>) -> Self {
        Self {
            id,
            ok: true,
            result,
            error: None,
        }
    }

    pub fn error(id: String, error: String) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

/// Event pushed from daemon to app.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonEvent {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<AppState>,
}

impl DaemonEvent {
    pub fn state_changed(state: AppState) -> Self {
        Self {
            event: "state_changed".into(),
            state: Some(state),
        }
    }
}
