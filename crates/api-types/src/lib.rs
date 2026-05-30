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

/// Binary request/response body format used by Rust-only object endpoints.
pub const POSTCARD_CONTENT_TYPE: &str = "application/vnd.clipper.postcard";

// Postcard is a positional binary format. Shared request/response structs that
// may cross postcard endpoints must serialize `Option::None` explicitly; omitting
// a field with `skip_serializing_if` leaves the decoder waiting for bytes that
// are not present.

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
    pub credential_request: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginChallengeResponse {
    pub challenge_id: String,
    pub credential_response: Vec<u8>,
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
#[strum(serialize_all = "snake_case")]
pub enum ObjectKind {
    Clipboard,
    File,
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
            | Self::InvalidPayloadSize => 400,
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
    use super::*;

    #[test]
    fn object_error_codes_have_canonical_http_statuses() {
        let cases = [
            (ApiErrorCode::ObjectAlreadyExists, 409),
            (ApiErrorCode::ObjectForbidden, 403),
            (ApiErrorCode::ObjectDeleteUnsupported, 400),
            (ApiErrorCode::ObjectNotReadyToComplete, 409),
            (ApiErrorCode::DuplicateObjectPayloadId, 400),
            (ApiErrorCode::ObjectPayloadNotFound, 404),
            (ApiErrorCode::ObjectPayloadAlreadyUploaded, 409),
            (ApiErrorCode::ObjectPayloadUploadInProgress, 409),
            (ApiErrorCode::ObjectPayloadNotUploaded, 409),
            (ApiErrorCode::MissingObjectPayloads, 400),
            (ApiErrorCode::MissingPayloadCompletion, 400),
            (ApiErrorCode::IncompletePayloadCompletion, 400),
            (ApiErrorCode::ObjectPayloadMetadataMismatch, 400),
            (ApiErrorCode::ObjectPayloadIntegrityMismatch, 400),
            (ApiErrorCode::Stream, 400),
        ];

        for (code, status) in cases {
            assert_eq!(code.http_status(), status, "{code:?}");
        }
    }

    #[test]
    fn auth_requests_round_trip_none_option_fields_with_postcard() {
        let login = LoginRequest {
            challenge_id: "challenge".to_string(),
            credential_finalization: vec![1, 2, 3],
            device_id: None,
            device_name: None,
            platform: None,
        };

        let bytes = postcard::to_allocvec(&login).expect("serialize login request");
        let decoded: LoginRequest = postcard::from_bytes(&bytes).expect("deserialize login");
        assert!(decoded.device_id.is_none());
        assert!(decoded.device_name.is_none());
        assert!(decoded.platform.is_none());

        let register_finish = RegisterFinishRequest {
            registration_id: "registration".to_string(),
            registration_upload: vec![4, 5, 6],
            device_id: None,
            device_name: None,
            platform: None,
        };

        let bytes =
            postcard::to_allocvec(&register_finish).expect("serialize register finish request");
        let decoded: RegisterFinishRequest =
            postcard::from_bytes(&bytes).expect("deserialize register finish");
        assert!(decoded.device_id.is_none());
        assert!(decoded.device_name.is_none());
        assert!(decoded.platform.is_none());
    }

    #[test]
    fn object_list_response_round_trips_none_cursor_with_postcard() {
        let response = ObjectListResponse {
            items: Vec::new(),
            next_before: None,
        };

        let bytes = postcard::to_allocvec(&response).expect("serialize response");
        let decoded: ObjectListResponse =
            postcard::from_bytes(&bytes).expect("deserialize response");

        assert!(decoded.items.is_empty());
        assert!(decoded.next_before.is_none());
    }

    #[test]
    fn object_init_request_round_trips_non_inline_payload_with_postcard() {
        let request = ObjectInitRequest {
            id: ObjectId::from(Uuid::nil()),
            kind: ObjectKind::File,
            meta_nonce: vec![1; XCHACHA20_NONCE_BYTES],
            meta_ciphertext: vec![2, 3],
            payloads: vec![ObjectPayloadInit {
                id: ObjectPayloadId::from(Uuid::nil()),
                nonce: vec![4; XCHACHA20_NONCE_BYTES],
                ciphertext_size: 12,
                sha256_ciphertext: vec![5; SHA256_BYTES],
                inline_ciphertext: None,
            }],
        };

        let bytes = postcard::to_allocvec(&request).expect("serialize request");
        let decoded: ObjectInitRequest = postcard::from_bytes(&bytes).expect("deserialize request");

        assert_eq!(decoded.payloads.len(), 1);
        assert!(decoded.payloads[0].inline_ciphertext.is_none());
    }

    #[test]
    fn encrypted_metadata_round_trips_none_size_with_postcard() {
        let clipboard = ClipboardMeta {
            mime_type: "text/plain".to_string(),
            size: None,
        };

        let bytes = postcard::to_allocvec(&clipboard).expect("serialize clipboard meta");
        let decoded: ClipboardMeta =
            postcard::from_bytes(&bytes).expect("deserialize clipboard meta");
        assert!(decoded.size.is_none());

        let file = FileMeta {
            filename: "example.txt".to_string(),
            mime_type: "text/plain".to_string(),
            size: None,
        };

        let bytes = postcard::to_allocvec(&file).expect("serialize file meta");
        let decoded: FileMeta = postcard::from_bytes(&bytes).expect("deserialize file meta");
        assert!(decoded.size.is_none());
    }
}
