//! HTTP + WebSocket client for the Clipper server.

use clipper_core::{crypto, models::*};
use reqwest::{Client, header};
use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, warn};
use url::Url;
use zeroize::Zeroizing;

const POSTCARD_ERROR_PREVIEW_BYTES: usize = 64;

/// Clipper API client.
#[derive(Clone)]
pub struct ApiClient {
    http: Client,
    base_url: Url,
    token: Option<String>,
}

pub struct AuthResult<T> {
    pub response: T,
    pub encryption_key: Zeroizing<[u8; 32]>,
}

#[derive(Clone, Copy)]
pub struct AuthDevice<'a> {
    pub id: Option<DeviceId>,
    pub name: &'a str,
    pub platform: &'a str,
    pub signing_secret_key: &'a [u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES],
}

impl ApiClient {
    pub fn new(base_url: &str) -> Self {
        Self::try_new(base_url).expect("invalid Clipper server URL")
    }

    pub fn try_new(base_url: &str) -> Result<Self, ClientError> {
        Ok(Self {
            http: Client::new(),
            base_url: parse_server_url(base_url)?,
            token: None,
        })
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    pub fn base_url_display(&self) -> String {
        display_server_url(&self.base_url)
    }

    pub fn set_base_url(&mut self, url: &str) -> Result<(), ClientError> {
        parse_server_url(url).map(|url| self.base_url = url)
    }

    pub fn websocket_url(&self) -> Result<Url, ClientError> {
        let mut url = self.api_url(&["ws"])?;
        let scheme = match url.scheme() {
            "https" => "wss",
            "http" => "ws",
            _ => {
                return Err(ClientError::InvalidServerUrl(
                    "Server URL must use http or https".into(),
                ));
            }
        };
        url.set_scheme(scheme)
            .map_err(|_| ClientError::InvalidServerUrl("Invalid WebSocket URL scheme".into()))?;
        Ok(url)
    }

    fn api_url(&self, segments: &[&str]) -> Result<Url, ClientError> {
        let mut url = self.base_url.clone();
        {
            let mut path = url.path_segments_mut().map_err(|_| {
                ClientError::InvalidServerUrl("Server URL cannot be used as a base URL".into())
            })?;
            path.push("api");
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    fn auth_header(&self) -> Option<String> {
        self.token.as_ref().map(|t| format!("Bearer {}", t))
    }

    async fn checked_response(resp: reqwest::Response) -> Result<reqwest::Response, ClientError> {
        if !resp.status().is_success() {
            return Err(api_error_from_response(resp).await);
        }

        Ok(resp)
    }

    fn postcard_body<T: Serialize>(value: &T) -> Result<Vec<u8>, ClientError> {
        Ok(postcard::to_allocvec(value)?)
    }

    async fn postcard_response<T: DeserializeOwned>(
        resp: reqwest::Response,
    ) -> Result<T, ClientError> {
        let resp = Self::checked_response(resp).await?;
        let url = resp.url().clone();
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = resp.bytes().await?;

        if !is_postcard_content_type(content_type.as_deref()) {
            return Err(ClientError::UnexpectedResponse(format!(
                "expected postcard response from {}, got content-type {}; status={}; body-bytes={}; body-prefix={}",
                url,
                content_type.as_deref().unwrap_or("<missing>"),
                status.as_u16(),
                bytes.len(),
                body_preview(&bytes),
            )));
        }

        postcard::from_bytes(&bytes)
            .map_err(|e| {
                ClientError::UnexpectedResponse(format!(
                    "postcard decode from {} failed: {}; status={}; content-type={}; body-bytes={}; body-prefix={}",
                    url,
                    e,
                    status.as_u16(),
                    content_type.as_deref().unwrap_or("<missing>"),
                    bytes.len(),
                    body_preview(&bytes),
                ))
            })
    }

    // ── Auth ──

    pub async fn login(
        &mut self,
        passphrase: &str,
        username: &str,
        device: AuthDevice<'_>,
    ) -> Result<AuthResult<LoginResponse>, ClientError> {
        validate_server_url(&self.base_url)?;

        let (credential_request, client_login_state) =
            crypto::opaque_client_login_start(passphrase.as_bytes())?;
        let challenge_req = LoginChallengeRequest {
            username: username.to_string(),
            credential_request,
        };
        let challenge_resp = self
            .http
            .post(self.api_url(&["auth", "challenge"])?)
            .header("Content-Type", POSTCARD_CONTENT_TYPE)
            .body(Self::postcard_body(&challenge_req)?)
            .send()
            .await?;

        let challenge_resp: LoginChallengeResponse =
            Self::postcard_response(challenge_resp).await?;
        let device_signing_public_key =
            crypto::device_signing_public_key(device.signing_secret_key);
        let device_login_proof_signature = device
            .id
            .map(|device_id| {
                let proof_body = DeviceLoginProofBodyV1 {
                    version: DEVICE_LOGIN_PROOF_VERSION,
                    challenge_id: challenge_resp.challenge_id.clone(),
                    challenge: challenge_resp.device_proof_challenge.clone(),
                    username: username.to_string(),
                    device_id,
                    device_signing_public_key: device_signing_public_key.to_vec(),
                };
                crypto::sign_device_login_proof_body(device.signing_secret_key, &proof_body)
            })
            .transpose()?;
        let finish = crypto::opaque_client_login_finish(
            &client_login_state,
            passphrase.as_bytes(),
            &challenge_resp.credential_response,
        )?;
        let encryption_key = crypto::derive_data_key_from_opaque_export_key(&finish.export_key);

        let req = LoginRequest {
            challenge_id: challenge_resp.challenge_id,
            credential_finalization: finish.credential_finalization,
            device_id: device.id,
            device_signing_public_key: device_signing_public_key.to_vec(),
            device_login_proof_signature,
            device_name: Some(device.name.to_string()),
            platform: Some(device.platform.to_string()),
        };
        let resp = self
            .http
            .post(self.api_url(&["auth", "login"])?)
            .header("Content-Type", POSTCARD_CONTENT_TYPE)
            .body(Self::postcard_body(&req)?)
            .send()
            .await?;

        let login_resp: LoginResponse = Self::postcard_response(resp).await?;
        self.token = Some(login_resp.token.clone());
        debug!("Logged in, device_id={}", login_resp.device_id);
        Ok(AuthResult {
            response: login_resp,
            encryption_key,
        })
    }

    pub async fn register(
        &mut self,
        access_key: &str,
        username: &str,
        passphrase: &str,
        device: AuthDevice<'_>,
    ) -> Result<AuthResult<RegisterFinishResponse>, ClientError> {
        validate_server_url(&self.base_url)?;

        let (registration_request, client_state) =
            crypto::opaque_client_register_start(passphrase.as_bytes())?;
        let start_req = RegisterStartRequest {
            access_key: access_key.to_string(),
            username: username.to_string(),
            registration_request,
        };
        let resp = self
            .http
            .post(self.api_url(&["auth", "register", "start"])?)
            .header("Content-Type", POSTCARD_CONTENT_TYPE)
            .body(Self::postcard_body(&start_req)?)
            .send()
            .await?;

        let start_resp: RegisterStartResponse = Self::postcard_response(resp).await?;
        let finish = crypto::opaque_client_register_finish(
            &client_state,
            passphrase.as_bytes(),
            &start_resp.registration_response,
        )?;
        let encryption_key = crypto::derive_data_key_from_opaque_export_key(&finish.export_key);
        let device_signing_public_key =
            crypto::device_signing_public_key(device.signing_secret_key);

        let finish_req = RegisterFinishRequest {
            registration_id: start_resp.registration_id,
            registration_upload: finish.registration_upload,
            device_id: device.id,
            device_signing_public_key: device_signing_public_key.to_vec(),
            device_name: Some(device.name.to_string()),
            platform: Some(device.platform.to_string()),
        };
        let resp = self
            .http
            .post(self.api_url(&["auth", "register", "finish"])?)
            .header("Content-Type", POSTCARD_CONTENT_TYPE)
            .body(Self::postcard_body(&finish_req)?)
            .send()
            .await?;

        let register_resp: RegisterFinishResponse = Self::postcard_response(resp).await?;
        self.token = Some(register_resp.token.clone());
        debug!(
            user_id = %register_resp.user_id,
            device_id = %register_resp.device_id,
            "Registered"
        );
        Ok(AuthResult {
            response: register_resp,
            encryption_key,
        })
    }

    pub async fn logout(&mut self) -> Result<(), ClientError> {
        let resp = self
            .http
            .post(self.api_url(&["auth", "logout"])?)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;

        if !resp.status().is_success() {
            warn!("Logout returned {}", resp.status());
        }
        self.token = None;
        Ok(())
    }

    // ── Objects ──

    pub async fn object_init(
        &self,
        req: &ObjectInitRequest,
    ) -> Result<ObjectInitResponse, ClientError> {
        let resp = self
            .http
            .post(self.api_url(&["objects", "init"])?)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .header("Content-Type", POSTCARD_CONTENT_TYPE)
            .body(Self::postcard_body(req)?)
            .send()
            .await?;

        Self::postcard_response(resp).await
    }

    pub async fn object_upload_payload(
        &self,
        object_id: &str,
        payload_id: &str,
        data: Vec<u8>,
    ) -> Result<OkResponse, ClientError> {
        let url = self.api_url(&["objects", object_id, "payloads", payload_id])?;
        let resp = self
            .http
            .put(url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .header("Content-Type", "application/octet-stream")
            .body(data)
            .send()
            .await?;

        Self::postcard_response(resp).await
    }

    pub async fn object_complete(
        &self,
        object_id: &str,
        req: &ObjectCompleteRequest,
    ) -> Result<ObjectCompleteResponse, ClientError> {
        let url = self.api_url(&["objects", object_id, "complete"])?;
        let resp = self
            .http
            .post(url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .header("Content-Type", POSTCARD_CONTENT_TYPE)
            .body(Self::postcard_body(req)?)
            .send()
            .await?;

        Self::postcard_response(resp).await
    }

    pub async fn list_objects(
        &self,
        kind: Option<ObjectKind>,
        limit: Option<u64>,
        created_seq_lte: Option<i64>,
        after: Option<ObjectListCursor>,
    ) -> Result<ObjectListResponse, ClientError> {
        let mut url = self.api_url(&["objects"])?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("limit", &limit.unwrap_or(100).to_string());
            if let Some(kind) = kind {
                query.append_pair("kind", kind.as_ref());
            }
            if let Some(created_seq_lte) = created_seq_lte {
                query.append_pair("created_seq_lte", &created_seq_lte.to_string());
            }
            if let Some(after) = after {
                query.append_pair("after_created_seq", &after.created_seq.to_string());
                query.append_pair("after_id", &after.id.to_string());
            }
        }

        let resp = self
            .http
            .get(url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;

        Self::postcard_response(resp).await
    }

    pub async fn get_object(&self, object_id: &str) -> Result<ObjectListItem, ClientError> {
        let url = self.api_url(&["objects", object_id])?;
        let resp = self
            .http
            .get(url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;

        Self::postcard_response(resp).await
    }

    pub async fn download_object_payload(
        &self,
        object_id: &str,
        payload_id: &str,
    ) -> Result<Vec<u8>, ClientError> {
        let url = self.api_url(&["objects", object_id, "payloads", payload_id])?;
        let resp = self
            .http
            .get(url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;

        Ok(Self::checked_response(resp).await?.bytes().await?.to_vec())
    }

    pub async fn delete_object(
        &self,
        object_id: &str,
    ) -> Result<ObjectDeleteResponse, ClientError> {
        let url = self.api_url(&["objects", object_id])?;
        let resp = self
            .http
            .delete(url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;

        Self::postcard_response(resp).await
    }

}

fn parse_server_url(base_url: &str) -> Result<Url, ClientError> {
    let mut url =
        Url::parse(base_url.trim()).map_err(|e| ClientError::InvalidServerUrl(e.to_string()))?;
    validate_server_url(&url)?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ClientError::InvalidServerUrl(
            "Server URL must not include a query or fragment".into(),
        ));
    }
    let normalized_path = url.path().trim_end_matches('/').to_string();
    url.set_path(&normalized_path);
    Ok(url)
}

fn display_server_url(url: &Url) -> String {
    url.as_str().trim_end_matches('/').to_string()
}

fn validate_server_url(url: &Url) -> Result<(), ClientError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ClientError::InvalidServerUrl(
            "Server URL must not include credentials".into(),
        ));
    }

    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(url) || is_android_emulator_host(url) => Ok(()),
        "http" => Err(ClientError::InvalidServerUrl(
            "Plain HTTP is only allowed for localhost servers".into(),
        )),
        _ => Err(ClientError::InvalidServerUrl(
            "Server URL must use http or https".into(),
        )),
    }
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(addr)) => addr.is_loopback(),
        Some(url::Host::Ipv6(addr)) => addr.is_loopback(),
        None => false,
    }
}

