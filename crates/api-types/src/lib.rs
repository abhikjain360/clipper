//! Server <-> sync-client API contract.
//!
//! This crate is the single source of truth for the HTTP and WebSocket JSON
//! payloads exchanged by `clipper-server` and `clipper-client`. Keep database
//! entities and UI state out of this crate; convert at the server/client edges.

use std::collections::HashSet;

use garde::Validate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use strum::{AsRefStr, Display, EnumString};
use uuid::Uuid;

pub const SHA256_BYTES: usize = 32;
pub const XCHACHA20_NONCE_BYTES: usize = 24;
pub const DEVICE_SIGNING_PUBLIC_KEY_BYTES: usize = 32;
pub const DEVICE_SIGNING_SECRET_KEY_BYTES: usize = 32;
pub const DEVICE_LOGIN_PROOF_CHALLENGE_BYTES: usize = 32;
pub const DEVICE_LOGIN_PROOF_SIGNATURE_BYTES: usize = 64;
pub const DEVICE_LOGIN_PROOF_VERSION: u64 = 1;
pub const OBJECT_ENVELOPE_SIGNATURE_BYTES: usize = 64;

// OWASP's practical Argon2id floor is 19 MiB, 2 iterations, 1 lane. The
// ceilings keep server-side configurable hashing from becoming an OOM footgun.
pub const ARGON2_MIN_M_COST_KIB: u32 = 19 * 1024;
pub const ARGON2_MAX_M_COST_KIB: u32 = 1024 * 1024;
pub const ARGON2_MIN_T_COST: u32 = 2;
pub const ARGON2_MAX_T_COST: u32 = 10;
pub const ARGON2_MIN_P_COST: u32 = 1;
pub const ARGON2_MAX_P_COST: u32 = 16;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
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

/// Binary request/response body format used by Rust-only object endpoints.
pub const POSTCARD_CONTENT_TYPE: &str = "application/vnd.clipper.postcard";

// Postcard is a positional binary format. Shared request/response structs that
// may cross postcard endpoints must serialize `Option::None` explicitly; omitting
// a field with `skip_serializing_if` leaves the decoder waiting for bytes that
// are not present.

