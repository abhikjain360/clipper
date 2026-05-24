//! Sync engine: manages client state, WebSocket connection, and clipboard/file operations.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use tokio::sync::{Mutex, RwLock, watch};
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

use crate::api_client::{
    ApiClient, ClientError, decrypt_clipboard_meta, decrypt_clipboard_payload,
    decrypt_file_blob_bytes, decrypt_file_meta_bytes, encrypt_clipboard_meta,
    encrypt_clipboard_payload, encrypt_file_blob_bytes, encrypt_file_meta_bytes,
};
use crate::local_store::LocalStore;
pub use clipper_app_types::{
    AppState, ConnectionStatus, DecryptedClipboardItem, DecryptedFileItem,
};
use clipper_core::crypto;
use clipper_core::models::*;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const INLINE_OBJECT_PAYLOAD_MAX_BYTES: usize = 64 * 1024;
const RECENT_CLIPBOARD_LIMIT: usize = 100;
const TEXT_CLIPBOARD_MIME_TYPE: &str = "text/plain";

/// The sync engine that owns all client state.
pub struct SyncEngine {
    api: Mutex<ApiClient>,
    local_store: LocalStore,
    encryption_key: RwLock<Option<Zeroizing<[u8; 32]>>>,
    state: RwLock<AppState>,
    state_tx: watch::Sender<u64>,
    state_rx: watch::Receiver<u64>,
    state_version: std::sync::atomic::AtomicU64,
    last_seq: RwLock<i64>,
    suppressed_text: RwLock<Option<(String, std::time::Instant)>>,
}

impl SyncEngine {
    pub fn new(base_url: &str) -> Arc<Self> {
        Self::new_with_data_dir(base_url, default_data_dir())
    }

    pub fn new_with_data_dir(base_url: &str, data_dir: impl Into<PathBuf>) -> Arc<Self> {
        let (tx, rx) = watch::channel(0u64);
        Arc::new(Self {
            api: Mutex::new(ApiClient::new(base_url)),
            local_store: LocalStore::new(data_dir),
            encryption_key: RwLock::new(None),
            state: RwLock::new(AppState::default()),
            state_tx: tx,
            state_rx: rx,
            state_version: std::sync::atomic::AtomicU64::new(0),
            last_seq: RwLock::new(0),
            suppressed_text: RwLock::new(None),
        })
    }

    pub async fn get_state(&self) -> AppState {
        self.state.read().await.clone()
    }

    pub async fn set_base_url(&self, url: &str) {
        let mut api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
        api.set_base_url(url);
    }

    pub async fn base_url(&self) -> String {
        let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
        api.base_url().to_string()
    }

    pub async fn set_saved_profile(&self, user_id: Option<String>, device_name: Option<String>) {
        let mut state = self.state.write().await;
        state.user_id = user_id;
        state.device_name = device_name;
        drop(state);
        self.bump_version();
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.state_rx.clone()
    }

    fn bump_version(&self) {
        let v = self
            .state_version
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let _ = self.state_tx.send(v);
    }

    // ── Auth ──

    pub async fn login(
        self: &Arc<Self>,
        passphrase: &str,
        device_name: &str,
    ) -> Result<(), ClientError> {
        self.login_with_platform_and_user(passphrase, None, device_name, "macos")
            .await
    }

    pub async fn login_with_platform(
        self: &Arc<Self>,
        passphrase: &str,
        device_name: &str,
        platform: &str,
    ) -> Result<(), ClientError> {
        self.login_with_platform_and_user(passphrase, None, device_name, platform)
            .await
    }

    pub async fn login_with_platform_and_user(
        self: &Arc<Self>,
        passphrase: &str,
        user_id: Option<&str>,
        device_name: &str,
        platform: &str,
    ) -> Result<(), ClientError> {
        let login_resp = {
            let mut api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            api.login(passphrase, user_id, device_name, platform)
                .await?
        };

        self.finish_auth(
            passphrase,
            device_name,
            login_resp.user_id.clone(),
            login_resp.device_id.clone(),
            &login_resp.server,
        )
        .await?;

        info!("Login complete, device_id={}", login_resp.device_id);
        Ok(())
    }

    pub async fn register(
        self: &Arc<Self>,
        access_key: &str,
        passphrase: &str,
        device_name: &str,
    ) -> Result<String, ClientError> {
        self.register_with_platform(access_key, passphrase, device_name, "macos")
            .await
    }

