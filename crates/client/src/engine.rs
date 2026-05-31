//! Sync engine: manages client state, WebSocket connection, and clipboard/file operations.

use std::{path::PathBuf, sync::Arc, time::Duration};

pub use clipper_app_types::{
    AppState, ConnectionStatus, DecryptedClipboardItem, DecryptedFileItem,
};
use clipper_core::{crypto, models::*};
use futures_util::{StreamExt, TryStreamExt, stream};
use tokio::sync::{Mutex, RwLock, watch};
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

use crate::{
    api_client::{
        ApiClient, ClientError, decrypt_clipboard_meta, decrypt_clipboard_payload,
        decrypt_file_blob_bytes, decrypt_file_meta_bytes, encrypt_clipboard_meta,
        encrypt_clipboard_payload, encrypt_file_blob_bytes, encrypt_file_meta_bytes,
    },
    local_store::LocalStore,
};

const INLINE_OBJECT_PAYLOAD_MAX_BYTES: usize = 64 * 1024;
const RECENT_CLIPBOARD_LIMIT: usize = 100;
const TEXT_CLIPBOARD_MIME_TYPE: &str = "text/plain";
const CLIPBOARD_HYDRATION_CONCURRENCY: usize = 8;
const LOCAL_PERSIST_CONCURRENCY: usize = 8;

#[derive(Debug, Clone)]
pub struct ClipboardPayload {
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub text: Option<String>,
}

