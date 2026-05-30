//! HTTP + WebSocket client for the Clipper server.

use base64::Engine;
use clipper_core::{crypto, models::*};
use reqwest::Client;
use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, warn};
use url::Url;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Clipper API client.
pub struct ApiClient {
    http: Client,
    base_url: String,
    token: Option<String>,
}

impl ApiClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            http: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: None,
        }
    }

    pub fn set_token(&mut self, token: String) {
        self.token = Some(token);
    }

    pub fn clear_token(&mut self) {
        self.token = None;
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn set_base_url(&mut self, url: &str) {
        self.base_url = url.trim_end_matches('/').to_string();
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth_header(&self) -> Option<String> {
        self.token.as_ref().map(|t| format!("Bearer {}", t))
    }

    async fn checked_response(resp: reqwest::Response) -> Result<reqwest::Response, ClientError> {
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api {
                status,
                message: body,
            });
        }

        Ok(resp)
    }

    fn postcard_body<T: Serialize>(value: &T) -> Result<Vec<u8>, ClientError> {
        postcard::to_allocvec(value).map_err(|e| ClientError::Other(format!("postcard: {}", e)))
    }

    async fn postcard_response<T: DeserializeOwned>(
        resp: reqwest::Response,
    ) -> Result<T, ClientError> {
        let resp = Self::checked_response(resp).await?;
        let bytes = resp.bytes().await?;
        postcard::from_bytes(&bytes)
            .map_err(|e| ClientError::Other(format!("postcard decode: {}", e)))
    }

    // ── Auth ──

    pub async fn login(
        &mut self,
        passphrase: &str,
        username: &str,
        device_name: &str,
        platform: &str,
    ) -> Result<LoginResponse, ClientError> {
        validate_server_url(&self.base_url)?;

        let (credential_request, client_login_state) =
            crypto::opaque_client_login_start(passphrase.as_bytes())?;
        let challenge_req = LoginChallengeRequest {
            username: username.to_string(),
            credential_request,
        };
        let challenge_resp = self
            .http
            .post(self.url("/api/auth/challenge"))
            .json(&challenge_req)
            .send()
            .await?;

        if !challenge_resp.status().is_success() {
            let status = challenge_resp.status().as_u16();
            let body = challenge_resp.text().await.unwrap_or_default();
            return Err(ClientError::Api {
                status,
                message: body,
            });
        }

        let challenge_resp: LoginChallengeResponse = challenge_resp.json().await?;

        let credential_response = B64
            .decode(&challenge_resp.credential_response_b64)
            .map_err(|e| ClientError::Other(format!("credential response decode: {}", e)))?;
        let (credential_finalization, _) = crypto::opaque_client_login_finish(
            &client_login_state,
            passphrase.as_bytes(),
            &credential_response,
        )?;

        let req = LoginRequest {
            challenge_id: challenge_resp.challenge_id,
            credential_finalization,
            device_id: None,
            device_name: Some(device_name.to_string()),
            platform: Some(platform.to_string()),
        };
        let resp = self
            .http
            .post(self.url("/api/auth/login"))
            .json(&req)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api {
                status,
                message: body,
            });
        }

        let login_resp: LoginResponse = resp.json().await?;
        self.token = Some(login_resp.token.clone());
        debug!("Logged in, device_id={}", login_resp.device_id);
        Ok(login_resp)
    }

    pub async fn register(
        &mut self,
        access_key: &str,
        username: &str,
        passphrase: &str,
        device_name: &str,
        platform: &str,
    ) -> Result<RegisterFinishResponse, ClientError> {
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
            .post(self.url("/api/auth/register/start"))
            .json(&start_req)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api {
                status,
                message: body,
            });
        }

        let start_resp: RegisterStartResponse = resp.json().await?;
        let registration_response = B64
            .decode(&start_resp.registration_response_b64)
            .map_err(|e| ClientError::Other(format!("registration response decode: {}", e)))?;
        let registration_upload = crypto::opaque_client_register_finish(
            &client_state,
            passphrase.as_bytes(),
            &registration_response,
        )?;

        let finish_req = RegisterFinishRequest {
            registration_id: start_resp.registration_id,
            registration_upload,
            device_id: None,
            device_name: Some(device_name.to_string()),
            platform: Some(platform.to_string()),
        };
        let resp = self
            .http
            .post(self.url("/api/auth/register/finish"))
            .json(&finish_req)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api {
                status,
                message: body,
            });
        }

        let register_resp: RegisterFinishResponse = resp.json().await?;
        self.token = Some(register_resp.token.clone());
        debug!(
            user_id = %register_resp.user_id,
            device_id = %register_resp.device_id,
            "Registered"
        );
        Ok(register_resp)
    }

    pub async fn logout(&mut self) -> Result<(), ClientError> {
        let resp = self
            .http
            .post(self.url("/api/auth/logout"))
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
            .post(self.url("/api/objects/init"))
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
        let url = format!(
            "{}/api/objects/{}/payloads/{}",
            self.base_url, object_id, payload_id
        );
        let resp = self
            .http
            .put(&url)
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
    ) -> Result<OkResponse, ClientError> {
        let url = format!("{}/api/objects/{}/complete", self.base_url, object_id);
        let resp = self
            .http
            .post(&url)
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
        before: Option<&str>,
    ) -> Result<ObjectListResponse, ClientError> {
        let mut url = Url::parse(&self.url("/api/objects"))
            .map_err(|e| ClientError::Other(format!("Invalid server URL: {}", e)))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("limit", &limit.unwrap_or(100).to_string());
            if let Some(kind) = kind {
                query.append_pair("kind", kind.as_str());
            }
            if let Some(before) = before {
                query.append_pair("before", before);
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

    pub async fn download_object_payload(
        &self,
        object_id: &str,
        payload_id: &str,
    ) -> Result<Vec<u8>, ClientError> {
        let url = format!(
            "{}/api/objects/{}/payloads/{}",
            self.base_url, object_id, payload_id
        );
        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;

        Ok(Self::checked_response(resp).await?.bytes().await?.to_vec())
    }

    pub async fn delete_object(&self, object_id: &str) -> Result<OkResponse, ClientError> {
        let url = format!("{}/api/objects/{}", self.base_url, object_id);
        let resp = self
            .http
            .delete(&url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;

        Self::postcard_response(resp).await
    }

    // ── Sync ──

    pub async fn bootstrap(&self) -> Result<BootstrapResponse, ClientError> {
        let resp = self
            .http
            .get(self.url("/api/sync/bootstrap"))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api {
                status,
                message: body,
            });
        }

        Ok(resp.json().await?)
    }

    // ── Health ──

    pub async fn health(&self) -> Result<HealthResponse, ClientError> {
        let resp = self.http.get(self.url("/api/health")).send().await?;
        Ok(resp.json().await?)
    }
}