    pub async fn register_with_platform(
        self: &Arc<Self>,
        access_key: &str,
        passphrase: &str,
        device_name: &str,
        platform: &str,
    ) -> Result<String, ClientError> {
        let register_resp = {
            let mut api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            api.register(access_key, passphrase, device_name, platform)
                .await?
        };

        self.finish_auth(
            passphrase,
            device_name,
            register_resp.user_id.clone(),
            register_resp.device_id.clone(),
            &register_resp.server,
        )
        .await?;

        info!(
            user_id = %register_resp.user_id,
            device_id = %register_resp.device_id,
            "Registration complete"
        );
        Ok(register_resp.user_id)
    }

    async fn finish_auth(
        self: &Arc<Self>,
        passphrase: &str,
        device_name: &str,
        user_id: String,
        device_id: String,
        server: &ServerInfo,
    ) -> Result<(), ClientError> {
        let encryption_salt = B64
            .decode(&server.encryption_salt_b64)
            .map_err(|e| ClientError::Other(format!("encryption_salt decode: {}", e)))?;
        self.local_store
            .set_profile(profile_id_from_encryption_salt(&encryption_salt));
        let encryption_key = crypto::derive_key(
            passphrase.as_bytes(),
            &encryption_salt,
            &server.encryption_params,
        )
        .map_err(ClientError::Crypto)?;

        *self.encryption_key.write().await = Some(encryption_key);

        {
            let mut state = self.state.write().await;
            state.logged_in = true;
            state.user_id = Some(user_id);
            state.device_id = Some(device_id);
            state.device_name = Some(device_name.to_string());
            state.error = None;
        }
        self.bump_version();

        self.sync_bootstrap().await?;

        let engine = Arc::clone(self);
        tokio::spawn(async move {
            engine.ws_loop().await;
        });

        // Start clipboard watcher on macOS
        #[cfg(target_os = "macos")]
        {
            let engine = Arc::clone(self);
            crate::clipboard_watcher::start_clipboard_watcher(engine);
        }

        Ok(())
    }

    pub async fn logout(&self) -> Result<(), ClientError> {
        {
            let mut api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            api.logout().await?;
        }
        *self.encryption_key.write().await = None;
        {
            let mut state = self.state.write().await;
            *state = AppState::default();
        }
        self.bump_version();
        info!("Logged out");
        Ok(())
    }

    // ── Clipboard ──

    pub async fn send_clipboard(&self, text: &str) -> Result<(), ClientError> {
        {
            let suppressed = self.suppressed_text.read().await;
            if let Some((ref s, at)) = *suppressed
                && s == text
                && at.elapsed() < Duration::from_secs(5)
            {
                debug!("Suppressed duplicate clipboard upload");
                return Ok(());
            }
        }

        {
            let state = self.state.read().await;
            if let Some(first) = state.clipboard_items.first()
                && first.text == text
            {
                debug!("Clipboard text matches most recent item, skipping");
                return Ok(());
            }
        }

        let device_id = {
            let state = self.state.read().await;
            state
                .device_id
                .clone()
                .ok_or_else(|| ClientError::Other("No device_id".into()))?
        };

        let req = {
            let encryption_key = self.encryption_key.read().await;
            let encryption_key = encryption_key
                .as_ref()
                .ok_or_else(|| ClientError::Other("Not logged in".into()))?;
            encrypt_clipboard(text, encryption_key, &device_id)?
        };

        {
            let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            api.upload_clipboard(&req).await?;
        }

        let item = DecryptedClipboardItem {
            id: req.id,
            text: text.to_string(),
            created_at: req.client_created_at.unwrap_or_default(),
            source_device_id: device_id,
        };
        self.local_store.persist_clipboard_item(&item).await?;
        let clipboard_items = self
            .local_store
            .recent_clipboard_items(RECENT_CLIPBOARD_LIMIT)
            .await?;

        {
            let mut state = self.state.write().await;
            state.clipboard_items = clipboard_items;
        }
        self.bump_version();
        debug!("Clipboard text uploaded");
        Ok(())
    }