/// Argon2id parameters used for server-side verifier derivation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Validate)]
pub struct Argon2Params {
    #[garde(custom(validate_argon2_m_cost))]
    pub m_cost: u32,
    #[garde(custom(validate_argon2_t_cost))]
    pub t_cost: u32,
    #[garde(custom(validate_argon2_p_cost))]
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

fn validate_argon2_m_cost(value: &u32, _: &()) -> garde::Result {
    if *value < ARGON2_MIN_M_COST_KIB {
        return Err(garde::Error::new(format!(
            "must be at least {ARGON2_MIN_M_COST_KIB} KiB",
        )));
    }
    if *value > ARGON2_MAX_M_COST_KIB {
        return Err(garde::Error::new(format!(
            "must be at most {ARGON2_MAX_M_COST_KIB} KiB",
        )));
    }
    Ok(())
}

fn validate_argon2_t_cost(value: &u32, _: &()) -> garde::Result {
    if *value < ARGON2_MIN_T_COST {
        return Err(garde::Error::new(format!(
            "must be at least {ARGON2_MIN_T_COST}",
        )));
    }
    if *value > ARGON2_MAX_T_COST {
        return Err(garde::Error::new(format!(
            "must be at most {ARGON2_MAX_T_COST}",
        )));
    }
    Ok(())
}

fn validate_argon2_p_cost(value: &u32, _: &()) -> garde::Result {
    if *value < ARGON2_MIN_P_COST {
        return Err(garde::Error::new(format!(
            "must be at least {ARGON2_MIN_P_COST}",
        )));
    }
    if *value > ARGON2_MAX_P_COST {
        return Err(garde::Error::new(format!(
            "must be at most {ARGON2_MAX_P_COST}",
        )));
    }
    Ok(())
}

fn validate_optional_device_login_proof_signature(
    value: &Option<Vec<u8>>,
    _: &(),
) -> garde::Result {
    let Some(value) = value else {
        return Ok(());
    };
    if value.len() != DEVICE_LOGIN_PROOF_SIGNATURE_BYTES {
        return Err(garde::Error::new(format!(
            "length must be {DEVICE_LOGIN_PROOF_SIGNATURE_BYTES}"
        )));
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct LoginChallengeRequest {
    #[garde(custom(validate_username))]
    pub username: String,
    #[garde(length(min = 1))]
    pub credential_request: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginChallengeResponse {
    pub challenge_id: String,
    pub credential_response: Vec<u8>,
    pub device_proof_challenge: Vec<u8>,
    pub server: ServerInfo,
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct LoginRequest {
    #[garde(length(min = 1))]
    pub challenge_id: String,
    #[garde(length(min = 1))]
    pub credential_finalization: Vec<u8>,
    #[garde(skip)]
    pub device_id: Option<DeviceId>,
    #[garde(length(equal = DEVICE_SIGNING_PUBLIC_KEY_BYTES))]
    pub device_signing_public_key: Vec<u8>,
    #[garde(custom(validate_optional_device_login_proof_signature))]
    pub device_login_proof_signature: Option<Vec<u8>>,
    #[garde(length(min = 1))]
    pub device_name: Option<String>,
    #[garde(length(min = 1))]
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
    pub registration_request: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterStartResponse {
    pub registration_id: String,
    pub user_id: String,
    pub registration_response: Vec<u8>,
    pub server: ServerInfo,
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct RegisterFinishRequest {
    #[garde(length(min = 1))]
    pub registration_id: String,
    #[garde(length(min = 1))]
    pub registration_upload: Vec<u8>,
    #[garde(skip)]
    pub device_id: Option<DeviceId>,
    #[garde(length(equal = DEVICE_SIGNING_PUBLIC_KEY_BYTES))]
    pub device_signing_public_key: Vec<u8>,
    #[garde(length(min = 1))]
    pub device_name: Option<String>,
    #[garde(length(min = 1))]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceLoginProofBodyV1 {
    pub version: u64,
    pub challenge_id: String,
    pub challenge: Vec<u8>,
    pub username: String,
    pub device_id: DeviceId,
    pub device_signing_public_key: Vec<u8>,
}

// -- Clipboard --

#[derive(Debug, Serialize, Deserialize)]
pub struct ClipboardMeta {
    pub mime_type: String,
    pub size: Option<i64>,
}

// -- Objects --

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, AsRefStr, Display, EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ObjectKind {
    Clipboard,
    File,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, AsRefStr, Display, EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ObjectEventType {
    Created,
    Deleted,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, AsRefStr, Display, EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ObjectEnvelopeOperation {
    Create,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct ObjectEnvelopePayloadV1 {
    #[garde(skip)]
    pub id: ObjectPayloadId,
    #[garde(length(equal = XCHACHA20_NONCE_BYTES))]
    pub nonce: Vec<u8>,
    #[garde(range(min = 0))]
    pub ciphertext_size: i64,
    #[garde(length(equal = SHA256_BYTES))]
    pub sha256_ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct ObjectEnvelopeBodyV1 {
    #[garde(skip)]
    pub object_id: ObjectId,
    #[garde(skip)]
    pub object_type: ObjectKind,
    #[garde(range(min = 1))]
    pub object_version: u64,
    #[garde(skip)]
    pub source_device_id: DeviceId,
    #[garde(length(min = 1))]
    pub created_at: String,
    #[garde(skip)]
    pub operation: ObjectEnvelopeOperation,
    #[garde(length(equal = XCHACHA20_NONCE_BYTES))]
    pub meta_nonce: Vec<u8>,
    #[garde(length(equal = SHA256_BYTES))]
    pub sha256_meta_ciphertext: Vec<u8>,
    #[garde(dive, length(min = 1), custom(validate_unique_envelope_payload_ids))]
    pub payloads: Vec<ObjectEnvelopePayloadV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct ObjectEnvelopeV1 {
    #[garde(dive)]
    pub body: ObjectEnvelopeBodyV1,
    #[garde(length(equal = OBJECT_ENVELOPE_SIGNATURE_BYTES))]
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
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
    pub inline_ciphertext: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
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
    #[garde(dive)]
    pub envelope: ObjectEnvelopeV1,
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
    pub created_seq: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct ObjectPayloadComplete {
    #[garde(skip)]
    pub id: ObjectPayloadId,
    #[garde(range(min = 0))]
    pub ciphertext_size: i64,
    #[garde(length(equal = SHA256_BYTES))]
    pub sha256_ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct ObjectCompleteRequest {
    #[garde(dive, length(min = 1), custom(validate_unique_complete_payload_ids))]
    pub payloads: Vec<ObjectPayloadComplete>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectCompleteResponse {
    pub created_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectDeleteResponse {
    pub deleted_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectPayloadDescriptor {
    pub id: ObjectPayloadId,
    pub nonce: Vec<u8>,
    pub ciphertext_size: i64,
    pub sha256_ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectListItem {
    pub id: ObjectId,
    pub kind: ObjectKind,
    pub created_seq: i64,
    pub meta_nonce: Vec<u8>,
    pub meta_ciphertext: Vec<u8>,
    pub payloads: Vec<ObjectPayloadDescriptor>,
    pub created_at: String,
    pub source_device_id: DeviceId,
    pub source_device_signing_public_key: Vec<u8>,
    pub envelope: ObjectEnvelopeV1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ObjectListCursor {
    pub created_seq: i64,
    pub id: ObjectId,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ObjectListResponse {
    pub items: Vec<ObjectListItem>,
    pub next_after: Option<ObjectListCursor>,
}

// -- WebSocket --

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsClientMessage {
    #[serde(rename = "hello")]
    Hello,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsServerMessage {
    #[serde(rename = "hello_ack")]
    HelloAck {
        server_time: String,
        stream_start_seq: i64,
    },
    #[serde(rename = "event")]
    Event {
        seq: i64,
        event_type: ObjectEventType,
        object_kind: ObjectKind,
        object_id: ObjectId,
        created_at: String,
    },
    #[serde(rename = "invalidate")]
    Invalidate { target: String },
    #[serde(rename = "error")]
    Error { error: WsError },
}

/// A protocol error the server reports to the client immediately before
/// closing the WebSocket. Carries a stable machine-readable `code` on the
/// wire; the human-readable message comes from `Display`, so clients map the
/// code rather than parsing free text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum WsError {
    /// A frame arrived before the required `hello` handshake.
    #[error("expected a hello message as the first frame")]
    ExpectedHello,
    /// The `hello` frame could not be parsed.
    #[error("hello message was malformed")]
    InvalidHello,
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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, AsRefStr, Display, EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ApiErrorCode {
    BadRequest,
    UnsupportedMediaType,
    ValidationFailed,
    InvalidId,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    PayloadTooLarge,
    ServerNotInitialized,
    Database,
    ServerSecret,
    Storage,
    PayloadRead,
    PayloadWrite,
    Stream,
    Unknown,
    InvalidObjectKind,
    ObjectNotFound,
    ObjectAlreadyExists,
    ObjectForbidden,
    ObjectDeleteUnsupported,
    ObjectNotReadyToComplete,
    DuplicateObjectPayloadId,
    ObjectPayloadNotFound,
    ObjectPayloadAlreadyUploaded,
    ObjectPayloadUploadInProgress,
    ObjectPayloadNotUploaded,
    MissingObjectPayloads,
    MissingPayloadCompletion,
    IncompletePayloadCompletion,
    ObjectPayloadMetadataMismatch,
    ObjectPayloadIntegrityMismatch,
    InvalidPayloadSize,
    InvalidObjectEnvelope,
}

impl ApiErrorCode {
    pub fn default_message(self) -> &'static str {
        match self {
            Self::BadRequest => "Bad request",
            Self::UnsupportedMediaType => "Unsupported media type",
            Self::ValidationFailed => "Validation failed",
            Self::InvalidId => "Invalid id",
            Self::Unauthorized => "Unauthorized",
            Self::Forbidden => "Forbidden",
            Self::NotFound => "Not found",
            Self::Conflict => "Conflict",
            Self::RateLimited => "Too many requests",
            Self::PayloadTooLarge => "Payload too large",
            Self::ServerNotInitialized => "Server not initialized",
            Self::Database => "Database error",
            Self::ServerSecret => "Server secret error",
            Self::Storage => "Storage error",
            Self::PayloadRead => "Payload read error",
            Self::PayloadWrite => "Payload write error",
            Self::Stream => "Stream error",
            Self::Unknown => "Unknown error",
            Self::InvalidObjectKind => "Invalid object kind",
            Self::ObjectNotFound => "Object not found",
            Self::ObjectAlreadyExists => "Object already exists",
            Self::ObjectForbidden => "Forbidden",
            Self::ObjectDeleteUnsupported => "Object cannot be deleted this way",
            Self::ObjectNotReadyToComplete => "Object is not ready to complete",
            Self::DuplicateObjectPayloadId => "Duplicate object payload id",
            Self::ObjectPayloadNotFound => "Object payload not found",
            Self::ObjectPayloadAlreadyUploaded => "Object payload already uploaded",
            Self::ObjectPayloadUploadInProgress => "Object payload upload in progress",
            Self::ObjectPayloadNotUploaded => "Object payload has not been uploaded",
            Self::MissingObjectPayloads => "Missing object payloads",
            Self::MissingPayloadCompletion => "Missing payload completion",
            Self::IncompletePayloadCompletion => {
                "Complete request does not cover all object payloads"
            }
            Self::ObjectPayloadMetadataMismatch => {
                "Payload metadata does not match initialized values"
            }
            Self::ObjectPayloadIntegrityMismatch => "Payload size or SHA-256 mismatch",
            Self::InvalidPayloadSize => "Invalid payload size",
            Self::InvalidObjectEnvelope => "Invalid object envelope",
        }
    }

    pub fn from_http_status(status: u16) -> Self {
        match status {
            400 => Self::BadRequest,
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            409 => Self::Conflict,
            413 => Self::PayloadTooLarge,
            415 => Self::UnsupportedMediaType,
            429 => Self::RateLimited,
            500 => Self::Unknown,
            503 => Self::ServerNotInitialized,
            _ => Self::Unknown,
        }
    }

    pub fn http_status(self) -> u16 {
        match self {
            Self::BadRequest
            | Self::ValidationFailed
            | Self::InvalidId
            | Self::InvalidObjectKind
            | Self::InvalidPayloadSize
            | Self::InvalidObjectEnvelope => 400,
            Self::Unauthorized => 401,
            Self::Forbidden | Self::ObjectForbidden => 403,
            Self::NotFound | Self::ObjectNotFound => 404,
            Self::Conflict
            | Self::ObjectAlreadyExists
            | Self::ObjectNotReadyToComplete
            | Self::ObjectPayloadAlreadyUploaded
            | Self::ObjectPayloadUploadInProgress
            | Self::ObjectPayloadNotUploaded => 409,
            Self::RateLimited => 429,
            Self::PayloadTooLarge => 413,
            Self::UnsupportedMediaType => 415,
            Self::ServerNotInitialized => 503,
            Self::ObjectPayloadNotFound => 404,
            Self::ObjectDeleteUnsupported
            | Self::DuplicateObjectPayloadId
            | Self::MissingObjectPayloads
            | Self::MissingPayloadCompletion
            | Self::IncompletePayloadCompletion
            | Self::ObjectPayloadMetadataMismatch
            | Self::ObjectPayloadIntegrityMismatch
            | Self::Stream => 400,
            Self::Database
            | Self::ServerSecret
            | Self::Storage
            | Self::PayloadRead
            | Self::PayloadWrite
            | Self::Unknown => 500,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub code: ApiErrorCode,
    pub message: String,
}

impl ErrorResponse {
    pub fn new(code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn from_code(code: ApiErrorCode) -> Self {
        Self::new(code, code.default_message())
    }
}

impl std::fmt::Display for ErrorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

// -- File metadata (encrypted) --

#[derive(Debug, Serialize, Deserialize)]
pub struct FileMeta {
    pub filename: String,
    pub mime_type: String,
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

fn validate_unique_envelope_payload_ids(
    value: &Vec<ObjectEnvelopePayloadV1>,
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

#[cfg(test)]
mod tests {
    use garde::Validate;

    use super::*;

    #[test]
    fn argon2_params_validate_pragmatic_security_floor() {
        Argon2Params {
            m_cost: ARGON2_MIN_M_COST_KIB,
            t_cost: ARGON2_MIN_T_COST,
            p_cost: ARGON2_MIN_P_COST,
        }
        .validate()
        .expect("minimum accepted Argon2 params should validate");

        assert!(
            Argon2Params {
                m_cost: ARGON2_MIN_M_COST_KIB - 1,
                t_cost: ARGON2_MIN_T_COST,
                p_cost: ARGON2_MIN_P_COST,
            }
            .validate()
            .is_err()
        );
        assert!(
            Argon2Params {
                m_cost: ARGON2_MIN_M_COST_KIB,
                t_cost: ARGON2_MIN_T_COST - 1,
                p_cost: ARGON2_MIN_P_COST,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn argon2_params_validate_resource_ceiling() {
        assert!(
            Argon2Params {
                m_cost: ARGON2_MAX_M_COST_KIB + 1,
                t_cost: ARGON2_MIN_T_COST,
                p_cost: ARGON2_MIN_P_COST,
            }
            .validate()
            .is_err()
        );
        assert!(
            Argon2Params {
                m_cost: ARGON2_MIN_M_COST_KIB,
                t_cost: ARGON2_MAX_T_COST + 1,
                p_cost: ARGON2_MIN_P_COST,
            }
            .validate()
            .is_err()
        );
        assert!(
            Argon2Params {
                m_cost: ARGON2_MIN_M_COST_KIB,
                t_cost: ARGON2_MIN_T_COST,
                p_cost: ARGON2_MAX_P_COST + 1,
            }
            .validate()
            .is_err()
        );
    }
}
