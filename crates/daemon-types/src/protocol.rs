//! IPC protocol types for daemon <-> app communication.
//!
//! Newline-delimited JSON over Unix socket.
//!
//! This crate is the single source of truth for the daemon IPC wire format.
//! The local daemon and any daemon clients should use these types so request,
//! response, and event payloads cannot drift.

use clipper_api_types::{ApiErrorCode, ErrorResponse};
use clipper_app_types::{AppState, DeviceInfo};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const IPC_AUTH_VERSION: u32 = 1;
pub const IPC_AUTH_NONCE_BYTES: usize = 32;
pub const IPC_AUTH_TAG_BYTES: usize = 32;

const IPC_CLIENT_AUTH_CONTEXT: &[u8] = b"clipper-ipc-client-auth-v1";
const IPC_DAEMON_AUTH_CONTEXT: &[u8] = b"clipper-ipc-daemon-auth-v1";

pub fn ipc_client_auth_message(daemon_nonce: &[u8], client_nonce: &[u8]) -> Vec<u8> {
    ipc_auth_message(IPC_CLIENT_AUTH_CONTEXT, daemon_nonce, client_nonce)
}

pub fn ipc_daemon_auth_message(daemon_nonce: &[u8], client_nonce: &[u8]) -> Vec<u8> {
    ipc_auth_message(IPC_DAEMON_AUTH_CONTEXT, daemon_nonce, client_nonce)
}

fn ipc_auth_message(context: &[u8], daemon_nonce: &[u8], client_nonce: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(
        context.len() + std::mem::size_of::<u32>() * 3 + daemon_nonce.len() + client_nonce.len(),
    );
    message.extend_from_slice(context);
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
    ListDevices,
    RemoveDevice(RemoveDeviceParams),
    Refresh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticateParams {
    pub protocol_version: u32,
    pub client_nonce: Vec<u8>,
    pub tag: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticateResult {
    pub protocol_version: u32,
    pub tag: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginParams {
    pub passphrase: Zeroizing<String>,
    pub username: String,
    pub device_name: Option<String>,
    pub server_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterParams {
    pub access_key: Zeroizing<String>,
    pub username: String,
    pub passphrase: Zeroizing<String>,
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
pub struct RemoveDeviceParams {
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceListResult {
    pub devices: Vec<DeviceInfo>,
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
    pub username: String,
}

/// Response from daemon to app.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DaemonResponse {
    Success {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
    },
    Error {
        id: String,
        error: ErrorResponse,
    },
}

impl DaemonResponse {
    pub fn success(id: String, result: Option<serde_json::Value>) -> Self {
        Self::Success { id, result }
    }

    pub fn error(id: String, error: ErrorResponse) -> Self {
        Self::Error { id, error }
    }

    pub fn error_message(id: String, message: impl Into<String>) -> Self {
        Self::error(id, ErrorResponse::new(ApiErrorCode::Unknown, message))
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
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DaemonEvent {
    AuthChallenge { auth_challenge: AuthChallenge },
    StateChanged { state: AppState },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthChallenge {
    pub protocol_version: u32,
    pub daemon_nonce: Vec<u8>,
}

impl DaemonEvent {
    pub fn auth_challenge(challenge: AuthChallenge) -> Self {
        Self::AuthChallenge {
            auth_challenge: challenge,
        }
    }

    pub fn state_changed(state: AppState) -> Self {
        Self::StateChanged { state }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unit_request_without_params() {
        let json = r#"{"id":"1","cmd":"logout"}"#;
        let req: DaemonRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req.command, DaemonCommand::Logout));
    }

    #[test]
    fn response_success_omits_error() {
        let resp = DaemonResponse::success("id1".into(), Some(serde_json::json!({"text": "hi"})));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("error"));
        assert!(json.contains(r#""status":"success""#));

        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            DaemonResponse::Success { result, .. } => {
                assert_eq!(result.unwrap()["text"], "hi");
            }
            DaemonResponse::Error { .. } => panic!("expected success response"),
        }
    }

    #[test]
    fn response_error_omits_result() {
        let resp = DaemonResponse::error_message("id2".into(), "bad stuff");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("result"));
        assert!(json.contains(r#""status":"error""#));

        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            DaemonResponse::Error { error, .. } => {
                assert_eq!(error.code, ApiErrorCode::Unknown);
                assert_eq!(error.message, "bad stuff");
            }
            DaemonResponse::Success { .. } => panic!("expected error response"),
        }
    }

    #[test]
    fn ipc_auth_messages_are_directional() {
        let daemon_nonce = [1_u8; IPC_AUTH_NONCE_BYTES];
        let client_nonce = [2_u8; IPC_AUTH_NONCE_BYTES];

        assert_ne!(
            ipc_client_auth_message(&daemon_nonce, &client_nonce),
            ipc_daemon_auth_message(&daemon_nonce, &client_nonce),
        );
    }

    /// Verify that the shared line union can parse both responses and events
    /// from the same JSON stream.
    #[test]
    fn bridge_can_distinguish_response_from_event() {
        // A response has "id"
        let resp_json = r#"{"status":"success","id":"x"}"#;
        let v: serde_json::Value = serde_json::from_str(resp_json).unwrap();
        assert!(v.get("id").is_some());
        assert!(v.get("event").is_none());

        // An event has "event" but no "id"
        let event_json = r#"{"event":"state_changed","state":{"connection_status":"Disconnected","clipboard_items":[],"files":[]}}"#;
        let v: serde_json::Value = serde_json::from_str(event_json).unwrap();
        assert!(v.get("id").is_none());
        assert_eq!(v["event"], "state_changed");

        let parsed: DaemonLine = serde_json::from_str(event_json).unwrap();
        assert!(matches!(parsed, DaemonLine::Event(_)));
    }
}