    pub async fn copy_to_local(&self, id: &str) -> Result<String, ClientError> {
        let text = {
            let state = self.state.read().await;
            state
                .clipboard_items
                .iter()
                .find(|i| i.id == id)
                .map(|item| item.text.clone())
        };

        let text = match text {
            Some(text) => text,
            None => self
                .local_store
                .clipboard_text(id)
                .await?
                .ok_or_else(|| ClientError::Other("Item not found".into()))?,
        };

        *self.suppressed_text.write().await = Some((text.clone(), std::time::Instant::now()));
        Ok(text)
    }

    // ── Files ──

    pub async fn upload_file(&self, file_path: &str) -> Result<String, ClientError> {
        let path = std::path::Path::new(file_path);
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let data = tokio::fs::read(path)
            .await
            .map_err(|e| ClientError::Other(format!("read file: {}", e)))?;

        let mime_type = mime_guess_from_filename(&filename);

        let encryption_key = self.encryption_key.read().await;
        let encryption_key = encryption_key
            .as_ref()
            .ok_or_else(|| ClientError::Other("Not logged in".into()))?;

        let device_id = {
            let state = self.state.read().await;
            state
                .device_id
                .clone()
                .ok_or_else(|| ClientError::Other("No device_id".into()))?
        };

        let meta = FileMeta {
            filename: filename.clone(),
            mime_type: mime_type.clone(),
            size: Some(data.len() as i64),
        };

        let (meta_nonce_b64, meta_ciphertext_b64) = encrypt_file_meta(&meta, encryption_key)?;
        let (blob_nonce_b64, encrypted_blob) = encrypt_file_blob(&data, encryption_key)?;

        let file_id = uuid::Uuid::new_v4().to_string();
        let blob_hash = crypto::sha256(&encrypted_blob);
        let blob_size = encrypted_blob.len() as i64;

        let init_req = FileInitRequest {
            id: file_id.clone(),
            meta_nonce_b64,
            meta_ciphertext_b64,
            blob_nonce_b64,
            blob_size,
            source_device_id: device_id.clone(),
        };

        {
            let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            api.file_init(&init_req).await?;
            api.file_upload_blob(&file_id, encrypted_blob).await?;
            api.file_complete(
                &file_id,
                &FileCompleteRequest {
                    sha256_ciphertext_b64: B64.encode(blob_hash),
                    blob_size,
                },
            )
            .await?;
        }

        {
            let mut state = self.state.write().await;
            state.files.insert(
                0,
                DecryptedFileItem {
                    id: file_id.clone(),
                    filename,
                    mime_type,
                    blob_size: data.len() as i64,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    source_device_id: device_id,
                },
            );
        }
        self.bump_version();
        info!(file_id = %file_id, "File uploaded");
        Ok(file_id)
    }

    pub async fn download_file(&self, file_id: &str, target_path: &str) -> Result<(), ClientError> {
        let encryption_key = self.encryption_key.read().await;
        let encryption_key = encryption_key
            .as_ref()
            .ok_or_else(|| ClientError::Other("Not logged in".into()))?;

        let (blob_nonce_b64, encrypted_blob) = {
            let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            let files_resp = api.list_files(Some(500), None).await?;
            let file_item = files_resp
                .items
                .iter()
                .find(|f| f.id == file_id)
                .ok_or_else(|| ClientError::Other("File not found on server".into()))?;
            let nonce = file_item.blob_nonce_b64.clone();
            let blob = api.download_file_blob(file_id).await?;
            (nonce, blob)
        };

        let plaintext = decrypt_file_blob(&blob_nonce_b64, &encrypted_blob, encryption_key)?;
        tokio::fs::write(std::path::Path::new(target_path), &plaintext)
            .await
            .map_err(|e| ClientError::Other(format!("write file: {}", e)))?;

        info!(file_id = %file_id, path = %target_path, "File downloaded");
        Ok(())
    }

    pub async fn delete_file(&self, file_id: &str) -> Result<(), ClientError> {
        {
            let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            api.delete_file(file_id).await?;
        }

        {
            let mut state = self.state.write().await;
            state.files.retain(|f| f.id != file_id);
        }
        self.bump_version();
        info!(file_id = %file_id, "File deleted");
        Ok(())
    }

    // ── Sync ──