fn validate_server_url(base_url: &str) -> Result<(), ClientError> {
    let url = Url::parse(base_url)
        .map_err(|e| ClientError::Other(format!("Invalid server URL: {}", e)))?;

    if !url.username().is_empty() || url.password().is_some() {
        return Err(ClientError::Other(
            "Server URL must not include credentials".into(),
        ));
    }

    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(&url) || is_android_emulator_host(&url) => Ok(()),
        "http" => Err(ClientError::Other(
            "Plain HTTP is only allowed for localhost servers".into(),
        )),
        _ => Err(ClientError::Other(
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

// ── Encryption helpers ──

/// Encrypt clipboard metadata for object upload.
pub fn encrypt_clipboard_meta(
    meta: &ClipboardMeta,
    encryption_key: &[u8; 32],
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let json = serde_json::to_vec(meta)
        .map_err(|e| crypto::CryptoError::Encrypt(format!("json: {}", e)))?;
    let (nonce, ciphertext) =
        crypto::encrypt(encryption_key, &json, crypto::AAD_CLIPBOARD_META_V1)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt clipboard metadata from an object.
pub fn decrypt_clipboard_meta(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
) -> Result<ClipboardMeta, crypto::CryptoError> {
    let plaintext = crypto::decrypt(
        encryption_key,
        nonce,
        ciphertext,
        crypto::AAD_CLIPBOARD_META_V1,
    )?;
    serde_json::from_slice(&plaintext)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("json: {}", e)))
}

/// Encrypt clipboard payload bytes for object upload.
pub fn encrypt_clipboard_payload(
    data: &[u8],
    encryption_key: &[u8; 32],
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let (nonce, ciphertext) =
        crypto::encrypt(encryption_key, data, crypto::AAD_CLIPBOARD_PAYLOAD_V1)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt clipboard payload bytes from an object.
pub fn decrypt_clipboard_payload(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
) -> Result<Vec<u8>, crypto::CryptoError> {
    crypto::decrypt(
        encryption_key,
        nonce,
        ciphertext,
        crypto::AAD_CLIPBOARD_PAYLOAD_V1,
    )
}

/// Encrypt file metadata for upload.
pub fn encrypt_file_meta(
    meta: &FileMeta,
    encryption_key: &[u8; 32],
) -> Result<(String, String), crypto::CryptoError> {
    let (nonce, ciphertext) = encrypt_file_meta_bytes(meta, encryption_key)?;
    Ok((B64.encode(nonce), B64.encode(ciphertext)))
}

/// Encrypt file metadata for object upload.
pub fn encrypt_file_meta_bytes(
    meta: &FileMeta,
    encryption_key: &[u8; 32],
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let json = serde_json::to_vec(meta)
        .map_err(|e| crypto::CryptoError::Encrypt(format!("json: {}", e)))?;
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, &json, crypto::AAD_FILE_META_V1)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt file metadata.
pub fn decrypt_file_meta(
    nonce_b64: &str,
    ciphertext_b64: &str,
    encryption_key: &[u8; 32],
) -> Result<FileMeta, crypto::CryptoError> {
    let nonce = B64
        .decode(nonce_b64)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("nonce decode: {}", e)))?;
    let ciphertext = B64
        .decode(ciphertext_b64)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("ciphertext decode: {}", e)))?;
    let plaintext = crypto::decrypt(
        encryption_key,
        &nonce,
        &ciphertext,
        crypto::AAD_FILE_META_V1,
    )?;
    decode_file_meta_plaintext(&plaintext)
}

