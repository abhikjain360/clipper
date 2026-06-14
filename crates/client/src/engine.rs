//! Sync engine: manages client state, WebSocket connection, and clipboard/file operations.

use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

pub use clipper_app_types::{
    AppState, AuthenticatedSession, ClipboardPayload, ConnectionStatus, DecryptedClipboardItem,
    DecryptedFileItem, DeviceInfo, SavedProfile,
};
use clipper_core::{crypto, models::*};
use futures_util::{StreamExt, stream};
use tokio::sync::{RwLock, watch};
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

use crate::{
    api_client::{
        ApiClient, AuthDevice, ClientError, decrypt_clipboard_meta, decrypt_clipboard_payload,
        decrypt_file_blob_bytes, decrypt_file_meta_bytes, encrypt_clipboard_meta,
        encrypt_clipboard_payload, encrypt_file_blob_bytes, encrypt_file_meta_bytes,
    },
    local_store::{
        DeviceSigningIdentity, EncryptedClipboardObject, EncryptedObject, LocalStore,
        LocalVisibleState,
    },
};

const INLINE_OBJECT_PAYLOAD_MAX_BYTES: usize = 64 * 1024;
const RECENT_CLIPBOARD_LIMIT: usize = 100;
/// MIME type used for plain-text clipboard entries.
pub const TEXT_CLIPBOARD_MIME_TYPE: &str = "text/plain";
const CLIPBOARD_HYDRATION_CONCURRENCY: usize = 8;
/// Largest clipboard payload (plaintext) the client will capture, upload, or
/// accept on download. The server is untrusted for content, so the client must
/// bound payload sizes independently of any server-supplied/server-signed
/// `ciphertext_size`: reconciliation downloads run automatically on connect, so
/// an unbounded size would let a hostile server force the client to buffer
/// arbitrarily many bytes (per-download, with hydration concurrency) and OOM.
pub const MAX_CLIPBOARD_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
/// Absolute ceiling on a clipboard payload's ciphertext that the client will
/// buffer on download, independent of the server-signed `ciphertext_size`.
/// XChaCha20-Poly1305 adds only a fixed tag, so this stays just above the
/// plaintext cap to never reject the client's own uploads.
const MAX_CLIPBOARD_PAYLOAD_CIPHERTEXT_BYTES: i64 = (MAX_CLIPBOARD_PAYLOAD_BYTES + 4096) as i64;
/// Absolute ceiling on a file blob's ciphertext that the client will buffer on
/// download, independent of the server-signed `ciphertext_size`. Matches the
/// server's default `max_file_blob_bytes` so a hostile server cannot advertise
/// a multi-GiB size and OOM the client during a download.
const MAX_FILE_PAYLOAD_CIPHERTEXT_BYTES: i64 = 512 * 1024 * 1024;
const OBJECT_ENVELOPE_VERSION_V1: u64 = 1;
#[cfg(target_family = "wasm")]
const WS_TICKET_PROTOCOL: &str = "clipper-ticket";

struct DecryptedClipboardObject {
    item: DecryptedClipboardItem,
    payload: Vec<u8>,
    encrypted: EncryptedClipboardObject,
}

/// The sync engine that owns all client state.
pub struct SyncEngine {
    api: ApiClient,
    local_store: LocalStore,
    encryption_key: RwLock<Option<Zeroizing<[u8; 32]>>>,
    device_signing_key: RwLock<Option<Zeroizing<[u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]>>>,
    state: RwLock<AppState>,
    state_tx: watch::Sender<u64>,
    state_version: std::sync::atomic::AtomicU64,
    ws_restart_tx: watch::Sender<u64>,
    ws_restart_rx: watch::Receiver<u64>,
    suppressed_payload: RwLock<Option<([u8; 32], std::time::Instant)>>,
}

impl SyncEngine {
    pub fn new_with_data_dir(base_url: &str, data_dir: impl Into<PathBuf>) -> Arc<Self> {
        Self::try_new_with_data_dir(base_url, data_dir).expect("invalid Clipper server URL")
    }

    pub fn try_new_with_data_dir(
        base_url: &str,
        data_dir: impl Into<PathBuf>,
    ) -> Result<Arc<Self>, ClientError> {
        let (tx, _) = watch::channel(0u64);
        let (ws_restart_tx, ws_restart_rx) = watch::channel(0u64);
        Ok(Arc::new(Self {
            api: ApiClient::try_new(base_url)?,
            local_store: LocalStore::new(data_dir),
            encryption_key: RwLock::new(None),
            device_signing_key: RwLock::new(None),
            state: RwLock::new(AppState::default()),
            state_tx: tx,
            state_version: std::sync::atomic::AtomicU64::new(0),
            ws_restart_tx,
            ws_restart_rx,
            suppressed_payload: RwLock::new(None),
        }))
    }

    pub async fn get_state(&self) -> AppState {
        self.state.read().await.clone()
    }

    pub fn base_url(&self) -> String {
        self.api.base_url_display()
    }

    pub async fn set_saved_profile(&self, username: Option<String>, device_name: Option<String>) {
        let mut state = self.state.write().await;
        state.saved_profile = username.map(|username| SavedProfile {
            username,
            device_name: device_name.unwrap_or_default(),
        });
        drop(state);
        self.bump_version();
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.state_tx.subscribe()
    }

    pub fn state_version(&self) -> u64 {
        self.state_version.load(Ordering::Acquire)
    }

    pub async fn wait_for_state_change_after(&self, seen_version: u64) -> Result<u64, ClientError> {
        let mut rx = self.subscribe();
        loop {
            let current = *rx.borrow_and_update();
            if current > seen_version {
                return Ok(current);
            }
            rx.changed()
                .await
                .map_err(|_| ClientError::Other("state stream closed".into()))?;
        }
    }

    fn bump_version(&self) {
        let v = self.state_version.fetch_add(1, Ordering::AcqRel) + 1;
        _ = self.state_tx.send(v);
    }

    // ── Auth ──

    pub async fn login_with_platform(
        self: &Arc<Self>,
        passphrase: &str,
        username: &str,
        device_name: &str,
        platform: &str,
    ) -> Result<(), ClientError> {
        let prepared = self.api.login_prepare(passphrase, username).await?;
        // The encryption key from `prepare` is the same value `finish_auth`
        // later hashes into the profile id, so the device identity is keyed to
        // the same profile that owns the rest of this user's local storage.
        let profile_id = profile_id_from_encryption_key(&prepared.encryption_key);
        let mut signing_identity = self
            .local_store
            .load_or_create_device_signing_identity(
                &profile_id,
                &prepared.device_identity_wrapping_key,
            )
            .await?;
        let requested_device_id = optional_device_id(signing_identity.device_id.as_deref())?;
        let auth = self
            .api
            .login_finish(
                username,
                AuthDevice {
                    id: requested_device_id,
                    name: device_name,
                    platform,
                    signing_secret_key: &signing_identity.signing_secret_key,
                },
                prepared,
            )
            .await?;
        let crate::api_client::AuthResult {
            response: login_resp,
            encryption_key,
            device_identity_wrapping_key,
        } = auth;
        signing_identity.device_id = Some(login_resp.device_id.clone());
        self.local_store
            .persist_device_signing_identity(
                &profile_id,
                &signing_identity,
                &device_identity_wrapping_key,
            )
            .await?;

        self.finish_auth(
            device_name,
            login_resp.username.clone(),
            login_resp.device_id.clone(),
            encryption_key,
            signing_identity,
        )
        .await?;

        info!("Login complete, device_id={}", login_resp.device_id);
        Ok(())
    }