fn is_android_emulator_host(url: &Url) -> bool {
    matches!(
        url.host(),
        Some(url::Host::Ipv4(addr))
            if addr == std::net::Ipv4Addr::new(10, 0, 2, 2)
                || addr == std::net::Ipv4Addr::new(10, 0, 3, 2)
    )
}

fn is_postcard_content_type(value: Option<&str>) -> bool {
    value
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(POSTCARD_CONTENT_TYPE))
}

fn body_preview(bytes: &[u8]) -> String {
    let preview = &bytes[..bytes.len().min(POSTCARD_ERROR_PREVIEW_BYTES)];
    if preview.is_empty() {
        return "<empty>".to_string();
    }

    if let Ok(text) = std::str::from_utf8(preview) {
        return text.chars().flat_map(char::escape_default).collect();
    }

    preview
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

async fn api_error_from_response(resp: reqwest::Response) -> ClientError {
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = resp.bytes().await.unwrap_or_default();
    let error = if is_json_content_type(content_type.as_deref()) {
        serde_json::from_slice::<ErrorResponse>(&bytes).unwrap_or_else(|_| {
            ErrorResponse::new(
                ApiErrorCode::from_http_status(status),
                format!(
                    "HTTP {status} with invalid error body: {}",
                    body_preview(&bytes)
                ),
            )
        })
    } else {
        ErrorResponse::new(ApiErrorCode::from_http_status(status), body_preview(&bytes))
    };

    ClientError::Api { status, error }
}

fn is_json_content_type(value: Option<&str>) -> bool {
    value
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

// ── Encryption helpers ──

/// Encrypt clipboard metadata for object upload.
pub fn encrypt_clipboard_meta(
    meta: &ClipboardMeta,
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let json = serde_json::to_vec(meta)
        .map_err(|e| crypto::CryptoError::Encrypt(format!("json: {}", e)))?;
    let aad = crypto::object_meta_aad_v1(envelope_body)?;
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, &json, &aad)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt clipboard metadata from an object.
pub fn decrypt_clipboard_meta(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
) -> Result<ClipboardMeta, crypto::CryptoError> {
    let aad = crypto::object_meta_aad_v1(envelope_body)?;
    let plaintext = crypto::decrypt(encryption_key, nonce, ciphertext, &aad)?;
    serde_json::from_slice(&plaintext)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("json: {}", e)))
}