struct DecryptedClipboardObject {
    item: DecryptedClipboardItem,
    payload: Vec<u8>,
}

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
    suppressed_payload: RwLock<Option<([u8; 32], std::time::Instant)>>,
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
            suppressed_payload: RwLock::new(None),
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

    pub async fn set_saved_profile(&self, username: Option<String>, device_name: Option<String>) {
        let mut state = self.state.write().await;
        state.username = username;
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
        _ = self.state_tx.send(v);
    }

    // ── Auth ──

    pub async fn login(
        self: &Arc<Self>,
        passphrase: &str,
        username: &str,
        device_name: &str,
    ) -> Result<(), ClientError> {
        self.login_with_platform(passphrase, username, device_name, "macos")
            .await
    }

    pub async fn login_with_platform(
        self: &Arc<Self>,
        passphrase: &str,
        username: &str,
        device_name: &str,
        platform: &str,
    ) -> Result<(), ClientError> {
        let auth = {
            let mut api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            api.login(passphrase, username, device_name, platform)
                .await?
        };
        let login_resp = auth.response;

        self.finish_auth(
            device_name,
            login_resp.username.clone(),
            login_resp.device_id.clone(),
            auth.encryption_key,
        )
        .await?;

        info!("Login complete, device_id={}", login_resp.device_id);
        Ok(())
    }

    pub async fn register(
        self: &Arc<Self>,
        access_key: &str,
        username: &str,
        passphrase: &str,
        device_name: &str,
    ) -> Result<String, ClientError> {
        self.register_with_platform(access_key, username, passphrase, device_name, "macos")
            .await
    }

    pub async fn register_with_platform(
        self: &Arc<Self>,
        access_key: &str,
        username: &str,
        passphrase: &str,
        device_name: &str,
        platform: &str,
    ) -> Result<String, ClientError> {
        let auth = {
            let mut api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            api.register(access_key, username, passphrase, device_name, platform)
                .await?
        };
        let register_resp = auth.response;

        self.finish_auth(
            device_name,
            register_resp.username.clone(),
            register_resp.device_id.clone(),
            auth.encryption_key,
        )
        .await?;

        info!(
            user_id = %register_resp.user_id,
            username = %register_resp.username,
            device_id = %register_resp.device_id,
            "Registration complete"
        );
        Ok(register_resp.username)
    }

    async fn finish_auth(
        self: &Arc<Self>,
        device_name: &str,
        username: String,
        device_id: String,
        encryption_key: Zeroizing<[u8; 32]>,
    ) -> Result<(), ClientError> {
        self.local_store
            .set_profile(profile_id_from_encryption_key(&encryption_key));

        *self.encryption_key.write().await = Some(encryption_key);

        {
            let mut state = self.state.write().await;
            state.logged_in = true;
            state.username = Some(username);
            state.device_id = Some(device_id);
            state.device_name = Some(device_name.to_string());
            state.error = None;
        }
        self.bump_version();

        self.sync_bootstrap().await?;

        #[cfg(not(target_family = "wasm"))]
        {
            let engine = Arc::clone(self);
            tokio::spawn(async move {
                engine.ws_loop().await;
            });
        }

        // Start platform clipboard watcher where background reads are available.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
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
        self.send_clipboard_payload(TEXT_CLIPBOARD_MIME_TYPE, text.as_bytes())
            .await?;
        debug!("Clipboard text uploaded");
        Ok(())
    }

    pub async fn send_clipboard_payload(
        &self,
        mime_type: &str,
        data: &[u8],
    ) -> Result<String, ClientError> {
        if !is_supported_clipboard_mime_type(mime_type) {
            return Err(ClientError::Other(format!(
                "Unsupported clipboard MIME type: {mime_type}"
            )));
        }

        let payload_digest = clipboard_payload_digest(mime_type, data);
        {
            let suppressed = self.suppressed_payload.read().await;
            if let Some((digest, at)) = *suppressed
                && digest == payload_digest
                && at.elapsed() < Duration::from_secs(5)
            {
                debug!("Suppressed duplicate clipboard upload");
                return self
                    .latest_clipboard_item_id_for_digest(&payload_digest)
                    .await
                    .ok_or_else(|| ClientError::Other("Suppressed clipboard item missing".into()));
            }
        }

        {
            let first = {
                let state = self.state.read().await;
                state.clipboard_items.first().cloned()
            };
            if let Some(first) = first
                && same_mime_type(&first.mime_type, mime_type)
                && self
                    .local_store
                    .clipboard_payload(&first.id)
                    .await?
                    .as_deref()
                    .is_some_and(|payload| {
                        clipboard_payload_digest(mime_type, payload) == payload_digest
                    })
            {
                debug!("Clipboard payload matches most recent item, skipping");
                return Ok(first.id.clone());
            }
        }

        let device_id = {
            let state = self.state.read().await;
            state
                .device_id
                .clone()
                .ok_or_else(|| ClientError::Other("No device_id".into()))?
        };

        let object_uuid = uuid::Uuid::now_v7();
        let payload_uuid = uuid::Uuid::now_v7();
        let object_id = object_uuid.to_string();
        let payload_id = payload_uuid.to_string();
        let plaintext_size = data.len() as i64;
        let (meta_nonce, meta_ciphertext, payload_nonce, encrypted_payload) = {
            let encryption_key = self.encryption_key.read().await;
            let encryption_key = encryption_key
                .as_ref()
                .ok_or_else(|| ClientError::Other("Not logged in".into()))?;
            let meta = ClipboardMeta {
                mime_type: mime_type.to_string(),
                size: Some(plaintext_size),
            };
            let (meta_nonce, meta_ciphertext) = encrypt_clipboard_meta(&meta, encryption_key)?;
            let (payload_nonce, encrypted_payload) =
                encrypt_clipboard_payload(data, encryption_key)?;
            (
                meta_nonce,
                meta_ciphertext,
                payload_nonce,
                encrypted_payload,
            )
        };

        let payload_hash = crypto::sha256(&encrypted_payload).to_vec();
        let payload_size = encrypted_payload.len() as i64;
        let init_req = ObjectInitRequest {
            id: object_uuid.into(),
            kind: ObjectKind::Clipboard,
            meta_nonce,
            meta_ciphertext,
            payloads: vec![ObjectPayloadInit {
                id: payload_uuid.into(),
                nonce: payload_nonce,
                ciphertext_size: payload_size,
                sha256_ciphertext: payload_hash.clone(),
                inline_ciphertext: inline_ciphertext(&encrypted_payload),
            }],
        };

        self.submit_single_payload_object(
            &object_id,
            &payload_id,
            &init_req,
            encrypted_payload,
            payload_size,
            payload_hash,
        )
        .await?;

        let created_at = chrono::Utc::now().to_rfc3339();
        let item = DecryptedClipboardItem {
            id: object_id.clone(),
            text: clipboard_display_text(mime_type, data),
            mime_type: mime_type.to_string(),
            payload_size: plaintext_size,
            created_at,
            source_device_id: device_id,
        };
        self.local_store
            .persist_clipboard_payload_item(&item, data)
            .await?;
        let clipboard_items = self
            .local_store
            .recent_clipboard_items(RECENT_CLIPBOARD_LIMIT)
            .await?;

        {
            let mut state = self.state.write().await;
            state.clipboard_items = clipboard_items;
        }
        self.bump_version();
        info!(
            clipboard_id = %object_id,
            mime_type,
            bytes = data.len(),
            "Clipboard uploaded",
        );
        Ok(object_id)
    }

    async fn latest_clipboard_item_id_for_digest(&self, digest: &[u8; 32]) -> Option<String> {
        let items = {
            let state = self.state.read().await;
            state.clipboard_items.clone()
        };
        for item in items {
            let Ok(Some(payload)) = self.local_store.clipboard_payload(&item.id).await else {
                continue;
            };
            if clipboard_payload_digest(&item.mime_type, &payload) == *digest {
                return Some(item.id.clone());
            }
        }
        None
    }

    pub async fn clipboard_payload(&self, id: &str) -> Result<ClipboardPayload, ClientError> {
        let item = {
            let state = self.state.read().await;
            state.clipboard_items.iter().find(|i| i.id == id).cloned()
        }
        .ok_or_else(|| ClientError::Other("Item not found".into()))?;

        let bytes = self
            .local_store
            .clipboard_payload(id)
            .await?
            .ok_or_else(|| ClientError::Other("Item payload not found".into()))?;
        let text = if is_text_mime_type(&item.mime_type) {
            Some(
                String::from_utf8(bytes.clone())
                    .map_err(|e| ClientError::Other(format!("clipboard text utf8: {e}")))?,
            )
        } else {
            None
        };

        *self.suppressed_payload.write().await = Some((
            clipboard_payload_digest(&item.mime_type, &bytes),
            std::time::Instant::now(),
        ));

        Ok(ClipboardPayload {
            mime_type: item.mime_type,
            bytes,
            text,
        })
    }

    pub async fn copy_to_local(&self, id: &str) -> Result<String, ClientError> {
        let state_item = {
            let state = self.state.read().await;
            state
                .clipboard_items
                .iter()
                .find(|i| i.id == id)
                .map(|item| (item.text.clone(), item.mime_type.clone()))
        };

        let text = match state_item {
            Some((text, mime_type)) => {
                if !is_text_mime_type(&mime_type) {
                    return Err(ClientError::Other(format!(
                        "Clipboard item is {mime_type}; copying non-text clipboard payloads is not wired to the OS clipboard yet"
                    )));
                }
                text
            }
            None => self
                .local_store
                .clipboard_text(id)
                .await?
                .ok_or_else(|| ClientError::Other("Item not found".into()))?,
        };

        *self.suppressed_payload.write().await = Some((
            clipboard_payload_digest(TEXT_CLIPBOARD_MIME_TYPE, text.as_bytes()),
            std::time::Instant::now(),
        ));
        Ok(text)
    }

    async fn submit_single_payload_object(
        &self,
        object_id: &str,
        payload_id: &str,
        init_req: &ObjectInitRequest,
        encrypted_payload: Vec<u8>,
        payload_size: i64,
        payload_hash: Vec<u8>,
    ) -> Result<(), ClientError> {
        let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
        let init_resp = api.object_init(init_req).await?;
        let payload_id_typed = payload_id
            .parse()
            .map_err(|e| ClientError::Other(format!("Invalid payload id: {e}")))?;
        if init_resp.complete {
            return Ok(());
        }

        if !init_resp
            .upload_urls
            .iter()
            .any(|upload| upload.id == payload_id_typed)
        {
            return Err(ClientError::Other(
                "Object payload upload URL missing".into(),
            ));
        }

        api.object_upload_payload(object_id, payload_id, encrypted_payload)
            .await?;
        api.object_complete(
            object_id,
            &ObjectCompleteRequest {
                payloads: vec![ObjectPayloadComplete {
                    id: payload_id_typed,
                    ciphertext_size: payload_size,
                    sha256_ciphertext: payload_hash,
                }],
            },
        )
        .await?;
        Ok(())
    }

    // ── Files ──

    #[cfg(not(target_family = "wasm"))]
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
        self.upload_file_bytes(&filename, None, &data).await
    }

    #[cfg(target_family = "wasm")]
    pub async fn upload_file(&self, _file_path: &str) -> Result<String, ClientError> {
        Err(ClientError::Other(
            "Path-based file upload is not available on web".into(),
        ))
    }

    pub async fn upload_file_bytes(
        &self,
        filename: &str,
        mime_type: Option<&str>,
        data: &[u8],
    ) -> Result<String, ClientError> {
        let filename = safe_object_filename(filename);
        let mime_type =
            normalized_mime_type(mime_type).unwrap_or_else(|| mime_guess_from_filename(&filename));

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

        let file_uuid = uuid::Uuid::now_v7();
        let payload_uuid = uuid::Uuid::now_v7();
        let file_id = file_uuid.to_string();
        let payload_id = payload_uuid.to_string();
        let (meta_nonce, meta_ciphertext, blob_nonce, encrypted_blob) = {
            let encryption_key = self.encryption_key.read().await;
            let encryption_key = encryption_key
                .as_ref()
                .ok_or_else(|| ClientError::Other("Not logged in".into()))?;
            let (meta_nonce, meta_ciphertext) = encrypt_file_meta_bytes(&meta, encryption_key)?;
            let (blob_nonce, encrypted_blob) = encrypt_file_blob_bytes(data, encryption_key)?;
            (meta_nonce, meta_ciphertext, blob_nonce, encrypted_blob)
        };

        let blob_hash = crypto::sha256(&encrypted_blob).to_vec();
        let blob_size = encrypted_blob.len() as i64;

        let init_req = ObjectInitRequest {
            id: file_uuid.into(),
            kind: ObjectKind::File,
            meta_nonce,
            meta_ciphertext,
            payloads: vec![ObjectPayloadInit {
                id: payload_uuid.into(),
                nonce: blob_nonce,
                ciphertext_size: blob_size,
                sha256_ciphertext: blob_hash.clone(),
                inline_ciphertext: inline_ciphertext(&encrypted_blob),
            }],
        };

        self.submit_single_payload_object(
            &file_id,
            &payload_id,
            &init_req,
            encrypted_blob,
            blob_size,
            blob_hash,
        )
        .await?;

        {
            let mut state = self.state.write().await;
            state.files.insert(
                0,
                DecryptedFileItem {
                    id: file_id.clone(),
                    filename: filename.clone(),
                    mime_type: mime_type.clone(),
                    blob_size: data.len() as i64,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    source_device_id: device_id,
                },
            );
        }
        self.bump_version();
        info!(file_id = %file_id, filename = %filename, "File uploaded");
        Ok(file_id)
    }

    pub async fn download_file_bytes(&self, file_id: &str) -> Result<Vec<u8>, ClientError> {
        let (blob_nonce, encrypted_blob) = {
            let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            let files_resp = api
                .list_objects(Some(ObjectKind::File), Some(500), None)
                .await?;
            let file_item = files_resp
                .items
                .iter()
                .find(|f| f.id.to_string() == file_id)
                .ok_or_else(|| ClientError::Other("File not found on server".into()))?;
            let payload = single_payload(file_item)?;
            let nonce = payload.nonce.clone();
            let blob = api
                .download_object_payload(file_id, &payload.id.to_string())
                .await?;
            (nonce, blob)
        };

        let plaintext = {
            let encryption_key = self.encryption_key.read().await;
            let encryption_key = encryption_key
                .as_ref()
                .ok_or_else(|| ClientError::Other("Not logged in".into()))?;
            decrypt_file_blob_bytes(&blob_nonce, &encrypted_blob, encryption_key)?
        };
        info!(file_id = %file_id, "File downloaded");
        Ok(plaintext)
    }

    #[cfg(not(target_family = "wasm"))]
    pub async fn download_file(&self, file_id: &str, target_path: &str) -> Result<(), ClientError> {
        let plaintext = self.download_file_bytes(file_id).await?;
        tokio::fs::write(std::path::Path::new(target_path), &plaintext)
            .await
            .map_err(|e| ClientError::Other(format!("write file: {}", e)))?;

        info!(file_id = %file_id, path = %target_path, "File downloaded");
        Ok(())
    }

    #[cfg(target_family = "wasm")]
    pub async fn download_file(
        &self,
        _file_id: &str,
        _target_path: &str,
    ) -> Result<(), ClientError> {
        Err(ClientError::Other(
            "Path-based file download is not available on web".into(),
        ))
    }

    pub async fn delete_file(&self, file_id: &str) -> Result<(), ClientError> {
        {
            let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            api.delete_object(file_id).await?;
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

        let (clipboard_objects, files) = self.fetch_object_state(100).await?;

        self.persist_clipboard_objects(&clipboard_objects).await?;
        let clipboard_items = self
            .local_store
            .recent_clipboard_items(RECENT_CLIPBOARD_LIMIT)
            .await?;
        let clipboard_count = clipboard_items.len();
        let file_count = files.len();

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
            clipboard_count, file_count, bootstrap.latest_seq,
        );
        Ok(())
    }

    pub async fn refresh(&self) -> Result<(), ClientError> {
        let (clipboard_objects, files) = self.fetch_object_state(100).await?;

        self.persist_clipboard_objects(&clipboard_objects).await?;
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

    async fn persist_clipboard_objects(
        &self,
        objects: &[DecryptedClipboardObject],
    ) -> Result<(), ClientError> {
        let objects = objects
            .iter()
            .map(|object| (object.item.clone(), object.payload.clone()))
            .collect::<Vec<_>>();
        stream::iter(objects)
            .map(|(item, payload)| async move {
                self.local_store
                    .persist_clipboard_payload_item(&item, &payload)
                    .await
            })
            .buffer_unordered(LOCAL_PERSIST_CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;
        Ok(())
    }

    async fn fetch_object_state(
        &self,
        limit: u64,
    ) -> Result<(Vec<DecryptedClipboardObject>, Vec<DecryptedFileItem>), ClientError> {
        let api = {
            let api: tokio::sync::MutexGuard<'_, ApiClient> = self.api.lock().await;
            api.clone()
        };
        let (clipboard_resp, files_resp) = tokio::try_join!(
            api.list_objects(Some(ObjectKind::Clipboard), Some(limit), None),
            api.list_objects(Some(ObjectKind::File), Some(limit), None),
        )?;

        let encryption_key = {
            let encryption_key = self.encryption_key.read().await;
            **encryption_key
                .as_ref()
                .ok_or_else(|| ClientError::Other("Not logged in".into()))?
        };

        let clipboard_items = stream::iter(clipboard_resp.items)
            .map(|item| {
                let api = &api;
                async move {
                    match self
                        .decrypt_clipboard_object_item_with_api(api, &item, &encryption_key)
                        .await
                    {
                        Ok(item) => Some(item),
                        Err(e) => {
                            warn!(id = %item.id, "Failed to load clipboard object: {}", e);
                            None
                        }
                    }
                }
            })
            .buffer_unordered(CLIPBOARD_HYDRATION_CONCURRENCY)
            .filter_map(std::future::ready)
            .collect::<Vec<_>>()
            .await;

        let mut files = Vec::new();
        for item in &files_resp.items {
            match decrypt_file_object_item(item, &encryption_key) {
                Ok(item) => files.push(item),
                Err(e) => {
                    warn!(id = %item.id, "Failed to decrypt file object: {}", e);
                }
            }
        }

        Ok((clipboard_items, files))
    }

    async fn decrypt_clipboard_object_item_with_api(
        &self,
        api: &ApiClient,
        item: &ObjectListItem,
        encryption_key: &[u8; 32],
    ) -> Result<DecryptedClipboardObject, ClientError> {
        let meta = decrypt_clipboard_meta(&item.meta_nonce, &item.meta_ciphertext, encryption_key)?;
        if !is_supported_clipboard_mime_type(&meta.mime_type) {
            return Err(ClientError::Other(format!(
                "Unsupported clipboard MIME type: {}",
                meta.mime_type
            )));
        }
        let payload = single_payload(item)?;
        let payload_size = meta.size.unwrap_or(payload.ciphertext_size);
        let encrypted_payload = api
            .download_object_payload(&item.id.to_string(), &payload.id.to_string())
            .await?;
        let plaintext =
            decrypt_clipboard_payload(&payload.nonce, &encrypted_payload, encryption_key)?;
        let text = clipboard_display_text(&meta.mime_type, &plaintext);

        Ok(DecryptedClipboardObject {
            item: DecryptedClipboardItem {
                id: item.id.to_string(),
                text,
                mime_type: meta.mime_type,
                payload_size,
                created_at: item.created_at.clone(),
                source_device_id: item.source_device_id.to_string(),
            },
            payload: plaintext,
        })
    }

    // ── WebSocket ──

    #[cfg(not(target_family = "wasm"))]
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

    #[cfg(not(target_family = "wasm"))]
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
                            match object_kind.as_ref() {
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
                        Ok(WsServerMessage::Error { error }) => {
                            warn!("Server rejected WS connection: {error}");
                        }
                        Err(e) => {
                            warn!("Failed to parse WS message: {}", e);
                        }
                    }
                }
                tungstenite::Message::Ping(data) => {
                    _ = write.send(tungstenite::Message::Pong(data)).await;
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

fn normalized_mime_type(mime_type: Option<&str>) -> Option<String> {
    mime_type
        .map(str::trim)
        .filter(|mime_type| !mime_type.is_empty())
        .map(ToOwned::to_owned)
}

fn safe_object_filename(filename: &str) -> String {
    let filename = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(filename)
        .trim();
    if filename.is_empty() || filename == "." || filename == ".." {
        "unknown".to_string()
    } else {
        filename.to_string()
    }
}

fn inline_ciphertext(ciphertext: &[u8]) -> Option<Vec<u8>> {
    (ciphertext.len() <= INLINE_OBJECT_PAYLOAD_MAX_BYTES).then(|| ciphertext.to_vec())
}

fn single_payload(item: &ObjectListItem) -> Result<&ObjectPayloadDescriptor, ClientError> {
    if item.payloads.len() != 1 {
        return Err(ClientError::Other(format!(
            "Object {} has {} payloads; exactly one is supported by this client",
            item.id,
            item.payloads.len()
        )));
    }

    Ok(&item.payloads[0])
}

fn decrypt_file_object_item(
    item: &ObjectListItem,
    encryption_key: &[u8; 32],
) -> Result<DecryptedFileItem, ClientError> {
    let meta = decrypt_file_meta_bytes(&item.meta_nonce, &item.meta_ciphertext, encryption_key)?;
    let payload = single_payload(item)?;
    Ok(DecryptedFileItem {
        id: item.id.to_string(),
        filename: meta.filename,
        mime_type: meta.mime_type,
        blob_size: meta.size.unwrap_or(payload.ciphertext_size),
        created_at: item.created_at.clone(),
        source_device_id: item.source_device_id.to_string(),
    })
}

fn clipboard_display_text(mime_type: &str, data: &[u8]) -> String {
    if is_text_mime_type(mime_type) {
        String::from_utf8_lossy(data).into_owned()
    } else {
        clipboard_display_label(mime_type, data.len() as i64)
    }
}

fn clipboard_display_label(mime_type: &str, size: i64) -> String {
    format!("{mime_type} clipboard payload ({size} bytes)")
}

fn clipboard_payload_digest(mime_type: &str, data: &[u8]) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(mime_type.len() + 1 + data.len());
    bytes.extend_from_slice(normalized_clipboard_mime_type(mime_type).as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(data);
    crypto::sha256(&bytes)
}

fn is_supported_clipboard_mime_type(mime_type: &str) -> bool {
    is_text_mime_type(mime_type) || top_level_mime_type(mime_type) == "image"
}

fn same_mime_type(a: &str, b: &str) -> bool {
    normalized_clipboard_mime_type(a) == normalized_clipboard_mime_type(b)
}

fn is_text_mime_type(mime_type: &str) -> bool {
    top_level_mime_type(mime_type) == "text"
}

fn normalized_clipboard_mime_type(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .unwrap_or(mime_type)
        .trim()
        .to_ascii_lowercase()
}

fn top_level_mime_type(mime_type: &str) -> String {
    normalized_clipboard_mime_type(mime_type)
        .split('/')
        .next()
        .unwrap_or("")
        .to_string()
}

#[cfg(not(target_family = "wasm"))]
fn default_data_dir() -> PathBuf {
    std::env::var_os("CLIPPER_CLIENT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("clipper-client"))
}

#[cfg(target_family = "wasm")]
fn default_data_dir() -> PathBuf {
    PathBuf::from("web")
}

fn profile_id_from_encryption_key(encryption_key: &[u8; 32]) -> String {
    hex_string(&crypto::sha256(encryption_key))
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
