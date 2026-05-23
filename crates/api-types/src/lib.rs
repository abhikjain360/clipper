//! Server <-> sync-client API contract.
//!
//! This crate is the single source of truth for the HTTP and WebSocket JSON
//! payloads exchanged by `clipper-server` and `clipper-client`. Keep database
//! entities and UI state out of this crate; convert at the server/client edges.

use serde::{Deserialize, Serialize};

/// Argon2id parameters used in both auth and encryption key derivation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Argon2Params {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for Argon2Params {
    fn default() -> Self {
        Self {
            m_cost: 65536, // 64 MiB
            t_cost: 3,
            p_cost: 1,
        }
    }
}

// -- Auth --

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginChallengeRequest {
    pub credential_request_b64: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginChallengeResponse {
    pub challenge_id: String,
    pub credential_response_b64: String,
    pub server: ServerInfo,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginRequest {
    pub challenge_id: String,
    pub credential_finalization_b64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub device_id: String,
    pub server: ServerInfo,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerInfo {
    pub enc_salt_b64: String,
    pub auth_params: Argon2Params,
    pub enc_params: Argon2Params,
}

// -- Clipboard --

#[derive(Debug, Serialize, Deserialize)]
pub struct ClipboardUploadRequest {
    pub id: String,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
    pub ciphertext_sha256_b64: String,
    pub source_device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_created_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClipboardItem {
    pub id: String,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
    pub created_at: String,
    pub source_device_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClipboardListResponse {
    pub items: Vec<ClipboardItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

// -- Files --

#[derive(Debug, Serialize, Deserialize)]
pub struct FileInitRequest {
    pub id: String,
    pub meta_nonce_b64: String,
    pub meta_ciphertext_b64: String,
    pub blob_nonce_b64: String,
    pub blob_size: i64,
    pub source_device_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileInitResponse {
    pub upload_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileCompleteRequest {
    pub sha256_ciphertext_b64: String,
    pub blob_size: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileListItem {
    pub id: String,
    pub meta_nonce_b64: String,
    pub meta_ciphertext_b64: String,
    pub blob_nonce_b64: String,
    pub blob_size: i64,
    pub created_at: String,
    pub source_device_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileListResponse {
    pub items: Vec<FileListItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

// -- WebSocket --

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsClientMessage {
    #[serde(rename = "hello")]
    Hello { last_seq: i64 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsServerMessage {
    #[serde(rename = "hello_ack")]
    HelloAck {
        server_time: String,
        latest_seq: i64,
    },
    #[serde(rename = "event")]
    Event {
        seq: i64,
        event_type: String,
        object_kind: String,
        object_id: String,
        created_at: String,
    },
    #[serde(rename = "invalidate")]
    Invalidate { target: String },
}

// -- Sync --

#[derive(Debug, Serialize, Deserialize)]
pub struct BootstrapResponse {
    pub device: DeviceInfo,
    pub clipboard_items: Vec<ClipboardItem>,
    pub files: Vec<FileListItem>,
    pub latest_seq: i64,
    pub server: ServerInfo,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
}

// -- Generic --

#[derive(Debug, Serialize, Deserialize)]
pub struct OkResponse {
    pub ok: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

// -- File metadata (encrypted) --

#[derive(Debug, Serialize, Deserialize)]
pub struct FileMeta {
    pub filename: String,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
}