/// Encrypt clipboard payload bytes for object upload.
pub fn encrypt_clipboard_payload(
    data: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
    payload_id: ObjectPayloadId,
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let aad = crypto::object_payload_aad_v1(envelope_body, payload_id)?;
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, data, &aad)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt clipboard payload bytes from an object.
pub fn decrypt_clipboard_payload(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
    payload_id: ObjectPayloadId,
) -> Result<Vec<u8>, crypto::CryptoError> {
    let aad = crypto::object_payload_aad_v1(envelope_body, payload_id)?;
    crypto::decrypt(encryption_key, nonce, ciphertext, &aad)
}

/// Encrypt file metadata for object upload.
pub fn encrypt_file_meta_bytes(
    meta: &FileMeta,
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let json = serde_json::to_vec(meta)
        .map_err(|e| crypto::CryptoError::Encrypt(format!("json: {}", e)))?;
    let aad = crypto::object_meta_aad_v1(envelope_body)?;
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, &json, &aad)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt file metadata from object bytes.
pub fn decrypt_file_meta_bytes(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
) -> Result<FileMeta, crypto::CryptoError> {
    let aad = crypto::object_meta_aad_v1(envelope_body)?;
    let plaintext = crypto::decrypt(encryption_key, nonce, ciphertext, &aad)?;
    decode_file_meta_plaintext(&plaintext)
}