    async fn sync_bootstrap(&self) -> Result<(), ClientError> {
        let bootstrap = {
            let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            api.bootstrap().await?
        };

        let (clipboard_items, files) = {
            let encryption_key = self.encryption_key.read().await;
            let encryption_key = encryption_key
                .as_ref()
                .ok_or_else(|| ClientError::Other("Not logged in".into()))?;

            let mut clipboard_items = Vec::new();
            for item in &bootstrap.clipboard_items {
                match decrypt_clipboard(item, encryption_key) {
                    Ok(text) => {
                        clipboard_items.push(DecryptedClipboardItem {
                            id: item.id.clone(),
                            text,
                            created_at: item.created_at.clone(),
                            source_device_id: item.source_device_id.clone(),
                        });
                    }
                    Err(e) => {
                        warn!(id = %item.id, "Failed to decrypt clipboard item: {}", e);
                    }
                }
            }

            let mut files = Vec::new();
            for file in &bootstrap.files {
                match decrypt_file_meta(
                    &file.meta_nonce_b64,
                    &file.meta_ciphertext_b64,
                    encryption_key,
                ) {
                    Ok(meta) => {
                        files.push(DecryptedFileItem {
                            id: file.id.clone(),
                            filename: meta.filename,
                            mime_type: meta.mime_type,
                            blob_size: file.blob_size,
                            created_at: file.created_at.clone(),
                            source_device_id: file.source_device_id.clone(),
                        });
                    }
                    Err(e) => {
                        warn!(id = %file.id, "Failed to decrypt file meta: {}", e);
                    }
                }
            }

            (clipboard_items, files)
        };

        self.local_store
            .persist_clipboard_items(&clipboard_items)
            .await?;
        let clipboard_items = self
            .local_store
            .recent_clipboard_items(RECENT_CLIPBOARD_LIMIT)
            .await?;

        *self.last_seq.write().await = bootstrap.latest_seq;

        {
            let mut state = self.state.write().await;
            state.clipboard_items = clipboard_items;
            state.files = files;
            state.connection_status = ConnectionStatus::Connected;
        }
        self.bump_version();

        debug!(
            "Bootstrap complete: {} clipboard, {} files, seq={}",
            bootstrap.clipboard_items.len(),
            bootstrap.files.len(),
            bootstrap.latest_seq,
        );
        Ok(())
    }

    pub async fn refresh(&self) -> Result<(), ClientError> {
        let (clipboard_resp, files_resp) = {
            let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            let c = api.list_clipboard(Some(100), None).await?;
            let f = api.list_files(Some(100), None).await?;
            (c, f)
        };

        let (clipboard_items, files) = {
            let encryption_key = self.encryption_key.read().await;
            let encryption_key = encryption_key
                .as_ref()
                .ok_or_else(|| ClientError::Other("Not logged in".into()))?;

            let mut clipboard_items = Vec::new();
            for item in &clipboard_resp.items {
                match decrypt_clipboard(item, encryption_key) {
                    Ok(text) => {
                        clipboard_items.push(DecryptedClipboardItem {
                            id: item.id.clone(),
                            text,
                            created_at: item.created_at.clone(),
                            source_device_id: item.source_device_id.clone(),
                        });
                    }
                    Err(e) => {
                        warn!(id = %item.id, "Failed to decrypt: {}", e);
                    }
                }
            }

            let mut files = Vec::new();
            for file in &files_resp.items {
                match decrypt_file_meta(
                    &file.meta_nonce_b64,
                    &file.meta_ciphertext_b64,
                    encryption_key,
                ) {
                    Ok(meta) => {
                        files.push(DecryptedFileItem {
                            id: file.id.clone(),
                            filename: meta.filename,
                            mime_type: meta.mime_type,
                            blob_size: file.blob_size,
                            created_at: file.created_at.clone(),
                            source_device_id: file.source_device_id.clone(),
                        });
                    }
                    Err(e) => {
                        warn!(id = %file.id, "Failed to decrypt file meta: {}", e);
                    }
                }
            }

            (clipboard_items, files)
        };

        self.local_store
            .persist_clipboard_items(&clipboard_items)
            .await?;
        let clipboard_items = self
            .local_store
            .recent_clipboard_items(RECENT_CLIPBOARD_LIMIT)
            .await?;

        {
            let mut state = self.state.write().await;
            state.clipboard_items = clipboard_items;
            state.files = files;
        }
        self.bump_version();
        Ok(())
    }

    // ── WebSocket ──

