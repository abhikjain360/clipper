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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = DaemonRequest {
            id: "abc-123".into(),
            cmd: "login".into(),
            params: serde_json::json!({"passphrase": "secret"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: DaemonRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "abc-123");
        assert_eq!(parsed.cmd, "login");
        assert_eq!(parsed.params["passphrase"], "secret");
    }

    #[test]
    fn request_missing_params_defaults_to_null() {
        let json = r#"{"id":"1","cmd":"logout"}"#;
        let req: DaemonRequest = serde_json::from_str(json).unwrap();
        assert!(req.params.is_null());
    }

    #[test]
    fn response_success_omits_error() {
        let resp = DaemonResponse::success("id1".into(), Some(serde_json::json!({"text": "hi"})));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("error"));
        assert!(json.contains(r#""ok":true"#));

        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.ok);
        assert!(parsed.error.is_none());
        assert_eq!(parsed.result.unwrap()["text"], "hi");
    }

    #[test]
    fn response_error_omits_result() {
        let resp = DaemonResponse::error("id2".into(), "bad stuff".into());
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("result"));
        assert!(json.contains(r#""ok":false"#));

        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert!(!parsed.ok);
        assert_eq!(parsed.error.unwrap(), "bad stuff");
        assert!(parsed.result.is_none());
    }

    #[test]
    fn event_state_changed_roundtrip() {
        let state = AppState {
            logged_in: true,
            device_id: Some("dev1".into()),
            device_name: Some("Mac".into()),
            connection_status: crate::ConnectionStatus::Connected,
            clipboard_items: vec![],
            files: vec![],
            error: None,
        };
        let event = DaemonEvent::state_changed(state);
        let json = serde_json::to_string(&event).unwrap();
        let parsed: DaemonEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.event, "state_changed");
        let s = parsed.state.unwrap();
        assert!(s.logged_in);
        assert_eq!(s.device_id.unwrap(), "dev1");
        assert_eq!(s.connection_status, crate::ConnectionStatus::Connected);
    }

    /// Verify that the bridge-side DaemonMessage shape (which uses Option fields)
    /// can parse both responses and events from the same JSON stream.
    #[test]
    fn bridge_can_distinguish_response_from_event() {
        // A response has "id"
        let resp_json = r#"{"id":"x","ok":true,"result":null}"#;
        let v: serde_json::Value = serde_json::from_str(resp_json).unwrap();
        assert!(v.get("id").is_some());
        assert!(v.get("event").is_none());

        // An event has "event" but no "id"
        let event_json = r#"{"event":"state_changed","state":{"logged_in":false,"connection_status":"Disconnected","clipboard_items":[],"files":[]}}"#;
        let v: serde_json::Value = serde_json::from_str(event_json).unwrap();
        assert!(v.get("id").is_none());
        assert_eq!(v["event"], "state_changed");
    }
}