fn decode_file_meta_plaintext(plaintext: &[u8]) -> Result<FileMeta, crypto::CryptoError> {
    serde_json::from_slice(plaintext)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("json: {}", e)))
}

/// Encrypt file blob data for object upload.
pub fn encrypt_file_blob_bytes(
    data: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
    payload_id: ObjectPayloadId,
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let aad = crypto::object_payload_aad_v1(envelope_body, payload_id)?;
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, data, &aad)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt file blob data from object bytes.
pub fn decrypt_file_blob_bytes(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
    payload_id: ObjectPayloadId,
) -> Result<Vec<u8>, crypto::CryptoError> {
    let aad = crypto::object_payload_aad_v1(envelope_body, payload_id)?;
    crypto::decrypt(encryption_key, nonce, ciphertext, &aad)
}

// ── Errors ──

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error {status} ({code}): {message}", code = error.code, message = error.message)]
    Api { status: u16, error: ErrorResponse },
    #[error("Crypto error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("WebSocket error: {0}")]
    WebSocket(String),
    #[error("Local store error: {0}")]
    LocalStore(String),
    /// No active session: a token, device identity, signing key, or encryption
    /// key required for this action is missing. The client must log in first.
    #[error("Not authenticated; sign in first")]
    NotAuthenticated,
    /// A clipboard or object MIME type the client does not know how to handle.
    #[error("Unsupported MIME type: {mime_type}")]
    UnsupportedMimeType { mime_type: String },
    /// A clipboard or file item is not present in the local state.
    #[error("Item not found: {id}")]
    ItemNotFound { id: String },
    /// The item exists but its stored payload bytes are unavailable locally.
    #[error("Payload not found for item: {id}")]
    PayloadNotFound { id: String },
    /// A UUID-typed identifier (device, object, or payload id) failed to parse.
    #[error("Invalid {kind}: {source}")]
    InvalidId {
        kind: &'static str,
        source: uuid::Error,
    },
    /// The configured server URL is not a usable base URL or violates client
    /// transport policy (scheme, credentials, query/fragment).
    #[error("Invalid server URL: {0}")]
    InvalidServerUrl(String),
    /// An object was a different kind than the operation requires.
    #[error("Expected a {expected} object but found a {actual} object")]
    UnexpectedObjectKind {
        expected: ObjectKind,
        actual: ObjectKind,
    },
    /// The server's response did not meet a protocol expectation the client
    /// relies on (missing fields, mismatched identity, unexpected encoding).
    #[error("Unexpected server response: {0}")]
    UnexpectedResponse(String),
    /// Encoding a request body to the postcard wire format failed.
    #[error("Serialization error: {0}")]
    Serialization(#[from] postcard::Error),
    /// A local filesystem operation failed.
    #[error("{context}: {source}")]
    Io {
        context: &'static str,
        source: std::io::Error,
    },
    /// The action is not available in this build or on this platform.
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Other(String),
}