    async fn ws_loop(self: &Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        loop {
            {
                let state = self.state.read().await;
                if !state.logged_in {
                    return;
                }
            }

            {
                let mut state = self.state.write().await;
                state.connection_status = ConnectionStatus::Connecting;
            }
            self.bump_version();

            match self.ws_connect().await {
                Ok(()) => {
                    backoff = Duration::from_secs(1);
                }
                Err(e) => {
                    warn!("WebSocket error: {}", e);
                    {
                        let mut state = self.state.write().await;
                        state.connection_status = ConnectionStatus::Disconnected;
                    }
                    self.bump_version();
                }
            }

            {
                let state = self.state.read().await;
                if !state.logged_in {
                    return;
                }
            }

            let jitter = Duration::from_millis(rand::random_range(0..1000));
            tokio::time::sleep(backoff + jitter).await;
            backoff = (backoff * 2).min(max_backoff);
        }
    }

    async fn ws_connect(&self) -> Result<(), ClientError> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite;

        let (token, base_url) = {
            let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            let t = api
                .token()
                .ok_or_else(|| ClientError::Other("No token".into()))?
                .to_string();
            let u = api.base_url().to_string();
            (t, u)
        };

        let ws_url = base_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        let ws_url = format!("{}/api/ws", ws_url);

        let request = tungstenite::http::Request::builder()
            .uri(&ws_url)
            .header("Authorization", format!("Bearer {}", token))
            .header(
                "Host",
                url::Url::parse(&base_url)
                    .map(|u| u.host_str().unwrap_or("localhost").to_string())
                    .unwrap_or_else(|_| "localhost".into()),
            )
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .body(())
            .map_err(|e| ClientError::WebSocket(e.to_string()))?;

        let (ws_stream, _) = tokio_tungstenite::connect_async(request).await.map_err(
            |e: tokio_tungstenite::tungstenite::Error| ClientError::WebSocket(e.to_string()),
        )?;

        let (mut write, mut read) = ws_stream.split();

        let last_seq = *self.last_seq.read().await;
        let hello = WsClientMessage::Hello { last_seq };
        let hello_json =
            serde_json::to_string(&hello).map_err(|e| ClientError::WebSocket(e.to_string()))?;
        write
            .send(tungstenite::Message::Text(hello_json.into()))
            .await
            .map_err(|e: tungstenite::Error| ClientError::WebSocket(e.to_string()))?;

        {
            let mut state = self.state.write().await;
            state.connection_status = ConnectionStatus::Connected;
        }
        self.bump_version();
        info!("WebSocket connected, last_seq={}", last_seq);

        while let Some(msg_result) = read.next().await {
            let msg: tungstenite::Message = msg_result
                .map_err(|e: tungstenite::Error| ClientError::WebSocket(e.to_string()))?;

            match msg {
                tungstenite::Message::Text(text) => {
                    match serde_json::from_str::<WsServerMessage>(&text) {
                        Ok(WsServerMessage::HelloAck { latest_seq, .. }) => {
                            debug!("WS hello_ack, latest_seq={}", latest_seq);
                        }
                        Ok(WsServerMessage::Event {
                            seq,
                            event_type,
                            object_kind,
                            ..
                        }) => {
                            debug!("WS event seq={} type={}", seq, event_type);
                            *self.last_seq.write().await = seq;
                            match object_kind.as_str() {
                                "clipboard" | "file" => {
                                    if let Err(e) = self.refresh().await {
                                        warn!("Failed to refresh after event: {}", e);
                                    }
                                }
                                _ => {}
                            }
                        }
                        Ok(WsServerMessage::Invalidate { .. }) => {
                            info!("WS invalidate, full refresh");
                            if let Err(e) = self.refresh().await {
                                warn!("Failed to refresh after invalidate: {}", e);
                            }
                        }
                        Err(e) => {
                            warn!("Failed to parse WS message: {}", e);
                        }
                    }
                }
                tungstenite::Message::Ping(data) => {
                    let _ = write.send(tungstenite::Message::Pong(data)).await;
                }
                tungstenite::Message::Close(_) => {
                    info!("WebSocket closed by server");
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }
}

fn mime_guess_from_filename(filename: &str) -> String {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "txt" => "text/plain",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "zip" => "application/zip",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn default_data_dir() -> PathBuf {
    std::env::var_os("CLIPPER_CLIENT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("clipper-client"))
}

fn profile_id_from_encryption_salt(encryption_salt: &[u8]) -> String {
    hex_string(&crypto::sha256(encryption_salt))
}

fn hex_string(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