    pub async fn register_with_platform(
        self: &Arc<Self>,
        access_key: &str,
        username: &str,
        passphrase: &str,
        device_name: &str,
        platform: &str,
    ) -> Result<String, ClientError> {
        let prepared = self
            .api
            .register_prepare(access_key, username, passphrase)
            .await?;
        let profile_id = profile_id_from_encryption_key(&prepared.encryption_key);
        let mut signing_identity = self
            .local_store
            .load_or_create_device_signing_identity(
                &profile_id,
                &prepared.device_identity_wrapping_key,
            )
            .await?;
        let requested_device_id = optional_device_id(signing_identity.device_id.as_deref())?;
        let auth = self
            .api
            .register_finish(
                AuthDevice {
                    id: requested_device_id,
                    name: device_name,
                    platform,
                    signing_secret_key: &signing_identity.signing_secret_key,
                },
                prepared,
            )
            .await?;
        let crate::api_client::AuthResult {
            response: register_resp,
            encryption_key,
            device_identity_wrapping_key,
        } = auth;
        signing_identity.device_id = Some(register_resp.device_id.clone());
        self.local_store
            .persist_device_signing_identity(
                &profile_id,
                &signing_identity,
                &device_identity_wrapping_key,
            )
            .await?;

        self.finish_auth(
            device_name,
            register_resp.username.clone(),
            register_resp.device_id.clone(),
            encryption_key,
            signing_identity,
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
        signing_identity: DeviceSigningIdentity,
    ) -> Result<(), ClientError> {
        let cache_key = *encryption_key;
        self.local_store
            .set_profile(profile_id_from_encryption_key(&encryption_key));

        *self.encryption_key.write().await = Some(encryption_key);
        *self.device_signing_key.write().await = Some(signing_identity.signing_secret_key);

        {
            let mut state = self.state.write().await;
            state.session = Some(AuthenticatedSession {
                username: username.clone(),
                device_id: device_id.clone(),
                device_name: device_name.to_string(),
            });
            state.saved_profile = Some(SavedProfile {
                username,
                device_name: device_name.to_string(),
            });
            state.connection_status = ConnectionStatus::Connecting;
            state.error = None;
        }
        self.bump_version();

        match self
            .local_store
            .hydrate_ciphertext_cache(&cache_key, RECENT_CLIPBOARD_LIMIT)
            .await
        {
            Ok(visible) => self.publish_visible_state(visible).await,
            Err(error) => warn!("Failed to hydrate local ciphertext cache: {}", error),
        }

        {
            let engine = Arc::clone(self);
            spawn_background(async move {
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

    async fn current_device_signing_context(
        &self,
    ) -> Result<
        (
            String,
            DeviceId,
            Zeroizing<[u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]>,
        ),
        ClientError,
    > {
        let device_id = {
            let state = self.state.read().await;
            state
                .device_id()
                .map(ToString::to_string)
                .ok_or(ClientError::NotAuthenticated)?
        };
        let device_id_typed = device_id.parse().map_err(|source| ClientError::InvalidId {
            kind: "device id",
            source,
        })?;
        let signing_key = {
            let signing_key = self.device_signing_key.read().await;
            let signing_key = signing_key.as_ref().ok_or(ClientError::NotAuthenticated)?;
            Zeroizing::new(**signing_key)
        };
        Ok((device_id, device_id_typed, signing_key))
    }

    pub async fn logout(&self) -> Result<(), ClientError> {
        // Best-effort server-side revocation: an offline or failed call must not
        // leave key material resident, so tear down local state unconditionally.
        if let Err(error) = self.api.logout().await {
            warn!(%error, "Server-side logout failed; clearing local session anyway");
        }
        self.api.clear_token();
        *self.encryption_key.write().await = None;
        *self.device_signing_key.write().await = None;
        self.local_store.clear_memory().await;
        {
            let mut state = self.state.write().await;
            *state = AppState::default();
        }
        self.bump_version();
        info!("Logged out");
        Ok(())
    }

    // ── Devices ──

    /// List the user's registered devices, marking the one this client is
    /// logged in on (`is_current`) so the UI can keep it out of harm's way.
    pub async fn list_devices(&self) -> Result<Vec<DeviceInfo>, ClientError> {
        let current_device_id = self.current_device_id().await?;
        let response = self.api.list_devices().await?;
        Ok(response
            .devices
            .into_iter()
            .map(|device| {
                let id = device.id.to_string();
                DeviceInfo {
                    is_current: id == current_device_id,
                    id,
                    name: device.name,
                    platform: device.platform,
                    created_at: device.created_at,
                    last_seen_at: device.last_seen_at,
                }
            })
            .collect())
    }

    /// Remove one of the user's devices. Removing the current device revokes
    /// this session server-side, so we tear down local auth state the way
    /// `logout` does and let the UI return to the login screen.
    pub async fn remove_device(&self, device_id: &str) -> Result<(), ClientError> {
        let current_device_id = self.current_device_id().await?;
        let is_current = device_id == current_device_id;
        let result = self.api.remove_device(device_id).await;
        if is_current {
            // Removing the current device revokes this session server-side, so
            // tear down local auth state regardless of whether the server call
            // succeeded — a failed/offline call must not leave keys resident.
            if let Err(error) = &result {
                warn!(%error, "Removing current device failed server-side; clearing local session anyway");
            }
            self.api.clear_token();
            *self.encryption_key.write().await = None;
            *self.device_signing_key.write().await = None;
            self.local_store.clear_memory().await;
            *self.state.write().await = AppState::default();
            self.bump_version();
            info!("Removed the current device; local session cleared");
            return Ok(());
        }
        result?;
        self.bump_version();
        Ok(())
    }

    async fn current_device_id(&self) -> Result<String, ClientError> {
        let state = self.state.read().await;
        state
            .device_id()
            .map(ToString::to_string)
            .ok_or(ClientError::NotAuthenticated)
    }

    // ── Clipboard ──

    pub async fn send_clipboard_payload(
        &self,
        mime_type: &str,
        data: &[u8],
    ) -> Result<String, ClientError> {
        if !is_supported_clipboard_mime_type(mime_type) {
            return Err(ClientError::UnsupportedMimeType {
                mime_type: mime_type.to_string(),
            });
        }
        // Authoritative, platform-independent ceiling: never buffer + encrypt an
        // oversized clipboard payload. The clipboard is a shared same-user
        // resource any local app can fill, so an unbounded capture would let a
        // local process (or a buggy/legitimate huge copy) double the bytes in
        // memory (plaintext + ciphertext) before the server ever rejects them.
        if data.len() > MAX_CLIPBOARD_PAYLOAD_BYTES {
            return Err(ClientError::PayloadTooLarge {
                size: data.len() as i64,
                limit: MAX_CLIPBOARD_PAYLOAD_BYTES as i64,
            });
        }

        let encryption_key = self.current_encryption_key().await?;
        let payload_digest = clipboard_payload_digest(mime_type, data);
        {
            let suppressed = self.suppressed_payload.read().await;
            if let Some((digest, at)) = *suppressed
                && digest == payload_digest
                && at.elapsed() < Duration::from_secs(5)
            {
                debug!("Suppressed duplicate clipboard upload");
                return self
                    .latest_clipboard_item_id_for_digest(&payload_digest, &encryption_key)
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
                    .clipboard_payload(&first.id, &encryption_key)
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

        let (device_id, device_id_typed, signing_key) =
            self.current_device_signing_context().await?;

        let object_uuid = uuid::Uuid::now_v7();
        let payload_uuid = uuid::Uuid::now_v7();
        let object_id = object_uuid.to_string();
        let payload_id = payload_uuid.to_string();
        let object_id_typed: ObjectId = object_uuid.into();
        let payload_id_typed: ObjectPayloadId = payload_uuid.into();
        let created_at = chrono::Utc::now().to_rfc3339();
        let plaintext_size = data.len() as i64;
        let aad_body = create_object_envelope_body_for_aad(
            object_id_typed,
            ObjectKind::Clipboard,
            device_id_typed,
            created_at.clone(),
            vec![payload_id_typed],
        );
        let (meta_nonce, meta_ciphertext, payload_nonce, encrypted_payload) = {
            let meta = ClipboardMeta {
                mime_type: mime_type.to_string(),
                size: Some(plaintext_size),
            };
            let (meta_nonce, meta_ciphertext) =
                encrypt_clipboard_meta(&meta, &encryption_key, &aad_body)?;
            let (payload_nonce, encrypted_payload) =
                encrypt_clipboard_payload(data, &encryption_key, &aad_body, payload_id_typed)?;
            (
                meta_nonce,
                meta_ciphertext,
                payload_nonce,
                encrypted_payload,
            )
        };

        let payload_hash = crypto::sha256(&encrypted_payload).to_vec();
        let payload_size = encrypted_payload.len() as i64;
        let envelope_body = create_object_envelope_body(
            object_id_typed,
            ObjectKind::Clipboard,
            device_id_typed,
            created_at.clone(),
            meta_nonce.clone(),
            crypto::sha256(&meta_ciphertext).to_vec(),
            vec![ObjectEnvelopePayloadV1 {
                id: payload_id_typed,
                nonce: payload_nonce.clone(),
                ciphertext_size: payload_size,
                sha256_ciphertext: payload_hash.clone(),
            }],
        );
        let envelope = ObjectEnvelopeV1 {
            signature: crypto::sign_object_envelope_body(&signing_key, &envelope_body)?,
            body: envelope_body,
        };
        let init_req = ObjectInitRequest {
            id: object_id_typed,
            kind: ObjectKind::Clipboard,
            meta_nonce,
            meta_ciphertext,
            payloads: vec![ObjectPayloadInit {
                id: payload_id_typed,
                nonce: payload_nonce,
                ciphertext_size: payload_size,
                sha256_ciphertext: payload_hash.clone(),
                inline_ciphertext: inline_ciphertext(&encrypted_payload),
            }],
            envelope,
        };
        let encrypted = encrypted_clipboard_from_init(&init_req, encrypted_payload.clone());

        let created_seq = self
            .submit_single_payload_object(
                &object_id,
                &payload_id,
                &init_req,
                encrypted_payload,
                payload_size,
                payload_hash,
            )
            .await?;

        let item = DecryptedClipboardItem {
            id: object_id.clone(),
            text: clipboard_display_text(mime_type, data),
            mime_type: mime_type.to_string(),
            payload_size: plaintext_size,
            created_at,
            source_device_id: device_id,
        };
        let visible = self
            .local_store
            .persist_local_clipboard_present_encrypted(
                &item,
                data,
                &encrypted,
                created_seq,
                created_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?;
        self.publish_visible_state(visible).await;
        info!(
            clipboard_id = %object_id,
            mime_type,
            bytes = data.len(),
            "Clipboard uploaded",
        );
        Ok(object_id)
    }

    async fn latest_clipboard_item_id_for_digest(
        &self,
        digest: &[u8; 32],
        encryption_key: &[u8; 32],
    ) -> Option<String> {
        let items = {
            let state = self.state.read().await;
            state.clipboard_items.clone()
        };
        for item in items {
            let Ok(Some(payload)) = self
                .local_store
                .clipboard_payload(&item.id, encryption_key)
                .await
            else {
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
        .ok_or_else(|| ClientError::ItemNotFound { id: id.to_string() })?;

        let encryption_key = self.current_encryption_key().await?;
        let bytes = self
            .local_store
            .clipboard_payload(id, &encryption_key)
            .await?
            .ok_or_else(|| ClientError::PayloadNotFound { id: id.to_string() })?;
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
        let item = {
            let state = self.state.read().await;
            state.clipboard_items.iter().find(|i| i.id == id).cloned()
        }
        .ok_or_else(|| ClientError::ItemNotFound { id: id.to_string() })?;

        if !is_text_mime_type(&item.mime_type) {
            return Err(ClientError::Unsupported(format!(
                "Clipboard item is {}; copying non-text clipboard payloads is not wired to the OS clipboard yet",
                item.mime_type
            )));
        };
        let encryption_key = self.current_encryption_key().await?;
        let bytes = self
            .local_store
            .clipboard_payload(id, &encryption_key)
            .await?
            .ok_or_else(|| ClientError::PayloadNotFound { id: id.to_string() })?;
        let text = String::from_utf8(bytes.clone())
            .map_err(|e| ClientError::Other(format!("clipboard text utf8: {e}")))?;

        *self.suppressed_payload.write().await = Some((
            clipboard_payload_digest(&item.mime_type, &bytes),
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
    ) -> Result<i64, ClientError> {
        let api = &self.api;
        let init_resp = api.object_init(init_req).await?;
        let payload_id_typed = payload_id
            .parse()
            .map_err(|source| ClientError::InvalidId {
                kind: "payload id",
                source,
            })?;
        let upload_urls = match init_resp {
            ObjectInitResponse::Complete { created_seq } => return Ok(created_seq),
            ObjectInitResponse::Pending { upload_urls } => upload_urls,
        };

        let upload_needed = upload_urls
            .iter()
            .any(|upload| upload.id == payload_id_typed);
        if !upload_needed && !upload_urls.is_empty() {
            return Err(ClientError::UnexpectedResponse(
                "object payload upload URL missing".into(),
            ));
        }

        if upload_needed {
            api.object_upload_payload(object_id, payload_id, encrypted_payload)
                .await?;
        }
        let complete_resp = api
            .object_complete(
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
        Ok(complete_resp.created_seq)
    }

    // ── Files ──

    #[cfg(not(target_family = "wasm"))]
    pub async fn upload_file(&self, file_path: &str) -> Result<String, ClientError> {
        self.upload_file_path(std::path::Path::new(file_path)).await
    }

    #[cfg(not(target_family = "wasm"))]
    pub async fn upload_file_path(&self, path: &std::path::Path) -> Result<String, ClientError> {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let data = tokio::fs::read(path)
            .await
            .map_err(|source| ClientError::Io {
                context: "read file",
                source,
            })?;
        self.upload_file_bytes(&filename, None, &data).await
    }

    #[cfg(target_family = "wasm")]
    pub async fn upload_file(&self, _file_path: &str) -> Result<String, ClientError> {
        Err(ClientError::Unsupported(
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

        let (device_id, device_id_typed, signing_key) =
            self.current_device_signing_context().await?;

        let meta = FileMeta {
            filename: filename.clone(),
            mime_type: mime_type.clone(),
            size: Some(data.len() as i64),
        };

        let file_uuid = uuid::Uuid::now_v7();
        let payload_uuid = uuid::Uuid::now_v7();
        let file_id = file_uuid.to_string();
        let payload_id = payload_uuid.to_string();
        let file_id_typed: ObjectId = file_uuid.into();
        let payload_id_typed: ObjectPayloadId = payload_uuid.into();
        let created_at = chrono::Utc::now().to_rfc3339();
        let aad_body = create_object_envelope_body_for_aad(
            file_id_typed,
            ObjectKind::File,
            device_id_typed,
            created_at.clone(),
            vec![payload_id_typed],
        );
        let (meta_nonce, meta_ciphertext, blob_nonce, encrypted_blob) = {
            let encryption_key = self.encryption_key.read().await;
            let encryption_key = encryption_key
                .as_ref()
                .ok_or(ClientError::NotAuthenticated)?;
            let (meta_nonce, meta_ciphertext) =
                encrypt_file_meta_bytes(&meta, encryption_key, &aad_body)?;
            let (blob_nonce, encrypted_blob) =
                encrypt_file_blob_bytes(data, encryption_key, &aad_body, payload_id_typed)?;
            (meta_nonce, meta_ciphertext, blob_nonce, encrypted_blob)
        };

        let blob_hash = crypto::sha256(&encrypted_blob).to_vec();
        let blob_size = encrypted_blob.len() as i64;
        let envelope_body = create_object_envelope_body(
            file_id_typed,
            ObjectKind::File,
            device_id_typed,
            created_at.clone(),
            meta_nonce.clone(),
            crypto::sha256(&meta_ciphertext).to_vec(),
            vec![ObjectEnvelopePayloadV1 {
                id: payload_id_typed,
                nonce: blob_nonce.clone(),
                ciphertext_size: blob_size,
                sha256_ciphertext: blob_hash.clone(),
            }],
        );
        let envelope = ObjectEnvelopeV1 {
            signature: crypto::sign_object_envelope_body(&signing_key, &envelope_body)?,
            body: envelope_body,
        };

        let init_req = ObjectInitRequest {
            id: file_id_typed,
            kind: ObjectKind::File,
            meta_nonce,
            meta_ciphertext,
            payloads: vec![ObjectPayloadInit {
                id: payload_id_typed,
                nonce: blob_nonce,
                ciphertext_size: blob_size,
                sha256_ciphertext: blob_hash.clone(),
                inline_ciphertext: inline_ciphertext(&encrypted_blob),
            }],
            envelope,
        };
        let encrypted = encrypted_object_from_init(&init_req);

        let created_seq = self
            .submit_single_payload_object(
                &file_id,
                &payload_id,
                &init_req,
                encrypted_blob,
                blob_size,
                blob_hash,
            )
            .await?;

        let item = DecryptedFileItem {
            id: file_id.clone(),
            filename: filename.clone(),
            mime_type: mime_type.clone(),
            blob_size: data.len() as i64,
            created_at,
            source_device_id: device_id,
        };
        let visible = self
            .local_store
            .persist_local_file_present_encrypted(
                &item,
                &encrypted,
                created_seq,
                created_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?;
        self.publish_visible_state(visible).await;
        info!(file_id = %file_id, filename = %filename, "File uploaded");
        Ok(file_id)
    }

    pub async fn download_file_bytes(&self, file_id: &str) -> Result<Vec<u8>, ClientError> {
        let (file_item, payload, encrypted_blob) = {
            let api = &self.api;
            let file_item = api.get_object(file_id).await?;
            verify_object_list_item_envelope(&file_item)?;
            if file_item.kind != ObjectKind::File {
                return Err(ClientError::UnexpectedObjectKind {
                    expected: ObjectKind::File,
                    actual: file_item.kind,
                });
            }
            let payload = single_payload(&file_item)?.clone();
            check_payload_ciphertext_size(&payload, MAX_FILE_PAYLOAD_CIPHERTEXT_BYTES)?;
            let blob = api
                .download_object_payload(file_id, &payload.id.to_string(), payload.ciphertext_size)
                .await?;
            verify_payload_hash(&payload, &blob)?;
            (file_item, payload, blob)
        };

        let plaintext = {
            let encryption_key = self.encryption_key.read().await;
            let encryption_key = encryption_key
                .as_ref()
                .ok_or(ClientError::NotAuthenticated)?;
            decrypt_file_blob_bytes(
                &payload.nonce,
                &encrypted_blob,
                encryption_key,
                &file_item.envelope.body,
                payload.id,
            )?
        };
        info!(file_id = %file_id, "File downloaded");
        Ok(plaintext)
    }

    #[cfg(not(target_family = "wasm"))]
    pub async fn download_file(&self, file_id: &str, target_path: &str) -> Result<(), ClientError> {
        self.download_file_path(file_id, std::path::Path::new(target_path))
            .await
    }

    #[cfg(not(target_family = "wasm"))]
    pub async fn download_file_path(
        &self,
        file_id: &str,
        target_path: &std::path::Path,
    ) -> Result<(), ClientError> {
        let plaintext = self.download_file_bytes(file_id).await?;
        tokio::fs::write(target_path, &plaintext)
            .await
            .map_err(|source| ClientError::Io {
                context: "write file",
                source,
            })?;

        info!(file_id = %file_id, path = %target_path.display(), "File downloaded");
        Ok(())
    }

    #[cfg(target_family = "wasm")]
    pub async fn download_file(
        &self,
        _file_id: &str,
        _target_path: &str,
    ) -> Result<(), ClientError> {
        Err(ClientError::Unsupported(
            "Path-based file download is not available on web".into(),
        ))
    }

    pub async fn delete_file(&self, file_id: &str) -> Result<(), ClientError> {
        let delete_resp = {
            let api = &self.api;
            api.delete_object(file_id).await?
        };
        let visible = self
            .local_store
            .apply_local_delete(
                ObjectKind::File,
                file_id,
                delete_resp.deleted_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?;
        self.publish_visible_state(visible).await;
        info!(file_id = %file_id, "File deleted");
        Ok(())
    }

    // ── Sync ──

    pub async fn refresh(&self) -> Result<(), ClientError> {
        _ = self.ws_restart_tx.send(*self.ws_restart_tx.borrow() + 1);
        Ok(())
    }

    async fn publish_visible_state(&self, visible: LocalVisibleState) {
        {
            let mut state = self.state.write().await;
            state.clipboard_items = visible.clipboard_items;
            state.files = visible.files;
        }
        self.bump_version();
    }

    async fn start_reconciliation(self: &Arc<Self>, generation: u64, stream_start_seq: i64) {
        let file_engine = Arc::clone(self);
        spawn_background(async move {
            if let Err(error) = file_engine
                .snapshot_files(generation, stream_start_seq)
                .await
            {
                warn!("File snapshot failed: {}", error);
            }
        });

        let clipboard_engine = Arc::clone(self);
        spawn_background(async move {
            if let Err(error) = clipboard_engine
                .snapshot_clipboard(generation, stream_start_seq)
                .await
            {
                warn!("Clipboard snapshot failed: {}", error);
            }
        });
    }

    async fn handle_ws_text(
        self: &Arc<Self>,
        text: &str,
        generation: u64,
    ) -> Result<bool, ClientError> {
        match serde_json::from_str::<WsServerMessage>(text) {
            Ok(WsServerMessage::HelloAck { .. }) => {
                debug!("Ignoring duplicate WS hello_ack");
            }
            Ok(WsServerMessage::Event {
                seq,
                event_type,
                object_kind,
                object_id,
                ..
            }) => {
                debug!("WS event seq={} type={}", seq, event_type);
                match event_type {
                    ObjectEventType::Created => {
                        self.handle_created_event(generation, object_kind, object_id, seq)
                            .await?;
                    }
                    ObjectEventType::Deleted if object_kind == ObjectKind::File => {
                        self.handle_deleted_event(generation, ObjectKind::File, object_id, seq)
                            .await?;
                    }
                    ObjectEventType::Deleted => {
                        warn!(
                            seq,
                            object_kind = %object_kind,
                            "Ignoring unsupported WS delete event for object kind",
                        );
                    }
                }
            }
            Ok(WsServerMessage::Invalidate { .. }) => {
                info!("WS invalidate requested reconnect");
                return Ok(false);
            }
            Ok(WsServerMessage::Error { error }) => {
                warn!("Server rejected WS connection: {error}");
                return Ok(false);
            }
            Err(e) => {
                warn!("Failed to parse WS message: {}", e);
            }
        }
        Ok(true)
    }

    async fn snapshot_files(
        self: &Arc<Self>,
        generation: u64,
        stream_start_seq: i64,
    ) -> Result<(), ClientError> {
        let api = &self.api;
        let encryption_key = self.current_encryption_key().await?;
        let mut after = None;
        loop {
            let page = api
                .list_objects(
                    Some(ObjectKind::File),
                    Some(100),
                    Some(stream_start_seq),
                    after,
                )
                .await?;
            for item in page.items {
                match decrypt_file_object_item(&item, &encryption_key) {
                    Ok(file) => {
                        self.persist_file_snapshot_item(
                            &file,
                            &encrypted_object_from_list_item(&item),
                            item.created_seq,
                            generation,
                        )
                        .await?;
                    }
                    Err(e) => warn!(id = %item.id, "Failed to decrypt file object: {}", e),
                }
            }
            match page.next_after {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }

        if let Some(visible) = self
            .local_store
            .sweep_kind(
                ObjectKind::File,
                generation,
                stream_start_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?
        {
            self.publish_visible_state(visible).await;
        }
        Ok(())
    }

    async fn snapshot_clipboard(
        self: &Arc<Self>,
        generation: u64,
        stream_start_seq: i64,
    ) -> Result<(), ClientError> {
        let api = &self.api;
        let encryption_key = self.current_encryption_key().await?;
        // Capture a shared borrow so the per-item `async move` blocks copy the
        // reference, not the key material.
        let encryption_key = &encryption_key;
        let mut after = None;
        loop {
            let page = api
                .list_objects(
                    Some(ObjectKind::Clipboard),
                    Some(100),
                    Some(stream_start_seq),
                    after,
                )
                .await?;
            let mut objects = stream::iter(page.items)
                .map(|item| async move {
                    let created_seq = item.created_seq;
                    match self
                        .decrypt_clipboard_object_item_with_api(api, &item, encryption_key)
                        .await
                    {
                        Ok(object) => Some((object, created_seq)),
                        Err(e) => {
                            warn!(id = %item.id, "Failed to load clipboard object: {}", e);
                            None
                        }
                    }
                })
                .buffer_unordered(CLIPBOARD_HYDRATION_CONCURRENCY)
                .filter_map(std::future::ready);

            while let Some((object, created_seq)) = objects.next().await {
                self.persist_clipboard_snapshot_item(&object, created_seq, generation)
                    .await?;
            }

            match page.next_after {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }

        if let Some(visible) = self
            .local_store
            .sweep_kind(
                ObjectKind::Clipboard,
                generation,
                stream_start_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?
        {
            self.publish_visible_state(visible).await;
        }
        Ok(())
    }

    async fn persist_file_snapshot_item(
        &self,
        file: &DecryptedFileItem,
        encrypted: &EncryptedObject,
        created_seq: i64,
        generation: u64,
    ) -> Result<(), ClientError> {
        if let Some(visible) = self
            .local_store
            .persist_snapshot_file_present_encrypted(
                file,
                encrypted,
                created_seq,
                generation,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?
        {
            self.publish_visible_state(visible).await;
        }
        Ok(())
    }

    async fn persist_clipboard_snapshot_item(
        &self,
        object: &DecryptedClipboardObject,
        created_seq: i64,
        generation: u64,
    ) -> Result<(), ClientError> {
        if let Some(visible) = self
            .local_store
            .persist_snapshot_clipboard_present_encrypted(
                &object.item,
                &object.payload,
                &object.encrypted,
                created_seq,
                generation,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?
        {
            self.publish_visible_state(visible).await;
        }
        Ok(())
    }

    /// Returns the key inside a zeroizing wrapper so per-call copies are wiped
    /// on drop; callers borrow `&[u8; 32]` from it via deref.
    async fn current_encryption_key(&self) -> Result<Zeroizing<[u8; 32]>, ClientError> {
        let encryption_key = self.encryption_key.read().await;
        Ok(encryption_key
            .as_ref()
            .ok_or(ClientError::NotAuthenticated)?
            .clone())
    }

    async fn decrypt_clipboard_object_item_with_api(
        &self,
        api: &ApiClient,
        item: &ObjectListItem,
        encryption_key: &[u8; 32],
    ) -> Result<DecryptedClipboardObject, ClientError> {
        verify_object_list_item_envelope(item)?;
        let meta = decrypt_clipboard_meta(
            &item.meta_nonce,
            &item.meta_ciphertext,
            encryption_key,
            &item.envelope.body,
        )?;
        if !is_supported_clipboard_mime_type(&meta.mime_type) {
            return Err(ClientError::UnsupportedMimeType {
                mime_type: meta.mime_type,
            });
        }
        let payload = single_payload(item)?;
        check_payload_ciphertext_size(payload, MAX_CLIPBOARD_PAYLOAD_CIPHERTEXT_BYTES)?;
        let payload_size = meta.size.unwrap_or(payload.ciphertext_size);
        let encrypted_payload = api
            .download_object_payload(
                &item.id.to_string(),
                &payload.id.to_string(),
                payload.ciphertext_size,
            )
            .await?;
        verify_payload_hash(payload, &encrypted_payload)?;
        let plaintext = decrypt_clipboard_payload(
            &payload.nonce,
            &encrypted_payload,
            encryption_key,
            &item.envelope.body,
            payload.id,
        )?;
        let text = clipboard_display_text(&meta.mime_type, &plaintext);

        Ok(DecryptedClipboardObject {
            encrypted: EncryptedClipboardObject {
                object: encrypted_object_from_list_item(item),
                payload_ciphertext: encrypted_payload,
            },
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

    async fn handle_created_event(
        self: &Arc<Self>,
        generation: u64,
        kind: ObjectKind,
        object_id: ObjectId,
        event_seq: i64,
    ) -> Result<(), ClientError> {
        let object_id_text = object_id.to_string();
        let should_materialize = self
            .local_store
            .mark_pending_create(kind, &object_id_text, event_seq, generation)
            .await?;

        if should_materialize {
            let engine = Arc::clone(self);
            spawn_background(async move {
                if let Err(error) = engine
                    .materialize_object(generation, kind, object_id, event_seq)
                    .await
                {
                    warn!(
                        object_id = %object_id,
                        event_seq,
                        "Failed to materialize live object: {}",
                        error,
                    );
                }
            });
        }

        Ok(())
    }

    async fn handle_deleted_event(
        &self,
        generation: u64,
        kind: ObjectKind,
        object_id: ObjectId,
        event_seq: i64,
    ) -> Result<(), ClientError> {
        if let Some(visible) = self
            .local_store
            .apply_live_delete(
                kind,
                &object_id.to_string(),
                event_seq,
                generation,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?
        {
            self.publish_visible_state(visible).await;
        }
        Ok(())
    }

    async fn materialize_object(
        self: &Arc<Self>,
        generation: u64,
        kind: ObjectKind,
        object_id: ObjectId,
        event_seq: i64,
    ) -> Result<(), ClientError> {
        // Collab objects are server-visible documents, not end-to-end-encrypted
        // objects: they are never returned by the encrypted-object endpoints and
        // are not materialized into the local object store. Wiring collab into
        // the sync engine / app state is Phase 2 UI work tracked separately; here
        // we just skip them so the match stays total.
        if kind == ObjectKind::Collab {
            debug!(object_id = %object_id, event_seq, "Skipping materialize for collab object");
            return Ok(());
        }

        let api = &self.api;
        let object_id_text = object_id.to_string();
        let item = match api.get_object(&object_id_text).await {
            Ok(item) => item,
            Err(error) if is_not_found_error(&error) => {
                self.remove_absent_object(generation, &object_id_text)
                    .await?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };

        if item.id != object_id || item.kind != kind {
            return Err(ClientError::UnexpectedResponse(format!(
                "materialized object {object_id} returned mismatched identity"
            )));
        }
        if item.created_seq != event_seq {
            debug!(
                object_id = %object_id,
                event_seq,
                created_seq = item.created_seq,
                "Live create event seq differed from object created_seq",
            );
        }

        let encryption_key = self.current_encryption_key().await?;
        match kind {
            ObjectKind::Clipboard => {
                let object = self
                    .decrypt_clipboard_object_item_with_api(api, &item, &encryption_key)
                    .await?;
                self.persist_clipboard_snapshot_item(&object, item.created_seq, generation)
                    .await?;
            }
            ObjectKind::File => {
                let file = decrypt_file_object_item(&item, &encryption_key)?;
                self.persist_file_snapshot_item(
                    &file,
                    &encrypted_object_from_list_item(&item),
                    item.created_seq,
                    generation,
                )
                .await?;
            }
            // Skipped above before any network call; an explicit arm keeps the
            // match total without re-handling it.
            ObjectKind::Collab => {}
        }
        Ok(())
    }

    async fn remove_absent_object(
        &self,
        generation: u64,
        object_id: &str,
    ) -> Result<(), ClientError> {
        if let Some(visible) = self
            .local_store
            .remove_absent_object(object_id, generation, RECENT_CLIPBOARD_LIMIT)
            .await?
        {
            self.publish_visible_state(visible).await;
        }
        Ok(())
    }

    // ── WebSocket ──

    #[cfg(not(target_family = "wasm"))]
    async fn ws_loop(self: &Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        loop {
            {
                let state = self.state.read().await;
                if !state.is_logged_in() {
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
                if !state.is_logged_in() {
                    return;
                }
            }

            let jitter = Duration::from_millis(rand::random_range(0..1000));
            tokio::time::sleep(backoff + jitter).await;
            backoff = (backoff * 2).min(max_backoff);
        }
    }

    #[cfg(not(target_family = "wasm"))]
    async fn ws_connect(self: &Arc<Self>) -> Result<(), ClientError> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite;

        let (token, ws_url, host) = {
            let api = &self.api;
            let t = api
                .token()
                .ok_or(ClientError::NotAuthenticated)?
                .to_string();
            let ws_url = api.websocket_url()?;
            let host = api.base_url().host_str().unwrap_or("localhost").to_string();
            (t, ws_url, host)
        };

        let request = tungstenite::http::Request::builder()
            .uri(ws_url.as_str())
            .header("Authorization", format!("Bearer {}", token))
            .header("Host", host)
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

        let hello = WsClientMessage::Hello;
        let hello_json =
            serde_json::to_string(&hello).map_err(|e| ClientError::WebSocket(e.to_string()))?;
        write
            .send(tungstenite::Message::Text(hello_json.into()))
            .await
            .map_err(|e: tungstenite::Error| ClientError::WebSocket(e.to_string()))?;

        let stream_start_seq = loop {
            let msg = read
                .next()
                .await
                .ok_or_else(|| ClientError::WebSocket("closed before hello_ack".into()))?
                .map_err(|e: tungstenite::Error| ClientError::WebSocket(e.to_string()))?;
            match msg {
                tungstenite::Message::Text(text) => {
                    match serde_json::from_str::<WsServerMessage>(&text) {
                        Ok(WsServerMessage::HelloAck {
                            stream_start_seq, ..
                        }) => break stream_start_seq,
                        Ok(WsServerMessage::Error { error }) => {
                            return Err(ClientError::WebSocket(error.to_string()));
                        }
                        Ok(other) => {
                            debug!("Ignoring WS message before hello_ack: {:?}", other);
                        }
                        Err(e) => {
                            return Err(ClientError::WebSocket(format!(
                                "failed to parse hello_ack: {e}"
                            )));
                        }
                    }
                }
                tungstenite::Message::Ping(data) => {
                    _ = write.send(tungstenite::Message::Pong(data)).await;
                }
                tungstenite::Message::Close(_) => {
                    return Err(ClientError::WebSocket("closed before hello_ack".into()));
                }
                _ => {}
            }
        };

        let generation = self.local_store.start_generation().await;
        self.start_reconciliation(generation, stream_start_seq)
            .await;

        {
            let mut state = self.state.write().await;
            state.connection_status = ConnectionStatus::Connected;
        }
        self.bump_version();
        info!(
            stream_start_seq,
            generation, "WebSocket connected and reconciliation started"
        );

        let mut restart_rx = self.ws_restart_rx.clone();
        loop {
            tokio::select! {
                changed = restart_rx.changed() => {
                    if changed.is_ok() {
                        info!("WebSocket reconnect requested");
                    }
                    break;
                }
                msg_result = read.next() => {
                    let Some(msg_result) = msg_result else {
                        break;
                    };
                    let msg: tungstenite::Message = msg_result
                        .map_err(|e: tungstenite::Error| ClientError::WebSocket(e.to_string()))?;

                    match msg {
                        tungstenite::Message::Text(text) => {
                            if !self.handle_ws_text(&text, generation).await? {
                                break;
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
            }
        }

        Ok(())
    }

    #[cfg(target_family = "wasm")]
    async fn ws_loop(self: &Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        loop {
            {
                let state = self.state.read().await;
                if !state.is_logged_in() {
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
                if !state.is_logged_in() {
                    return;
                }
            }

            gloo_timers::future::sleep(backoff).await;
            backoff = (backoff * 2).min(max_backoff);
        }
    }

    #[cfg(target_family = "wasm")]
    async fn ws_connect(self: &Arc<Self>) -> Result<(), ClientError> {
        let api = &self.api;
        let ticket = api.websocket_ticket().await?;
        let ws_url = api.websocket_ticket_url()?;

        let mut ws = BrowserWs::connect(ws_url.as_str(), &ticket.ticket).await?;

        let hello = WsClientMessage::Hello;
        let hello_json =
            serde_json::to_string(&hello).map_err(|e| ClientError::WebSocket(e.to_string()))?;
        ws.send_text(&hello_json)?;

        let stream_start_seq = loop {
            let text = ws
                .next_text()
                .await?
                .ok_or_else(|| ClientError::WebSocket("closed before hello_ack".into()))?;
            match serde_json::from_str::<WsServerMessage>(&text) {
                Ok(WsServerMessage::HelloAck {
                    stream_start_seq, ..
                }) => break stream_start_seq,
                Ok(WsServerMessage::Error { error }) => {
                    return Err(ClientError::WebSocket(error.to_string()));
                }
                Ok(other) => {
                    debug!("Ignoring WS message before hello_ack: {:?}", other);
                }
                Err(e) => {
                    return Err(ClientError::WebSocket(format!(
                        "failed to parse hello_ack: {e}"
                    )));
                }
            }
        };

        let generation = self.local_store.start_generation().await;
        self.start_reconciliation(generation, stream_start_seq)
            .await;

        {
            let mut state = self.state.write().await;
            state.connection_status = ConnectionStatus::Connected;
        }
        self.bump_version();
        info!(
            stream_start_seq,
            generation, "WebSocket connected and reconciliation started"
        );

        let mut restart_rx = self.ws_restart_rx.clone();
        loop {
            tokio::select! {
                changed = restart_rx.changed() => {
                    if changed.is_ok() {
                        info!("WebSocket reconnect requested");
                    }
                    break;
                }
                msg = ws.next_text() => {
                    let Some(text) = msg? else {
                        info!("WebSocket closed by server");
                        break;
                    };
                    if !self.handle_ws_text(&text, generation).await? {
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(target_family = "wasm")]
enum BrowserWsMessage {
    Open,
    Text(String),
    Error(String),
    Close(String),
}

#[cfg(target_family = "wasm")]
struct BrowserWs {
    socket: web_sys::WebSocket,
    rx: tokio::sync::mpsc::UnboundedReceiver<BrowserWsMessage>,
    _onopen: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>,
    _onmessage: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MessageEvent)>,
    _onerror: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>,
    _onclose: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::CloseEvent)>,
}

#[cfg(target_family = "wasm")]
impl BrowserWs {
    async fn connect(url: &str, ticket: &str) -> Result<Self, ClientError> {
        use wasm_bindgen::JsCast;

        let protocols = js_sys::Array::new();
        protocols.push(&wasm_bindgen::JsValue::from_str(WS_TICKET_PROTOCOL));
        protocols.push(&wasm_bindgen::JsValue::from_str(ticket));
        let socket = web_sys::WebSocket::new_with_str_sequence(url, protocols.as_ref())
            .map_err(|error| ClientError::WebSocket(js_error_message(error)))?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let onopen = {
            let tx = tx.clone();
            wasm_bindgen::closure::Closure::wrap(Box::new(move |_event: web_sys::Event| {
                _ = tx.send(BrowserWsMessage::Open);
            }) as Box<dyn FnMut(web_sys::Event)>)
        };
        socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));

        let onmessage = {
            let tx = tx.clone();
            wasm_bindgen::closure::Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                if let Some(text) = event.data().as_string() {
                    _ = tx.send(BrowserWsMessage::Text(text));
                }
            })
                as Box<dyn FnMut(web_sys::MessageEvent)>)
        };
        socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

        let onerror = {
            let tx = tx.clone();
            wasm_bindgen::closure::Closure::wrap(Box::new(move |_event: web_sys::Event| {
                _ = tx.send(BrowserWsMessage::Error("browser WebSocket error".into()));
            }) as Box<dyn FnMut(web_sys::Event)>)
        };
        socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));

        let onclose = {
            let tx = tx.clone();
            wasm_bindgen::closure::Closure::wrap(Box::new(move |event: web_sys::CloseEvent| {
                let reason = if event.reason().is_empty() {
                    format!("closed with code {}", event.code())
                } else {
                    format!("closed with code {}: {}", event.code(), event.reason())
                };
                _ = tx.send(BrowserWsMessage::Close(reason));
            })
                as Box<dyn FnMut(web_sys::CloseEvent)>)
        };
        socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));

        let mut ws = Self {
            socket,
            rx,
            _onopen: onopen,
            _onmessage: onmessage,
            _onerror: onerror,
            _onclose: onclose,
        };

        loop {
            match ws.rx.recv().await {
                Some(BrowserWsMessage::Open) => return Ok(ws),
                Some(BrowserWsMessage::Text(_)) => {}
                Some(BrowserWsMessage::Error(error)) => {
                    return Err(ClientError::WebSocket(error));
                }
                Some(BrowserWsMessage::Close(reason)) => {
                    return Err(ClientError::WebSocket(reason));
                }
                None => return Err(ClientError::WebSocket("WebSocket closed".into())),
            }
        }
    }

    fn send_text(&self, text: &str) -> Result<(), ClientError> {
        self.socket
            .send_with_str(text)
            .map_err(|error| ClientError::WebSocket(js_error_message(error)))
    }

    async fn next_text(&mut self) -> Result<Option<String>, ClientError> {
        loop {
            match self.rx.recv().await {
                Some(BrowserWsMessage::Open) => {}
                Some(BrowserWsMessage::Text(text)) => return Ok(Some(text)),
                Some(BrowserWsMessage::Error(error)) => {
                    return Err(ClientError::WebSocket(error));
                }
                Some(BrowserWsMessage::Close(_reason)) => return Ok(None),
                None => return Ok(None),
            }
        }
    }
}

#[cfg(target_family = "wasm")]
fn js_error_message(error: wasm_bindgen::JsValue) -> String {
    error
        .as_string()
        .unwrap_or_else(|| "browser WebSocket operation failed".into())
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

/// Reject a server-declared payload ciphertext size that exceeds the client's
/// independent ceiling *before* any bytes are downloaded/buffered. The server
/// is untrusted for content and fully controls `ciphertext_size` (it supplies
/// the signing key the envelope is verified against), so this bound must not
/// rely on the envelope. Defends against download-side OOM (finding: malicious
/// server can OOM the client via unbounded payload download).
fn check_payload_ciphertext_size(
    payload: &ObjectPayloadDescriptor,
    limit: i64,
) -> Result<(), ClientError> {
    if payload.ciphertext_size > limit {
        return Err(ClientError::PayloadTooLarge {
            size: payload.ciphertext_size,
            limit,
        });
    }
    Ok(())
}

fn single_payload(item: &ObjectListItem) -> Result<&ObjectPayloadDescriptor, ClientError> {
    if item.payloads.len() != 1 {
        return Err(ClientError::UnexpectedResponse(format!(
            "object {} has {} payloads; exactly one is supported by this client",
            item.id,
            item.payloads.len()
        )));
    }

    Ok(&item.payloads[0])
}

fn encrypted_clipboard_from_init(
    init_req: &ObjectInitRequest,
    payload_ciphertext: Vec<u8>,
) -> EncryptedClipboardObject {
    EncryptedClipboardObject {
        object: encrypted_object_from_init(init_req),
        payload_ciphertext,
    }
}

fn encrypted_object_from_init(init_req: &ObjectInitRequest) -> EncryptedObject {
    EncryptedObject {
        meta_nonce: init_req.meta_nonce.clone(),
        meta_ciphertext: init_req.meta_ciphertext.clone(),
        payloads: init_req
            .payloads
            .iter()
            .map(|payload| ObjectPayloadDescriptor {
                id: payload.id,
                nonce: payload.nonce.clone(),
                ciphertext_size: payload.ciphertext_size,
                sha256_ciphertext: payload.sha256_ciphertext.clone(),
            })
            .collect(),
        created_at: init_req.envelope.body.created_at.clone(),
        source_device_id: init_req.envelope.body.source_device_id.to_string(),
        envelope: init_req.envelope.clone(),
    }
}

fn encrypted_object_from_list_item(item: &ObjectListItem) -> EncryptedObject {
    EncryptedObject {
        meta_nonce: item.meta_nonce.clone(),
        meta_ciphertext: item.meta_ciphertext.clone(),
        payloads: item.payloads.clone(),
        created_at: item.created_at.clone(),
        source_device_id: item.source_device_id.to_string(),
        envelope: item.envelope.clone(),
    }
}

fn optional_device_id(device_id: Option<&str>) -> Result<Option<DeviceId>, ClientError> {
    device_id
        .map(|device_id| {
            device_id.parse().map_err(|source| ClientError::InvalidId {
                kind: "saved device id",
                source,
            })
        })
        .transpose()
}

fn create_object_envelope_body_for_aad(
    object_id: ObjectId,
    kind: ObjectKind,
    source_device_id: DeviceId,
    created_at: String,
    payload_ids: Vec<ObjectPayloadId>,
) -> ObjectEnvelopeBodyV1 {
    create_object_envelope_body(
        object_id,
        kind,
        source_device_id,
        created_at,
        Vec::new(),
        Vec::new(),
        payload_ids
            .into_iter()
            .map(|id| ObjectEnvelopePayloadV1 {
                id,
                nonce: Vec::new(),
                ciphertext_size: 0,
                sha256_ciphertext: Vec::new(),
            })
            .collect(),
    )
}

fn create_object_envelope_body(
    object_id: ObjectId,
    kind: ObjectKind,
    source_device_id: DeviceId,
    created_at: String,
    meta_nonce: Vec<u8>,
    sha256_meta_ciphertext: Vec<u8>,
    payloads: Vec<ObjectEnvelopePayloadV1>,
) -> ObjectEnvelopeBodyV1 {
    ObjectEnvelopeBodyV1 {
        object_id,
        object_type: kind,
        object_version: OBJECT_ENVELOPE_VERSION_V1,
        source_device_id,
        created_at,
        operation: ObjectEnvelopeOperation::Create,
        meta_nonce,
        sha256_meta_ciphertext,
        payloads,
    }
}

fn verify_object_list_item_envelope(item: &ObjectListItem) -> Result<(), ClientError> {
    let body = &item.envelope.body;
    // Reject an over-count payload list up front, before the per-payload
    // matching loop below: the server is untrusted and never runs the
    // api-types `length(max = MAX_OBJECT_PAYLOAD_ENTRIES)` validator on its
    // responses, so without this an attacker-supplied item with a huge matched
    // payload set would cost O(n^2) UUID comparisons (and a wasted signature
    // verify) before `single_payload` rejects it. Clients only ever produce a
    // single payload, so 16 is already far more than this client uses.
    if item.payloads.len() > MAX_OBJECT_PAYLOAD_ENTRIES
        || body.payloads.len() > MAX_OBJECT_PAYLOAD_ENTRIES
    {
        return Err(object_envelope_error("object has too many payload entries"));
    }
    let meta_hash = crypto::sha256(&item.meta_ciphertext);
    if body.object_id != item.id
        || body.object_type != item.kind
        || body.object_version != OBJECT_ENVELOPE_VERSION_V1
        || body.operation != ObjectEnvelopeOperation::Create
        || body.source_device_id != item.source_device_id
        || body.created_at != item.created_at
        || body.meta_nonce != item.meta_nonce
        || body.sha256_meta_ciphertext.as_slice() != meta_hash.as_slice()
    {
        return Err(object_envelope_error(
            "object envelope does not match list item",
        ));
    }

    if body.payloads.len() != item.payloads.len() {
        return Err(object_envelope_error(
            "object envelope payload set does not match list item",
        ));
    }
    for envelope_payload in &body.payloads {
        let Some(payload) = item
            .payloads
            .iter()
            .find(|payload| payload.id == envelope_payload.id)
        else {
            return Err(object_envelope_error(
                "object envelope references unknown payload",
            ));
        };
        if envelope_payload.nonce != payload.nonce
            || envelope_payload.ciphertext_size != payload.ciphertext_size
            || envelope_payload.sha256_ciphertext != payload.sha256_ciphertext
        {
            return Err(object_envelope_error(
                "object envelope payload metadata mismatch",
            ));
        }
    }

    // When the source device has been reclaimed the server cannot supply its
    // signing key, so the Ed25519 provenance check is unavailable. The envelope
    // is already cross-checked against the item above, and the export-key AEAD
    // AAD (verified at decrypt time) is the real authenticity mechanism, so an
    // absent key downgrades provenance verification rather than rejecting the
    // object.
    match &item.source_device_signing_public_key {
        Some(public_key) => crypto::verify_object_envelope_signature(public_key, &item.envelope)
            .map_err(ClientError::from),
        None => Ok(()),
    }
}

fn verify_payload_hash(
    payload: &ObjectPayloadDescriptor,
    ciphertext: &[u8],
) -> Result<(), ClientError> {
    let payload_hash = crypto::sha256(ciphertext);
    if payload.sha256_ciphertext.as_slice() != payload_hash.as_slice() {
        return Err(object_envelope_error(
            "downloaded payload hash does not match object envelope",
        ));
    }
    Ok(())
}

fn object_envelope_error(message: impl Into<String>) -> ClientError {
    ClientError::Crypto(crypto::CryptoError::Signature(message.into()))
}

fn decrypt_file_object_item(
    item: &ObjectListItem,
    encryption_key: &[u8; 32],
) -> Result<DecryptedFileItem, ClientError> {
    verify_object_list_item_envelope(item)?;
    let meta = decrypt_file_meta_bytes(
        &item.meta_nonce,
        &item.meta_ciphertext,
        encryption_key,
        &item.envelope.body,
    )?;
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

fn is_not_found_error(error: &ClientError) -> bool {
    matches!(error, ClientError::Api { status, .. } if *status == 404)
}

#[cfg(not(target_family = "wasm"))]
fn spawn_background<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(future);
}

#[cfg(target_family = "wasm")]
fn spawn_background<F>(future: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(id: ObjectPayloadId) -> ObjectPayloadDescriptor {
        ObjectPayloadDescriptor {
            id,
            nonce: vec![0_u8; crypto::XCHACHA20_NONCE_BYTES],
            ciphertext_size: 0,
            sha256_ciphertext: vec![0_u8; crypto::SHA256_BYTES],
        }
    }

    fn envelope_payload(id: ObjectPayloadId) -> ObjectEnvelopePayloadV1 {
        ObjectEnvelopePayloadV1 {
            id,
            nonce: vec![0_u8; crypto::XCHACHA20_NONCE_BYTES],
            ciphertext_size: 0,
            sha256_ciphertext: vec![0_u8; crypto::SHA256_BYTES],
        }
    }

    /// Build a properly self-signed object list item whose envelope/item
    /// payload sets both have `count` entries. The signature is valid, so a
    /// rejection can only come from the over-count guard, and a legitimately
    /// sized list verifies cleanly.
    fn signed_item_with_payload_count(count: usize) -> ObjectListItem {
        let object_id: ObjectId = uuid::Uuid::now_v7().into();
        let device_id: DeviceId = uuid::Uuid::now_v7().into();
        let signing_key = crypto::generate_device_signing_secret_key();
        let public_key = crypto::device_signing_public_key(&signing_key);
        let payload_ids: Vec<ObjectPayloadId> =
            (0..count).map(|_| uuid::Uuid::now_v7().into()).collect();
        let body = ObjectEnvelopeBodyV1 {
            object_id,
            object_type: ObjectKind::Clipboard,
            object_version: OBJECT_ENVELOPE_VERSION_V1,
            source_device_id: device_id,
            created_at: "2026-06-13T00:00:00Z".into(),
            operation: ObjectEnvelopeOperation::Create,
            meta_nonce: vec![0_u8; crypto::XCHACHA20_NONCE_BYTES],
            sha256_meta_ciphertext: crypto::sha256(&[]).to_vec(),
            payloads: payload_ids.iter().copied().map(envelope_payload).collect(),
        };
        let signature = crypto::sign_object_envelope_body(&signing_key, &body).expect("sign");
        ObjectListItem {
            id: object_id,
            kind: ObjectKind::Clipboard,
            created_seq: 1,
            meta_nonce: vec![0_u8; crypto::XCHACHA20_NONCE_BYTES],
            meta_ciphertext: Vec::new(),
            payloads: payload_ids.iter().copied().map(descriptor).collect(),
            created_at: "2026-06-13T00:00:00Z".into(),
            source_device_id: device_id,
            source_device_signing_public_key: Some(public_key.to_vec()),
            envelope: ObjectEnvelopeV1 { body, signature },
        }
    }

    #[test]
    fn envelope_verification_rejects_over_count_payload_list_before_quadratic_work() {
        // A malicious server can pack a huge matched payload set into one item;
        // verifying it without an early cap is O(n^2). The cap must reject it up
        // front. The item is validly signed, so the rejection can ONLY be the
        // over-count guard firing ahead of the per-payload loop / Ed25519 verify.
        let item = signed_item_with_payload_count(MAX_OBJECT_PAYLOAD_ENTRIES + 1);
        let error = verify_object_list_item_envelope(&item).expect_err("must be rejected");
        match error {
            ClientError::Crypto(crypto::CryptoError::Signature(message)) => {
                assert!(
                    message.contains("too many payload entries"),
                    "expected over-count rejection, got: {message}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn envelope_verification_allows_max_payload_count_past_the_cap() {
        // At the cap the over-count guard must NOT fire: a legitimately sized,
        // validly signed list must still verify cleanly, proving the guard is a
        // ceiling and not an off-by-one that rejects valid lists.
        let item = signed_item_with_payload_count(MAX_OBJECT_PAYLOAD_ENTRIES);
        verify_object_list_item_envelope(&item).expect("payload count at the cap must verify");
    }

    #[test]
    fn envelope_verification_accepts_reclaimed_source_device_without_key() {
        // When the source device has been reclaimed the server cannot return its
        // signing key, so provenance is unverifiable. The object must still be
        // accepted (the AEAD AAD is the real authenticity mechanism); only the
        // Ed25519 provenance check is skipped. A tampered signature with no key
        // present must therefore NOT cause rejection here.
        let mut item = signed_item_with_payload_count(1);
        item.source_device_signing_public_key = None;
        if let Some(last) = item.envelope.signature.last_mut() {
            *last ^= 0x01;
        }
        verify_object_list_item_envelope(&item)
            .expect("a reclaimed-device item with no key must still verify");
    }

    #[test]
    fn envelope_verification_rejects_bad_signature_when_key_present() {
        // The complement: while the source device still exists, a tampered
        // signature must be rejected, proving the key-present path still verifies.
        let mut item = signed_item_with_payload_count(1);
        if let Some(last) = item.envelope.signature.last_mut() {
            *last ^= 0x01;
        }
        verify_object_list_item_envelope(&item)
            .expect_err("a tampered signature with a key present must be rejected");
    }

    #[tokio::test]
    async fn device_ops_require_authentication() {
        // Both device operations must short-circuit with NotAuthenticated before
        // any network call when there is no active session.
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", std::env::temp_dir());
        assert!(matches!(
            engine.list_devices().await,
            Err(ClientError::NotAuthenticated),
        ));
        assert!(matches!(
            engine
                .remove_device("00000000-0000-0000-0000-000000000000")
                .await,
            Err(ClientError::NotAuthenticated),
        ));
    }
}