#[cfg(test)]
mod crypto_tests {
    use super::*;

    fn envelope_body(object_id: uuid::Uuid, payload_id: uuid::Uuid) -> ObjectEnvelopeBodyV1 {
        ObjectEnvelopeBodyV1 {
            object_id: object_id.into(),
            object_type: ObjectKind::File,
            object_version: 1,
            source_device_id: uuid::Uuid::now_v7().into(),
            created_at: "2026-05-31T00:00:00Z".into(),
            operation: ObjectEnvelopeOperation::Create,
            meta_nonce: vec![0_u8; crypto::XCHACHA20_NONCE_BYTES],
            sha256_meta_ciphertext: vec![0_u8; crypto::SHA256_BYTES],
            payloads: vec![ObjectEnvelopePayloadV1 {
                id: payload_id.into(),
                nonce: vec![0_u8; crypto::XCHACHA20_NONCE_BYTES],
                ciphertext_size: 0,
                sha256_ciphertext: vec![0_u8; crypto::SHA256_BYTES],
            }],
        }
    }

    #[test]
    fn file_blob_decrypt_rejects_swapped_object_context() {
        let key = [7_u8; 32];
        let payload_a = uuid::Uuid::now_v7();
        let payload_b = uuid::Uuid::now_v7();
        let body_a = envelope_body(uuid::Uuid::now_v7(), payload_a);
        let body_b = envelope_body(uuid::Uuid::now_v7(), payload_b);

        let (nonce, ciphertext) =
            encrypt_file_blob_bytes(b"secret file", &key, &body_a, payload_a.into())
                .expect("encrypt");

        assert!(
            decrypt_file_blob_bytes(&nonce, &ciphertext, &key, &body_b, payload_b.into()).is_err(),
            "ciphertext from object A must not decrypt as object B"
        );
    }

