//! HTTP + WebSocket client for the Clipper server.

use base64::Engine;
use reqwest::Client;
use tracing::{debug, warn};

use clipper_core::crypto;
use clipper_core::models::*;

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

    // ── Auth ──

    pub async fn login(
        &mut self,
        passphrase: &str,
        device_name: &str,
        platform: &str,
    ) -> Result<LoginResponse, ClientError> {
        let req = LoginRequest {
            passphrase: passphrase.to_string(),
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

    // ── Clipboard ──

    pub async fn upload_clipboard(
        &self,
        req: &ClipboardUploadRequest,
    ) -> Result<OkResponse, ClientError> {
        let resp = self
            .http
            .post(self.url("/api/clipboard"))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .json(req)
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

    pub async fn list_clipboard(
        &self,
        limit: Option<u64>,
        before: Option<&str>,
    ) -> Result<ClipboardListResponse, ClientError> {
        let mut url = format!(
            "{}/api/clipboard?limit={}",
            self.base_url,
            limit.unwrap_or(100)
        );
        if let Some(b) = before {
            url.push_str(&format!("&before={}", b));
        }

        let resp = self
            .http
            .get(&url)
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

    // ── Files ──

    pub async fn file_init(&self, req: &FileInitRequest) -> Result<FileInitResponse, ClientError> {
        let resp = self
            .http
            .post(self.url("/api/files/init"))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .json(req)
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

    pub async fn file_upload_blob(&self, file_id: &str, data: Vec<u8>) -> Result<(), ClientError> {
        let url = format!("{}/api/files/{}/blob", self.base_url, file_id);
        let resp = self
            .http
            .put(&url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .body(data)
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

        let _ = resp.text().await;
        Ok(())
    }

    pub async fn file_complete(
        &self,
        file_id: &str,
        req: &FileCompleteRequest,
    ) -> Result<OkResponse, ClientError> {
        let url = format!("{}/api/files/{}/complete", self.base_url, file_id);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .json(req)
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

    pub async fn list_files(
        &self,
        limit: Option<u64>,
        before: Option<&str>,
    ) -> Result<FileListResponse, ClientError> {
        let mut url = format!("{}/api/files?limit={}", self.base_url, limit.unwrap_or(100));
        if let Some(b) = before {
            url.push_str(&format!("&before={}", b));
        }

        let resp = self
            .http
            .get(&url)
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

    pub async fn download_file_blob(&self, file_id: &str) -> Result<Vec<u8>, ClientError> {
        let url = format!("{}/api/files/{}/blob", self.base_url, file_id);
        let resp = self
            .http
            .get(&url)
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

        Ok(resp.bytes().await?.to_vec())
    }

    pub async fn delete_file(&self, file_id: &str) -> Result<OkResponse, ClientError> {
        let url = format!("{}/api/files/{}", self.base_url, file_id);
        let resp = self
            .http
            .delete(&url)
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

// ── Encryption helpers ──

/// Encrypt clipboard text for upload. Returns the upload request.
pub fn encrypt_clipboard(
    text: &str,
    enc_key: &[u8; 32],
    device_id: &str,
) -> Result<ClipboardUploadRequest, crypto::CryptoError> {
    let plaintext = text.as_bytes();
    let (nonce, ciphertext) = crypto::encrypt(enc_key, plaintext, crypto::AAD_CLIPBOARD_V1)?;
    let hash = crypto::sha256(&ciphertext);

    Ok(ClipboardUploadRequest {
        id: uuid::Uuid::new_v4().to_string(),
        nonce_b64: B64.encode(&nonce),
        ciphertext_b64: B64.encode(&ciphertext),
        ciphertext_sha256_b64: B64.encode(hash),
        source_device_id: device_id.to_string(),
        client_created_at: Some(chrono::Utc::now().to_rfc3339()),
    })
}

/// Decrypt a clipboard item. Returns the plaintext string.
pub fn decrypt_clipboard(
    item: &ClipboardItem,
    enc_key: &[u8; 32],
) -> Result<String, crypto::CryptoError> {
    let nonce = B64
        .decode(&item.nonce_b64)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("nonce decode: {}", e)))?;
    let ciphertext = B64
        .decode(&item.ciphertext_b64)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("ciphertext decode: {}", e)))?;

    let plaintext = crypto::decrypt(enc_key, &nonce, &ciphertext, crypto::AAD_CLIPBOARD_V1)?;
    String::from_utf8(plaintext).map_err(|e| crypto::CryptoError::Decrypt(format!("utf8: {}", e)))
}

/// Encrypt file metadata for upload.
pub fn encrypt_file_meta(
    meta: &FileMeta,
    enc_key: &[u8; 32],
) -> Result<(String, String), crypto::CryptoError> {
    let json = serde_json::to_vec(meta)
        .map_err(|e| crypto::CryptoError::Encrypt(format!("json: {}", e)))?;
    let (nonce, ciphertext) = crypto::encrypt(enc_key, &json, crypto::AAD_FILE_META_V1)?;
    Ok((B64.encode(&nonce), B64.encode(&ciphertext)))
}

/// Decrypt file metadata.
pub fn decrypt_file_meta(
    nonce_b64: &str,
    ciphertext_b64: &str,
    enc_key: &[u8; 32],
) -> Result<FileMeta, crypto::CryptoError> {
    let nonce = B64
        .decode(nonce_b64)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("nonce decode: {}", e)))?;
    let ciphertext = B64
        .decode(ciphertext_b64)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("ciphertext decode: {}", e)))?;
    let plaintext = crypto::decrypt(enc_key, &nonce, &ciphertext, crypto::AAD_FILE_META_V1)?;
    serde_json::from_slice(&plaintext)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("json: {}", e)))
}

/// Encrypt file blob data.
pub fn encrypt_file_blob(
    data: &[u8],
    enc_key: &[u8; 32],
) -> Result<(String, Vec<u8>), crypto::CryptoError> {
    let (nonce, ciphertext) = crypto::encrypt(enc_key, data, crypto::AAD_FILE_BLOB_V1)?;
    Ok((B64.encode(&nonce), ciphertext))
}

/// Decrypt file blob data.
pub fn decrypt_file_blob(
    nonce_b64: &str,
    ciphertext: &[u8],
    enc_key: &[u8; 32],
) -> Result<Vec<u8>, crypto::CryptoError> {
    let nonce = B64
        .decode(nonce_b64)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("nonce decode: {}", e)))?;
    crypto::decrypt(enc_key, &nonce, ciphertext, crypto::AAD_FILE_BLOB_V1)
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
    #[error("{0}")]
    Other(String),
}