/// Decrypt file metadata from object bytes.
pub fn decrypt_file_meta_bytes(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
) -> Result<FileMeta, crypto::CryptoError> {
    let plaintext = crypto::decrypt(encryption_key, nonce, ciphertext, crypto::AAD_FILE_META_V1)?;
    decode_file_meta_plaintext(&plaintext)
}

fn decode_file_meta_plaintext(plaintext: &[u8]) -> Result<FileMeta, crypto::CryptoError> {
    serde_json::from_slice(plaintext)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("json: {}", e)))
}

/// Encrypt file blob data.
pub fn encrypt_file_blob(
    data: &[u8],
    encryption_key: &[u8; 32],
) -> Result<(String, Vec<u8>), crypto::CryptoError> {
    let (nonce, ciphertext) = encrypt_file_blob_bytes(data, encryption_key)?;
    Ok((B64.encode(nonce), ciphertext))
}

/// Encrypt file blob data for object upload.
pub fn encrypt_file_blob_bytes(
    data: &[u8],
    encryption_key: &[u8; 32],
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, data, crypto::AAD_FILE_BLOB_V1)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt file blob data.
pub fn decrypt_file_blob(
    nonce_b64: &str,
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
) -> Result<Vec<u8>, crypto::CryptoError> {
    let nonce = B64
        .decode(nonce_b64)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("nonce decode: {}", e)))?;
    decrypt_file_blob_bytes(&nonce, ciphertext, encryption_key)
}

/// Decrypt file blob data from object bytes.
pub fn decrypt_file_blob_bytes(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
) -> Result<Vec<u8>, crypto::CryptoError> {
    crypto::decrypt(encryption_key, nonce, ciphertext, crypto::AAD_FILE_BLOB_V1)
}

// ── Errors ──

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error {status}: {message}")]
    Api { status: u16, message: String },
    #[error("Crypto error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("WebSocket error: {0}")]
    WebSocket(String),
    #[error("Local store error: {0}")]
    LocalStore(String),
    #[error("{0}")]
    Other(String),
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
        assert!(validate_server_url("http://127.0.0.1:8787").is_ok());
        assert!(validate_server_url("http://[::1]:8787").is_ok());
        assert!(validate_server_url("http://localhost:8787").is_ok());
    }

    #[test]
    fn server_url_allows_android_emulator_host_http() {
        assert!(validate_server_url("http://10.0.2.2:8787").is_ok());
        assert!(validate_server_url("http://10.0.3.2:8787").is_ok());
    }

    #[test]
    fn server_url_rejects_non_loopback_http() {
        assert!(validate_server_url("http://example.com").is_err());
        assert!(validate_server_url("http://192.168.1.5:8787").is_err());
    }

    #[test]
    fn server_url_rejects_embedded_credentials() {
        assert!(validate_server_url("https://user:pass@example.com").is_err());
    }
}