    #[test]
    fn file_metadata_decrypt_rejects_different_payload_set() {
        let key = [9_u8; 32];
        let object_id = uuid::Uuid::now_v7();
        let body_a = envelope_body(object_id, uuid::Uuid::now_v7());
        let body_b = envelope_body(object_id, uuid::Uuid::now_v7());
        let meta = FileMeta {
            filename: "notes.txt".into(),
            mime_type: "text/plain".into(),
            size: Some(12),
        };

        let (nonce, ciphertext) = encrypt_file_meta_bytes(&meta, &key, &body_a).expect("encrypt");

        assert!(
            decrypt_file_meta_bytes(&nonce, &ciphertext, &key, &body_b).is_err(),
            "metadata must be bound to the payload set it describes"
        );
    }
}

impl ClientError {
    pub fn error_response(&self) -> ErrorResponse {
        match self {
            Self::Api { error, .. } => error.clone(),
            Self::Http(error) => ErrorResponse::new(ApiErrorCode::Unknown, error.to_string()),
            Self::Crypto(error) => ErrorResponse::new(ApiErrorCode::Unknown, error.to_string()),
            Self::WebSocket(error) => ErrorResponse::new(ApiErrorCode::Unknown, error.clone()),
            Self::LocalStore(error) => ErrorResponse::new(ApiErrorCode::Storage, error.clone()),
            Self::NotAuthenticated => {
                ErrorResponse::new(ApiErrorCode::Unauthorized, self.to_string())
            }
            Self::UnsupportedMimeType { .. } => {
                ErrorResponse::new(ApiErrorCode::UnsupportedMediaType, self.to_string())
            }
            Self::ItemNotFound { .. } | Self::PayloadNotFound { .. } => {
                ErrorResponse::new(ApiErrorCode::NotFound, self.to_string())
            }
            Self::InvalidId { .. } => ErrorResponse::new(ApiErrorCode::InvalidId, self.to_string()),
            Self::InvalidServerUrl(_) => {
                ErrorResponse::new(ApiErrorCode::BadRequest, self.to_string())
            }
            Self::UnexpectedObjectKind { .. } => {
                ErrorResponse::new(ApiErrorCode::InvalidObjectKind, self.to_string())
            }
            Self::UnexpectedResponse(_) | Self::Serialization(_) => {
                ErrorResponse::new(ApiErrorCode::Unknown, self.to_string())
            }
            Self::Io { .. } => ErrorResponse::new(ApiErrorCode::Storage, self.to_string()),
            Self::Unsupported(error) => ErrorResponse::new(ApiErrorCode::Unknown, error.clone()),
            Self::Other(error) => ErrorResponse::new(ApiErrorCode::Unknown, error.clone()),
        }
    }
}

