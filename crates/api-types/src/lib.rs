//! Server <-> sync-client API contract.
//!
//! This crate is the single source of truth for the HTTP and WebSocket JSON
//! payloads exchanged by `clipper-server` and `clipper-client`. Keep database
//! entities and UI state out of this crate; convert at the server/client edges.

use std::collections::HashSet;

use garde::Validate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const SHA256_BYTES: usize = 32;
const XCHACHA20_NONCE_BYTES: usize = 24;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            pub fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

uuid_id!(UserId);
uuid_id!(DeviceId);
uuid_id!(ObjectId);
uuid_id!(ObjectPayloadId);

mod base64_vec {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    pub fn serialize<S>(value: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&B64.encode(value))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        B64.decode(value).map_err(D::Error::custom)
    }
}

/// Binary request/response body format used by Rust-only object endpoints.
pub const POSTCARD_CONTENT_TYPE: &str = "application/vnd.clipper.postcard";

/// Argon2id parameters used for server-side verifier derivation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Validate)]
pub struct Argon2Params {
    #[garde(range(min = 1))]
    pub m_cost: u32,
    #[garde(range(min = 1))]
    pub t_cost: u32,
    #[garde(range(min = 1))]
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

pub const USERNAME_MIN_LEN: usize = 3;
pub const USERNAME_MAX_LEN: usize = 32;

/// Validate the wire format for a username: lowercase ASCII letters, digits,
/// underscore, or hyphen; `USERNAME_MIN_LEN..=USERNAME_MAX_LEN` chars.
pub fn validate_username(value: &str, _: &()) -> garde::Result {
    if value.len() < USERNAME_MIN_LEN || value.len() > USERNAME_MAX_LEN {
        return Err(garde::Error::new(format!(
            "must be {USERNAME_MIN_LEN}..={USERNAME_MAX_LEN} characters",
        )));
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(garde::Error::new(
            "must contain only lowercase ascii letters, digits, '_' or '-'",
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct LoginChallengeRequest {
    #[garde(custom(validate_username))]
    pub username: String,
    #[garde(length(min = 1))]
    #[serde(rename = "credential_request_b64", with = "base64_vec")]
    pub credential_request: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginChallengeResponse {
    pub challenge_id: String,
    pub credential_response_b64: String,
    pub server: ServerInfo,
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct LoginRequest {
    #[garde(length(min = 1))]
    pub challenge_id: String,
    #[garde(length(min = 1))]
    #[serde(rename = "credential_finalization_b64", with = "base64_vec")]
    pub credential_finalization: Vec<u8>,
    #[garde(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    #[garde(length(min = 1))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[garde(length(min = 1))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub user_id: String,
    pub username: String,
    pub device_id: String,
    pub server: ServerInfo,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ServerInfo {}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct RegisterStartRequest {
    #[garde(length(min = 1))]
    pub access_key: String,
    #[garde(custom(validate_username))]
    pub username: String,
    #[garde(length(min = 1))]
    #[serde(rename = "registration_request_b64", with = "base64_vec")]
    pub registration_request: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterStartResponse {
    pub registration_id: String,
    pub user_id: String,
    pub registration_response_b64: String,
    pub server: ServerInfo,
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct RegisterFinishRequest {
    #[garde(length(min = 1))]
    pub registration_id: String,
    #[garde(length(min = 1))]
    #[serde(rename = "registration_upload_b64", with = "base64_vec")]
    pub registration_upload: Vec<u8>,
    #[garde(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    #[garde(length(min = 1))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[garde(length(min = 1))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterFinishResponse {
    pub token: String,
    pub user_id: String,
    pub username: String,
    pub device_id: String,
    pub server: ServerInfo,
}

// -- Clipboard --

#[derive(Debug, Serialize, Deserialize)]
pub struct ClipboardMeta {
    pub mime_type: String,
    pub size: Option<i64>,
}

// -- Objects --

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectKind {
    Clipboard,
    File,
}

impl ObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clipboard => "clipboard",
            Self::File => "file",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct ObjectPayloadInit {
    #[garde(skip)]
    pub id: ObjectPayloadId,
    #[garde(length(equal = XCHACHA20_NONCE_BYTES))]
    pub nonce: Vec<u8>,
    #[garde(range(min = 0))]
    pub ciphertext_size: i64,
    #[garde(length(equal = SHA256_BYTES))]
    pub sha256_ciphertext: Vec<u8>,
    #[garde(custom(validate_inline_ciphertext(
        self.ciphertext_size,
        &self.sha256_ciphertext
    )))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_ciphertext: Option<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct ObjectInitRequest {
    #[garde(skip)]
    pub id: ObjectId,
    #[garde(skip)]
    pub kind: ObjectKind,
    #[garde(length(equal = XCHACHA20_NONCE_BYTES))]
    pub meta_nonce: Vec<u8>,
    #[garde(skip)]
    pub meta_ciphertext: Vec<u8>,
    #[garde(dive, length(min = 1), custom(validate_unique_init_payload_ids))]
    pub payloads: Vec<ObjectPayloadInit>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ObjectPayloadUpload {
    pub id: ObjectPayloadId,
    pub upload_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ObjectInitResponse {
    pub upload_urls: Vec<ObjectPayloadUpload>,
    pub complete: bool,
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct ObjectPayloadComplete {
    #[garde(skip)]
    pub id: ObjectPayloadId,
    #[garde(range(min = 0))]
    pub ciphertext_size: i64,
    #[garde(length(equal = SHA256_BYTES))]
    pub sha256_ciphertext: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct ObjectCompleteRequest {
    #[garde(dive, length(min = 1), custom(validate_unique_complete_payload_ids))]
    pub payloads: Vec<ObjectPayloadComplete>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ObjectPayloadDescriptor {
    pub id: ObjectPayloadId,
    pub nonce: Vec<u8>,
    pub ciphertext_size: i64,
    pub sha256_ciphertext: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ObjectListItem {
    pub id: ObjectId,
    pub kind: ObjectKind,
    pub meta_nonce: Vec<u8>,
    pub meta_ciphertext: Vec<u8>,
    pub payloads: Vec<ObjectPayloadDescriptor>,
    pub created_at: String,
    pub source_device_id: DeviceId,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ObjectListResponse {
    pub items: Vec<ObjectListItem>,
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
    pub latest_seq: i64,
    pub server: ServerInfo,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: DeviceId,
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

fn validate_inline_ciphertext<'a>(
    ciphertext_size: i64,
    sha256_ciphertext: &'a [u8],
) -> impl FnOnce(&Option<Vec<u8>>, &()) -> garde::Result + 'a {
    move |value, _| {
        let Some(value) = value else {
            return Ok(());
        };
        if ciphertext_size < 0 {
            return Ok(());
        }
        if value.len() as i64 != ciphertext_size {
            return Err(garde::Error::new(
                "length must match declared ciphertext_size",
            ));
        }

        let computed: [u8; SHA256_BYTES] = Sha256::digest(value).into();
        if computed.as_slice() != sha256_ciphertext {
            return Err(garde::Error::new("must match sha256_ciphertext"));
        }

        Ok(())
    }
}

fn validate_unique_init_payload_ids(value: &Vec<ObjectPayloadInit>, _: &()) -> garde::Result {
    let mut seen = HashSet::new();
    for payload in value {
        if !seen.insert(payload.id) {
            return Err(garde::Error::new("must not contain duplicate payload ids"));
        }
    }
    Ok(())
}

fn validate_unique_complete_payload_ids(
    value: &Vec<ObjectPayloadComplete>,
    _: &(),
) -> garde::Result {
    let mut seen = HashSet::new();
    for payload in value {
        if !seen.insert(payload.id) {
            return Err(garde::Error::new("must not contain duplicate payload ids"));
        }
    }
    Ok(())
}
