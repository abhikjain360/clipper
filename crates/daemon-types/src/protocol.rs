//! IPC protocol types for daemon <-> app communication.
//!
//! Newline-delimited JSON over Unix socket.
//!
//! This crate is the single source of truth for the daemon IPC wire format.
//! The Flutter bridge and the macOS daemon both use these types; Android uses
//! the same command enum in-process so it cannot drift from the daemon command
//! surface.

use clipper_app_types::AppState;
use serde::{Deserialize, Serialize};

pub const IPC_AUTH_VERSION: u32 = 1;
pub const IPC_AUTH_NONCE_BYTES: usize = 32;
pub const IPC_AUTH_TAG_BYTES: usize = 32;

const IPC_AUTH_CONTEXT: &[u8] = b"clipper-ipc-auth-v1";

pub fn ipc_auth_message(daemon_nonce: &[u8], client_nonce: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(
        IPC_AUTH_CONTEXT.len()
            + std::mem::size_of::<u32>() * 3
            + daemon_nonce.len()
            + client_nonce.len(),
    );
    message.extend_from_slice(IPC_AUTH_CONTEXT);
    message.extend_from_slice(&IPC_AUTH_VERSION.to_be_bytes());
    message.extend_from_slice(&(daemon_nonce.len() as u32).to_be_bytes());
    message.extend_from_slice(daemon_nonce);
    message.extend_from_slice(&(client_nonce.len() as u32).to_be_bytes());
    message.extend_from_slice(client_nonce);
    message
}

/// Request from app to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonRequest {
    pub id: String,
    #[serde(flatten)]
    pub command: DaemonCommand,
}

impl DaemonRequest {
    pub fn new(id: String, command: DaemonCommand) -> Self {
        Self { id, command }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "params", rename_all = "snake_case")]
pub enum DaemonCommand {
    Authenticate(AuthenticateParams),
    Login(LoginParams),
    Register(RegisterParams),
    Logout,
    GetState,
    SendClipboard(SendClipboardParams),
    SendClipboardPayload(SendClipboardPayloadParams),
    CopyToLocal(CopyToLocalParams),
    ClipboardPayload(ClipboardPayloadParams),
    UploadFile(UploadFileParams),
    DownloadFile(DownloadFileParams),
    DeleteFile(DeleteFileParams),
    Refresh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticateParams {
    pub protocol_version: u32,
    pub client_nonce: Vec<u8>,
    pub tag: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginParams {
    pub passphrase: String,
    pub user_id: Option<String>,
    pub device_name: Option<String>,
    pub server_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterParams {
    pub access_key: String,
    pub passphrase: String,
    pub device_name: Option<String>,
    pub server_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendClipboardParams {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendClipboardPayloadParams {
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyToLocalParams {
    pub item_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardPayloadParams {
    pub item_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadFileParams {
    pub file_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadFileParams {
    pub file_id: String,
    pub target_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteFileParams {
    pub file_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyToLocalResult {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardPayloadResult {
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadFileResult {
    pub file_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterResult {
    pub user_id: String,
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DaemonLine {
    Response(DaemonResponse),
    Event(DaemonEvent),
}

/// Event pushed from daemon to app.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonEvent {
    pub event: DaemonEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<AppState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_challenge: Option<AuthChallenge>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonEventKind {
    AuthChallenge,
    StateChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthChallenge {
    pub protocol_version: u32,
    pub daemon_nonce: Vec<u8>,
}

impl DaemonEvent {
    pub fn auth_challenge(challenge: AuthChallenge) -> Self {
        Self {
            event: DaemonEventKind::AuthChallenge,
            state: None,
            auth_challenge: Some(challenge),
        }
    }

    pub fn state_changed(state: AppState) -> Self {
        Self {
            event: DaemonEventKind::StateChanged,
            state: Some(state),
            auth_challenge: None,
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
            command: DaemonCommand::Login(LoginParams {
                passphrase: "secret".into(),
                user_id: None,
                device_name: None,
                server_url: None,
            }),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: DaemonRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "abc-123");
        match parsed.command {
            DaemonCommand::Login(params) => assert_eq!(params.passphrase, "secret"),
            _ => panic!("expected login"),
        }
    }

    #[test]
    fn unit_request_roundtrips_without_params() {
        let json = r#"{"id":"1","cmd":"logout"}"#;
        let req: DaemonRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req.command, DaemonCommand::Logout));
    }

    #[test]
    fn register_request_roundtrip() {
        let req = DaemonRequest {
            id: "reg-1".into(),
            command: DaemonCommand::Register(RegisterParams {
                access_key: "invite".into(),
                passphrase: "secret".into(),
                device_name: Some("Phone".into()),
                server_url: Some("http://localhost:8787".into()),
            }),
        };

        let json = serde_json::to_string(&req).unwrap();
        let parsed: DaemonRequest = serde_json::from_str(&json).unwrap();
        match parsed.command {
            DaemonCommand::Register(params) => {
                assert_eq!(params.access_key, "invite");
                assert_eq!(params.device_name.as_deref(), Some("Phone"));
            }
            _ => panic!("expected register"),
        }
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
            user_id: Some("user1".into()),
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
        assert_eq!(parsed.event, DaemonEventKind::StateChanged);
        let s = parsed.state.unwrap();
        assert!(s.logged_in);
        assert_eq!(s.device_id.unwrap(), "dev1");
        assert_eq!(s.connection_status, crate::ConnectionStatus::Connected);
    }

    /// Verify that the shared line union can parse both responses and events
    /// from the same JSON stream.
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

        let parsed: DaemonLine = serde_json::from_str(event_json).unwrap();
        assert!(matches!(parsed, DaemonLine::Event(_)));
    }
}