impl From<crate::local_store::LocalStoreError> for ClientError {
    fn from(error: crate::local_store::LocalStoreError) -> Self {
        Self::LocalStore(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_allows_loopback_http() {
        assert!(parse_server_url("http://127.0.0.1:8787").is_ok());
        assert!(parse_server_url("http://[::1]:8787").is_ok());
        assert!(parse_server_url("http://localhost:8787").is_ok());
    }

    #[test]
    fn server_url_allows_android_emulator_host_http() {
        assert!(parse_server_url("http://10.0.2.2:8787").is_ok());
        assert!(parse_server_url("http://10.0.3.2:8787").is_ok());
    }

    fn assert_invalid_server_url(input: &str) {
        assert!(
            matches!(
                parse_server_url(input),
                Err(ClientError::InvalidServerUrl(_))
            ),
            "expected InvalidServerUrl for {input:?}",
        );
    }

    #[test]
    fn server_url_rejects_non_loopback_http() {
        assert_invalid_server_url("http://example.com");
        assert_invalid_server_url("http://192.168.1.5:8787");
    }

    #[test]
    fn server_url_rejects_embedded_credentials() {
        assert_invalid_server_url("https://user:pass@example.com");
    }

    #[test]
    fn server_url_rejects_query_and_fragment() {
        assert_invalid_server_url("https://example.com?debug=true");
        assert_invalid_server_url("https://example.com#clipper");
    }

    #[test]
    fn api_url_preserves_base_path_and_encodes_segments() {
        let client = ApiClient::new("https://example.com/clipper/");
        let url = client
            .api_url(&["objects", "object/with/slashes", "payloads", "payload id"])
            .expect("api URL");

        assert_eq!(
            url.as_str(),
            "https://example.com/clipper/api/objects/object%2Fwith%2Fslashes/payloads/payload%20id"
        );
    }

    #[test]
    fn api_url_handles_root_base_without_double_slash() {
        let client = ApiClient::new("https://example.com/");

        assert_eq!(
            client.api_url(&["health"]).expect("api URL").as_str(),
            "https://example.com/api/health"
        );
    }

    #[test]
    fn base_url_display_omits_trailing_root_slash() {
        let client = ApiClient::new("https://example.com/");

        assert_eq!(client.base_url_display(), "https://example.com");
    }

    #[test]
    fn websocket_url_uses_ws_scheme_and_api_path() {
        let client = ApiClient::new("https://example.com/clipper");

        assert_eq!(
            client.websocket_url().expect("websocket URL").as_str(),
            "wss://example.com/clipper/api/ws"
        );
    }

    #[test]
    fn postcard_content_type_accepts_parameters_case_insensitively() {
        assert!(is_postcard_content_type(Some(POSTCARD_CONTENT_TYPE)));
        assert!(is_postcard_content_type(Some(
            "Application/Vnd.Clipper.Postcard; charset=binary"
        )));
        assert!(!is_postcard_content_type(Some("application/json")));
        assert!(!is_postcard_content_type(None));
    }

    #[test]
    fn body_preview_marks_empty_and_escapes_text() {
        assert_eq!(body_preview(b""), "<empty>");
        assert_eq!(body_preview(b"not found\n"), "not found\\n");
    }
}
