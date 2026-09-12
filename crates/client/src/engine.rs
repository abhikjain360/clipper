//! Sync engine: manages client state, WebSocket connection, and clipboard/file operations.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

pub use clipper_app_types::{
    ActualView, AlarmView, AppState, AuthenticatedSession, CalendarSourceView, ClipboardPayload,
    CollabItem, ConnectionStatus, DecryptedClipboardItem, DecryptedFileItem, DeviceInfo,
    IngestReport, OccurrenceView, SavedProfile, ScheduleItemView,
};
use clipper_core::{crypto, models::*};
pub use clipper_schedule::{
    CalendarSource, Expansion, IngestedEvent, IngestedStatus, OccurrenceOverride, RecurrenceEngine,
    RruleEngine, ScheduleItem, ScheduleSpan, SourceId, SourceKind, TimeRange,
};
use futures_util::{StreamExt, stream};
use tokio::sync::{Mutex, RwLock, RwLockReadGuard, watch};
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

use crate::{
    api_client::{
        ApiClient, AuthDevice, ClientError, decrypt_clipboard_meta, decrypt_clipboard_payload,
        decrypt_file_blob_bytes, decrypt_file_meta_bytes, encrypt_clipboard_meta,
        encrypt_clipboard_payload, encrypt_file_blob_bytes, encrypt_file_meta_bytes,
    },
    local_store::{
        DeviceSigningIdentity, EncryptedInlineObject, EncryptedObject, LocalHead, LocalStore,
        LocalVisibleState, StoredObjectIdentity, clipboard_display_text, is_text_mime_type,
        normalized_clipboard_mime_type, top_level_mime_type, verify_payload_ciphertext,
    },
    schedule::{
        OccurrenceLabel, ScheduleRecord, actual_view, decrypt_schedule_meta,
        decrypt_schedule_payload, encrypt_schedule_meta, encrypt_schedule_payload,
        ingested_as_series, occurrence_key, occurrence_view, zone_or_utc,
    },
};

#[path = "calendar_import.rs"]
mod calendar_import;
#[path = "schedule_context.rs"]
mod schedule_context;
use schedule_context::revision_ref;

const INLINE_OBJECT_PAYLOAD_MAX_BYTES: usize = 64 * 1024;
/// Shown for time logged against nothing planned.
const UNPLANNED_TITLE: &str = "Unplanned";
/// Ceiling on a schedule payload's ciphertext. A series definition is a few
/// hundred bytes; this leaves room for a long title and a heavily overridden
/// series while still refusing a hostile server's unbounded download.
const MAX_SCHEDULE_PAYLOAD_CIPHERTEXT_BYTES: i64 = 256 * 1024;
const RECENT_CLIPBOARD_LIMIT: usize = 100;
/// MIME type used for plain-text clipboard entries.
pub const TEXT_CLIPBOARD_MIME_TYPE: &str = "text/plain";
const CLIPBOARD_HYDRATION_CONCURRENCY: usize = 8;

/// A snapshot must move forward inside its fixed watermark. Validate the
/// response before persisting anything, including pages whose items fail to
/// decrypt, so an untrusted server cannot trap reconciliation on one page.
fn validate_snapshot_page(
    page: &ObjectListResponse,
    after: Option<ObjectListCursor>,
    watermark: i64,
) -> Result<(), ClientError> {
    let key = |cursor: ObjectListCursor| (cursor.created_seq, cursor.id.into_uuid());
    let mut previous = after.map(key);
    if page.items.len() > 100 {
        return Err(ClientError::UnexpectedResponse(
            "snapshot page exceeds requested limit".into(),
        ));
    }
    for item in &page.items {
        let current = (item.created_seq, item.id.into_uuid());
        if item.created_seq > watermark || previous.is_some_and(|old| current <= old) {
            return Err(ClientError::UnexpectedResponse(
                "snapshot cursor did not advance within its watermark".into(),
            ));
        }
        previous = Some(current);
    }
    if let Some(next) = page.next_after
        && (page.items.is_empty() || Some(key(next)) != previous)
    {
        return Err(ClientError::UnexpectedResponse(
            "snapshot continuation does not match its last item".into(),
        ));
    }
    Ok(())
}
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
/// Ceiling on the plaintext this client will encrypt and upload. The same
/// figure as the server's default `max_file_blob_bytes`, refused here so a huge
/// file fails before the whole ciphertext is built in memory.
const MAX_FILE_UPLOAD_PLAINTEXT_BYTES: usize = 512 * 1024 * 1024;
#[cfg(target_family = "wasm")]
const WS_TICKET_PROTOCOL: &str = "clipper-ticket";

struct DecryptedClipboardObject {
    item: DecryptedClipboardItem,
    payload: Vec<u8>,
    encrypted: EncryptedInlineObject,
}

/// The sync engine that owns all client state.
pub struct SyncEngine {
    api: ApiClient,
    local_store: LocalStore,
    encryption_key: RwLock<Option<Zeroizing<[u8; 32]>>>,
    device_signing_key: RwLock<Option<Zeroizing<[u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]>>>,
    /// Retained after auth so the browser client can persist a session-resume
    /// blob (`{ token, data key, this wrapping key }`) and later boot via
    /// [`SyncEngine::resume_with_platform`] without re-running OPAQUE. It is no
    /// more sensitive than the data key it sits beside, and is cleared on
    /// logout / current-device removal. Native shells keep their engine resident
    /// and never read it back, but holding it is harmless there.
    device_identity_wrapping_key: RwLock<Option<Zeroizing<[u8; 32]>>>,
    state: RwLock<AppState>,
    state_tx: watch::Sender<u64>,
    state_version: std::sync::atomic::AtomicU64,
    ws_restart_tx: watch::Sender<u64>,
    ws_restart_rx: watch::Receiver<u64>,
    suppressed_payload: RwLock<Option<([u8; 32], web_time::Instant)>>,
    /// Serialize this device's timer commands across UI/IPC callers.
    actual_write: Mutex<()>,
    calendar_write: Mutex<()>,
    import_rules: Mutex<std::collections::VecDeque<calendar_import::CachedImportRules>>,
    schedule_history: Mutex<HashMap<(u64, clipper_schedule::ObjectRevisionRef), ScheduleRecord>>,
    history_epoch: std::sync::atomic::AtomicU64,
    /// The stamp of the newest view published to `state`, so an older view
    /// arriving late is dropped rather than shown.
    published_stamp: std::sync::atomic::AtomicU64,
}

/// Secrets a browser client needs to resume a session after a page reload
/// without persisting the passphrase. Produced by
/// [`SyncEngine::session_resume_material`] and consumed by
/// [`SyncEngine::resume_with_platform`].
///
/// Deliberately excludes the passphrase and the OPAQUE export key: `data_key`
/// and `device_identity_wrapping_key` are derived leaves (they cannot re-derive
/// the root, re-run login, or enroll a new device) and `token` is
/// server-revocable. See `docs/local-at-rest-encryption.md`.
pub struct SessionResumeMaterial {
    pub token: String,
    pub data_key: Zeroizing<[u8; 32]>,
    pub device_identity_wrapping_key: Zeroizing<[u8; 32]>,
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
            device_identity_wrapping_key: RwLock::new(None),
            state: RwLock::new(AppState::default()),
            state_tx: tx,
            state_version: std::sync::atomic::AtomicU64::new(0),
            ws_restart_tx,
            ws_restart_rx,
            suppressed_payload: RwLock::new(None),
            actual_write: Mutex::new(()),
            calendar_write: Mutex::new(()),
            schedule_history: Mutex::new(HashMap::new()),
            history_epoch: std::sync::atomic::AtomicU64::new(0),
            published_stamp: std::sync::atomic::AtomicU64::new(0),
            import_rules: Mutex::new(std::collections::VecDeque::new()),
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
        let _calendar = self.calendar_write.lock().await;
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
            device_identity_wrapping_key,
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
        let _calendar = self.calendar_write.lock().await;
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
            device_identity_wrapping_key,
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

    /// Resume a previously authenticated session from persisted material rather
    /// than an OPAQUE login: a still-valid bearer `token`, the OPAQUE-derived
    /// `data_key`, and the matching `device_identity_wrapping_key`. No passphrase
    /// is involved, so resume can neither re-derive the OPAQUE root nor enroll a
    /// new device — it only re-mounts the device the persisted identity already
    /// names.
    ///
    /// The token is checked against the server first; a revoked/expired token, a
    /// missing local device identity, or a wrapping key that fails to unwrap it
    /// all error so the UI can fall back to the login screen.
    pub async fn resume_with_platform(
        self: &Arc<Self>,
        token: String,
        data_key: Zeroizing<[u8; 32]>,
        device_identity_wrapping_key: Zeroizing<[u8; 32]>,
        username: &str,
        device_name: &str,
    ) -> Result<(), ClientError> {
        let _calendar = self.calendar_write.lock().await;
        self.api.restore_token(token);
        if let Err(error) = self.api.validate_session().await {
            // Never leave a dead token resident; force a clean re-login instead.
            if !self.end_refused_session(&error).await {
                self.api.clear_token();
            }
            return Err(error);
        }

        let profile_id = profile_id_from_encryption_key(&data_key);
        let signing_identity = self
            .local_store
            .load_device_signing_identity(&profile_id, &device_identity_wrapping_key)
            .await?
            .ok_or(ClientError::NoResumableDeviceIdentity)?;
        let device_id = signing_identity
            .device_id
            .clone()
            .ok_or(ClientError::NoResumableDeviceIdentity)?;

        self.finish_auth(
            device_name,
            username.to_string(),
            device_id.clone(),
            data_key,
            device_identity_wrapping_key,
            signing_identity,
        )
        .await?;

        info!("Session resumed, device_id={device_id}");
        Ok(())
    }

    /// Snapshot the secrets a browser client needs to resume this session after a
    /// reload without persisting the passphrase. Returns `None` before auth.
    ///
    /// The data key decrypts all object content, so this is sensitive — but it is
    /// deliberately *not the OPAQUE root*: it cannot re-derive the passphrase,
    /// re-run login, or enroll a new device, and the token is server-revocable.
    /// The caller owns where these land (the web client uses `sessionStorage`).
    pub async fn session_resume_material(&self) -> Option<SessionResumeMaterial> {
        let token = self.api.token()?;
        let data_key = self.encryption_key.read().await.as_ref().cloned()?;
        let device_identity_wrapping_key = self
            .device_identity_wrapping_key
            .read()
            .await
            .as_ref()
            .cloned()?;
        Some(SessionResumeMaterial {
            token,
            data_key,
            device_identity_wrapping_key,
        })
    }

    async fn finish_auth(
        self: &Arc<Self>,
        device_name: &str,
        username: String,
        device_id: String,
        encryption_key: Zeroizing<[u8; 32]>,
        device_identity_wrapping_key: Zeroizing<[u8; 32]>,
        signing_identity: DeviceSigningIdentity,
    ) -> Result<(), ClientError> {
        let cache_key = *encryption_key;
        let epoch = {
            let mut active_key = self.encryption_key.write().await;
            let epoch = self.history_epoch.fetch_add(1, Ordering::SeqCst) + 1;
            self.schedule_history.lock().await.clear();
            self.import_rules.lock().await.clear();
            // Fence anything still in flight from the previous session: the
            // database below is a different profile's, and a straggling write
            // that still passed the old generation would land in it.
            self.local_store.fence_and_clear_memory().await;
            self.local_store
                .set_profile(profile_id_from_encryption_key(&encryption_key));
            *active_key = Some(encryption_key);
            epoch
        };
        *self.device_signing_key.write().await = Some(signing_identity.signing_secret_key);
        *self.device_identity_wrapping_key.write().await = Some(device_identity_wrapping_key);

        {
            let mut state = self.state.write().await;
            state.session = Some(AuthenticatedSession {
                username: username.clone(),
                device_id: device_id.clone(),
                device_name: device_name.to_string(),
                server_url: self.api.base_url_display(),
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
                engine.ws_loop(epoch).await;
            });
        }

        // Start platform clipboard watcher where background reads are available.
        #[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
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
        let _calendar = self.calendar_write.lock().await;
        // Best-effort server-side revocation: an offline or failed call must not
        // leave key material resident, so tear down local state unconditionally.
        if let Err(error) = self.api.logout().await {
            warn!(%error, "Server-side logout failed; clearing local session anyway");
        }
        self.clear_local_session().await;
        info!("Logged out");
        Ok(())
    }

    /// Drop everything this session holds.
    ///
    /// Shared by logout, removing this device, and a session the server has
    /// definitively refused: all three must leave no key material resident and
    /// no in-flight sync write able to land. Bumping the store generation
    /// fences those writes. Without it a straggling snapshot or live event
    /// still passes the generation check and writes into whichever profile
    /// database the next login opens.
    async fn clear_local_session(&self) {
        self.api.clear_token();
        {
            let mut active_key = self.encryption_key.write().await;
            self.history_epoch.fetch_add(1, Ordering::SeqCst);
            *active_key = None;
            self.schedule_history.lock().await.clear();
            self.import_rules.lock().await.clear();
        }
        *self.device_signing_key.write().await = None;
        *self.device_identity_wrapping_key.write().await = None;
        self.local_store.fence_and_clear_memory().await;
        *self.state.write().await = AppState::default();
        self.bump_version();
    }

    /// End a session the server has refused.
    ///
    /// Only a 401 counts. A dropped WebSocket or a network error is a reason to
    /// retry, and tearing the session down for one would log the user out every
    /// time their connection blinked.
    async fn end_refused_session(&self, error: &ClientError) -> bool {
        if !session_refused(error) {
            return false;
        }
        warn!("The server refused this session; signing out");
        self.clear_local_session().await;
        true
    }

    /// End a session the server has refused, named by the store `generation`
    /// the refused request was issued under.
    ///
    /// A 401 can arrive long after the request that earned it, by which time
    /// the user may have logged out and back in. Taking `calendar_write` makes
    /// this a session change like login and logout, so it cannot interleave
    /// with one, and the generation check then tells whether the refused
    /// session is still the one installed. Callers that already hold
    /// `calendar_write` must use [`SyncEngine::end_refused_session`] instead.
    async fn end_refused_session_for(&self, generation: u64, error: &ClientError) -> bool {
        if !session_refused(error) {
            return false;
        }
        let _calendar = self.calendar_write.lock().await;
        if self.local_store.current_generation().await != generation {
            debug!("A later session replaced the refused one; keeping it signed in");
            return false;
        }
        warn!("The server refused this session; signing out");
        self.clear_local_session().await;
        true
    }

    /// End a session the server has refused, named by the `epoch` the refused
    /// request was issued under.
    ///
    /// The WebSocket loop holds an epoch and not a store generation: it is
    /// refused during the handshake, before it has claimed one. Taking
    /// `calendar_write` makes this a session change like login and logout, so
    /// it cannot interleave with one, and the epoch check then tells whether
    /// the refused session is still the installed one.
    async fn end_refused_session_for_epoch(&self, epoch: u64, error: &ClientError) -> bool {
        if !session_refused(error) {
            return false;
        }
        let _calendar = self.calendar_write.lock().await;
        if !self.session_is_current(epoch) {
            debug!("A later session replaced the refused one; keeping it signed in");
            return false;
        }
        warn!("The server refused this session; signing out");
        self.clear_local_session().await;
        true
    }

    /// Whether the session `epoch` names is still the installed one.
    ///
    /// Both login and logout bump `history_epoch` while holding the encryption
    /// key write lock, so a caller that holds a read guard and sees an
    /// unchanged epoch knows the keys and the profile database are still the
    /// ones its session opened.
    fn session_is_current(&self, epoch: u64) -> bool {
        self.history_epoch.load(Ordering::SeqCst) == epoch
    }

    /// Claim a store generation for the session `epoch` names.
    ///
    /// The read guard is what makes the check and the claim one step: a login
    /// or logout has to take the write lock to bump the epoch, so it either
    /// happens entirely before this or entirely after it. Without that, a
    /// socket authenticated as the previous account can make itself the
    /// current generation and stream its events into the next account's
    /// profile.
    async fn start_generation_for_session(&self, epoch: u64) -> Result<u64, ClientError> {
        let _active_key = self.encryption_key.read().await;
        if !self.session_is_current(epoch) {
            return Err(ClientError::NotAuthenticated);
        }
        Ok(self.local_store.start_generation().await)
    }

    /// Hold the session `epoch` names still across a local write.
    ///
    /// A user-initiated write encrypts under the session key, waits for the
    /// server, and only then persists. Login and logout bump the epoch while
    /// holding the key write lock, so a caller that takes this guard and finds
    /// the epoch unchanged keeps the session fixed for as long as it holds the
    /// guard: the profile database it writes and the state it publishes are
    /// its own session's.
    ///
    /// A caller whose session ended gets `NotAuthenticated` and writes
    /// nothing. The object is already on the server under the account that
    /// made it, and that account's next reconciliation lists it.
    async fn hold_session_for_write(
        &self,
        epoch: u64,
    ) -> Result<RwLockReadGuard<'_, Option<Zeroizing<[u8; 32]>>>, ClientError> {
        let active_key = self.encryption_key.read().await;
        if !self.session_is_current(epoch) {
            debug!("Dropping a write whose session ended while the server call was in flight");
            return Err(ClientError::NotAuthenticated);
        }
        Ok(active_key)
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
        let _calendar = self.calendar_write.lock().await;
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
            self.clear_local_session().await;
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

        // Read before the network work, so the persist below can tell whether
        // the session that started this push is still the one running.
        let epoch = self.history_epoch.load(Ordering::SeqCst);
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
        let aad_body = object_envelope_body_for_aad(
            object_id_typed,
            ObjectKind::Clipboard,
            EnvelopePlacement::Create,
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
        let envelope_body = object_envelope_body(
            object_id_typed,
            ObjectKind::Clipboard,
            EnvelopePlacement::Create,
            device_id_typed,
            created_at.clone(),
            meta_nonce.clone(),
            crypto::sha256(&meta_ciphertext).to_vec(),
            vec![ObjectEnvelopePayload {
                id: payload_id_typed,
                nonce: payload_nonce.clone(),
                ciphertext_size: payload_size,
                sha256_ciphertext: payload_hash.clone(),
            }],
        );
        let envelope = ObjectEnvelope {
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
        let _session = self.hold_session_for_write(epoch).await?;
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
            web_time::Instant::now(),
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
            web_time::Instant::now(),
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
        let init_resp = self.api.object_init(init_req).await?;
        self.finish_single_payload_object(
            object_id,
            payload_id,
            init_resp,
            encrypted_payload,
            payload_size,
            payload_hash,
        )
        .await
    }

    /// Drive a started object write to a published seq.
    ///
    /// Shared by init and revise, which differ only in the call that starts
    /// them: after that a write is a write, and an inline payload means it is
    /// already finished.
    async fn finish_single_payload_object(
        &self,
        object_id: &str,
        payload_id: &str,
        init_resp: ObjectInitResponse,
        encrypted_payload: Vec<u8>,
        payload_size: i64,
        payload_hash: Vec<u8>,
    ) -> Result<i64, ClientError> {
        let api = &self.api;
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
        check_upload_plaintext_size(data.len())?;
        // Read before the network work, so the persist below can tell whether
        // the session that started this upload is still the one running.
        let epoch = self.history_epoch.load(Ordering::SeqCst);
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
        let aad_body = object_envelope_body_for_aad(
            file_id_typed,
            ObjectKind::File,
            EnvelopePlacement::Create,
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
        let envelope_body = object_envelope_body(
            file_id_typed,
            ObjectKind::File,
            EnvelopePlacement::Create,
            device_id_typed,
            created_at.clone(),
            meta_nonce.clone(),
            crypto::sha256(&meta_ciphertext).to_vec(),
            vec![ObjectEnvelopePayload {
                id: payload_id_typed,
                nonce: blob_nonce.clone(),
                ciphertext_size: blob_size,
                sha256_ciphertext: blob_hash.clone(),
            }],
        );
        let envelope = ObjectEnvelope {
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
        let _session = self.hold_session_for_write(epoch).await?;
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
        // Read before the network work, so the retention below can tell
        // whether the session that asked for this file is still the one
        // running when the bytes come back.
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let (file_item, payload, encrypted_blob) = {
            let api = &self.api;
            let file_item = api.get_object(file_id).await?;
            verify_object_list_item_envelope(&file_item)?;
            if file_item.id.to_string() != file_id {
                return Err(ClientError::UnexpectedResponse(format!(
                    "download of object {file_id} returned mismatched identity"
                )));
            }
            self.check_revision_advance(&file_item).await?;
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

        let encryption_key = self.current_encryption_key().await?;
        let plaintext = decrypt_file_blob_bytes(
            &payload.nonce,
            &encrypted_blob,
            &encryption_key,
            &file_item.envelope.body,
            payload.id,
        )?;
        self.retain_downloaded_file(&file_item, &encryption_key, epoch)
            .await?;
        info!(file_id = %file_id, "File downloaded");
        Ok(plaintext)
    }

    /// Record the revision a download accepted.
    ///
    /// A download verifies a file's head and then decrypts it, but nothing on
    /// that path stores anything, so without this the anchor never advances and
    /// the server can serve revision 3 and then revision 2 to the same device.
    /// The head is stored the way a fetched file is stored anywhere else.
    ///
    /// `epoch` is the session the download was started under. A slow download
    /// can finish after the user has logged out and back in as someone else,
    /// and this write goes to whichever profile database is mounted now, so a
    /// download from the previous account is dropped instead of stored and
    /// shown. The key read guard holds the session still for the whole write:
    /// login and logout both bump the epoch under the write lock. The store
    /// generation is the wrong fence here — it changes on every WebSocket
    /// reconnect, and a download still has to advance the anchor across one.
    async fn retain_downloaded_file(
        &self,
        item: &ObjectListItem,
        encryption_key: &[u8; 32],
        epoch: u64,
    ) -> Result<(), ClientError> {
        let object_id = item.id.to_string();
        let _active_key = self.encryption_key.read().await;
        if !self.session_is_current(epoch) {
            debug!(
                file_id = %object_id,
                "Dropping a download that outlived the session it was started under",
            );
            return Ok(());
        }
        // Already at this revision: the record is the one this would write.
        if self
            .local_store
            .local_head(&object_id)
            .await?
            .is_some_and(|head| head.revision >= item.revision)
        {
            return Ok(());
        }
        let file = decrypt_file_object_item(item, encryption_key)?;
        let visible = self
            .local_store
            .persist_local_file_present_encrypted(
                &file,
                &encrypted_object_from_list_item(item),
                item.created_seq,
                item.created_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?;
        self.publish_visible_state(visible).await;
        Ok(())
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

    /// Delete a file by appending a tombstone revision.
    ///
    /// This is the delete. `DELETE /api/objects/{id}` is purge, and it refuses
    /// an object that has not been tombstoned. Reclaiming the blob is that
    /// separate purge, which nothing calls yet.
    pub async fn delete_file(&self, file_id: &str) -> Result<(), ClientError> {
        let _write = self.calendar_write.lock().await;
        if self
            .local_store
            .schedule_records_with_ids()
            .await
            .iter()
            .filter_map(|(_, record)| record.as_source())
            .any(|source| {
                source
                    .pending_import
                    .as_ref()
                    .is_some_and(|batch| batch.object_id.to_string() == file_id)
            })
        {
            return Err(ClientError::InvalidArgument(
                "This feed is needed to finish a pending import; refresh the calendar first".into(),
            ));
        }
        let is_import = self.is_import_file(file_id).await?;
        let (deleted_seq, tombstone_head) = self.write_tombstone(file_id, ObjectKind::File).await?;
        let visible = self
            .local_store
            .apply_local_tombstone(
                ObjectKind::File,
                file_id,
                deleted_seq,
                tombstone_head,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?;
        self.publish_visible_state(visible).await;
        if is_import {
            match self.api.delete_object(file_id).await {
                Ok(_) | Err(ClientError::Api { status: 404, .. }) => {}
                Err(error) => return Err(error),
            }
        }
        info!(file_id = %file_id, "File deleted");
        Ok(())
    }

    // ── Schedule ──

    /// Create a schedule series.
    pub async fn create_schedule_item(&self, item: ScheduleItem) -> Result<String, ClientError> {
        self.create_schedule_record(ScheduleRecord::Item(Box::new(item)))
            .await
    }

    /// Seal a schedule record into an object and publish it.
    ///
    /// Mirrors the clipboard path: a small encrypted meta plus one inline
    /// payload, so `object_init` completes the object without a second
    /// round-trip. The meta says only which kind of record this is; the record
    /// itself is in the payload.
    async fn create_schedule_record(&self, record: ScheduleRecord) -> Result<String, ClientError> {
        let object_id = uuid::Uuid::now_v7().to_string();
        self.write_schedule_record(&object_id, record, EnvelopePlacement::Create)
            .await?;
        info!(object_id = %object_id, "Schedule record created");
        Ok(object_id)
    }

    /// Seal a schedule record and write it as one revision of an object.
    ///
    /// Genesis and revise differ only in the placement they are given and the
    /// route that starts the write. Sealing is the same for both, and so is
    /// riding a small record inline, which completes without a second
    /// round-trip. Deleting is not routed through here: a tombstone carries no
    /// payload, so it shares nothing with this beyond the envelope.
    async fn write_schedule_record(
        &self,
        object_id: &str,
        record: ScheduleRecord,
        placement: EnvelopePlacement,
    ) -> Result<i64, ClientError> {
        // Read before the network work, so the persist below can tell whether
        // the session that started this write is still the one running. The
        // timer paths hold `actual_write`, which a login or logout does not
        // take, so this is their only fence.
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let encryption_key = self.current_encryption_key().await?;
        let (device_id, device_id_typed, signing_key) =
            self.current_device_signing_context().await?;

        let object_uuid: uuid::Uuid =
            object_id.parse().map_err(|source| ClientError::InvalidId {
                kind: "object id",
                source,
            })?;
        let object_id_typed: ObjectId = object_uuid.into();
        let created_at = chrono::Utc::now().to_rfc3339();

        let payload_uuid = uuid::Uuid::now_v7();
        let payload_id = payload_uuid.to_string();
        let payload_id_typed: ObjectPayloadId = payload_uuid.into();

        let aad_body = object_envelope_body_for_aad(
            object_id_typed,
            ObjectKind::Schedule,
            placement,
            device_id_typed,
            created_at.clone(),
            vec![payload_id_typed],
        );
        let meta = record.meta();
        let (meta_nonce, meta_ciphertext) =
            encrypt_schedule_meta(&meta, &encryption_key, &aad_body)?;
        let (payload_nonce, encrypted_payload) =
            encrypt_schedule_payload(&record, &encryption_key, &aad_body, payload_id_typed)?;

        if encrypted_payload.len() > MAX_SCHEDULE_PAYLOAD_CIPHERTEXT_BYTES as usize {
            return Err(ClientError::InvalidArgument(
                "schedule record exceeds the 256 KiB encrypted size limit".into(),
            ));
        }

        let payload_hash = crypto::sha256(&encrypted_payload).to_vec();
        let payload_size = encrypted_payload.len() as i64;
        let envelope_body = object_envelope_body(
            object_id_typed,
            ObjectKind::Schedule,
            placement,
            device_id_typed,
            created_at.clone(),
            meta_nonce.clone(),
            crypto::sha256(&meta_ciphertext).to_vec(),
            vec![ObjectEnvelopePayload {
                id: payload_id_typed,
                nonce: payload_nonce.clone(),
                ciphertext_size: payload_size,
                sha256_ciphertext: payload_hash.clone(),
            }],
        );
        let envelope = ObjectEnvelope {
            signature: crypto::sign_object_envelope_body(&signing_key, &envelope_body)?,
            body: envelope_body,
        };
        let payloads = vec![ObjectPayloadInit {
            id: payload_id_typed,
            nonce: payload_nonce,
            ciphertext_size: payload_size,
            sha256_ciphertext: payload_hash.clone(),
            inline_ciphertext: inline_ciphertext(&encrypted_payload),
        }];

        let (encrypted_object, write_resp) = match placement {
            EnvelopePlacement::Create => {
                let init_req = ObjectInitRequest {
                    id: object_id_typed,
                    kind: ObjectKind::Schedule,
                    meta_nonce,
                    meta_ciphertext,
                    payloads,
                    envelope,
                };
                let encrypted = encrypted_object_from_init(&init_req);
                let resp = self.api.object_init(&init_req).await?;
                (encrypted, resp)
            }
            EnvelopePlacement::Revise(_) | EnvelopePlacement::Delete(_) => {
                let revise_req = ObjectReviseRequest {
                    meta_nonce,
                    meta_ciphertext,
                    payloads,
                    envelope,
                };
                let encrypted = encrypted_object_from_revise(&revise_req);
                let resp = self.api.object_revise(object_id, &revise_req).await?;
                (encrypted, resp)
            }
        };

        let encrypted = EncryptedInlineObject {
            object: encrypted_object,
            payload_ciphertext: encrypted_payload.clone(),
        };
        let created_seq = self
            .finish_single_payload_object(
                object_id,
                &payload_id,
                write_resp,
                encrypted_payload,
                payload_size,
                payload_hash,
            )
            .await?;

        let _session = self.hold_session_for_write(epoch).await?;
        let visible = self
            .local_store
            .persist_local_schedule_present_encrypted(
                StoredObjectIdentity {
                    object_id,
                    created_at: &created_at,
                    source_device_id: &device_id,
                },
                record,
                &encrypted,
                created_seq,
                created_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?;
        self.publish_visible_state(visible).await;
        Ok(created_seq)
    }

    /// Append a tombstone: a signed revision with no payloads.
    ///
    /// Shared by every deletable kind, because a tombstone is the same object
    /// in all of them. Its meta is still encrypted and still bound to the
    /// envelope even though it says nothing: the column is not nullable, and
    /// an empty ciphertext would be a second shape the server's checks would
    /// have to know about.
    async fn write_tombstone(
        &self,
        object_id: &str,
        kind: ObjectKind,
    ) -> Result<(i64, LocalHead), ClientError> {
        let encryption_key = self.current_encryption_key().await?;
        let (_, device_id_typed, signing_key) = self.current_device_signing_context().await?;
        let object_uuid: uuid::Uuid =
            object_id.parse().map_err(|source| ClientError::InvalidId {
                kind: "object id",
                source,
            })?;
        let object_id_typed: ObjectId = object_uuid.into();
        let placement = EnvelopePlacement::Delete(self.local_head(object_id).await?);
        let created_at = chrono::Utc::now().to_rfc3339();

        let aad_body = object_envelope_body_for_aad(
            object_id_typed,
            kind,
            placement,
            device_id_typed,
            created_at.clone(),
            Vec::new(),
        );
        let aad = crypto::object_meta_aad(&aad_body)?;
        let (meta_nonce, meta_ciphertext) =
            crypto::encrypt(&encryption_key, TOMBSTONE_META_PLAINTEXT, &aad)?;
        let envelope_body = object_envelope_body(
            object_id_typed,
            kind,
            placement,
            device_id_typed,
            created_at,
            meta_nonce.clone(),
            crypto::sha256(&meta_ciphertext).to_vec(),
            Vec::new(),
        );
        let revise_req = ObjectReviseRequest {
            meta_nonce,
            meta_ciphertext,
            payloads: Vec::new(),
            envelope: ObjectEnvelope {
                signature: crypto::sign_object_envelope_body(&signing_key, &envelope_body)?,
                body: envelope_body,
            },
        };
        let tombstone_head = LocalHead {
            revision: revise_req.envelope.body.revision,
            parent_hash: crypto::object_envelope_parent_hash(&revise_req.envelope.body)?,
        };
        match self.api.object_revise(object_id, &revise_req).await? {
            ObjectInitResponse::Complete { created_seq } => Ok((created_seq, tombstone_head)),
            ObjectInitResponse::Pending { .. } => Err(ClientError::UnexpectedResponse(
                "a tombstone carries no payloads and must complete immediately".into(),
            )),
        }
    }

    // ── Actuals ──

    /// Start unplanned time, or time against the exact context rendered by the calendar.
    /// Validate the plan before stopping another timer.
    pub async fn start_actual(&self, plan_context: Option<&str>) -> Result<String, ClientError> {
        let planned: Option<clipper_schedule::PlannedRef> = plan_context
            .map(|text| {
                if text.len() > 8192 {
                    return Err(ClientError::InvalidArgument(
                        "Plan context is too large".into(),
                    ));
                }
                serde_json::from_str(text)
                    .map_err(|e| ClientError::InvalidArgument(format!("Invalid plan context: {e}")))
            })
            .transpose()?;
        let _write = self.actual_write.lock().await;
        if let Some(planned) = &planned {
            self.schedule_revision(planned.schedule).await?;
            let records = self.local_store.schedule_records_with_heads().await?;
            self.validate_plan_context(planned, &records).await?;
            // History lookups may yield to sync. Revalidate current heads before
            // any timer mutation rather than silently adopting a newer plan.
            let refreshed = self.local_store.schedule_records_with_heads().await?;
            self.validate_plan_context(planned, &refreshed).await?;
        }
        if let Some(running) = self.running_actual_id().await {
            self.stop_actual_inner(&running).await?;
        }
        self.create_schedule_record(ScheduleRecord::Actual(Box::new(
            clipper_schedule::ActualRecord {
                id: clipper_schedule::ActualId::new(),
                planned,
                span: clipper_schedule::ActualSpan::Running {
                    started: chrono::Utc::now(),
                },
            },
        )))
        .await
    }

    /// Stop the timer, closing the record at now.
    ///
    /// Two writes per session and no more: the record is created on start
    /// and replaced on stop. Persisting progress on a tick would turn an hour
    /// of work into sixty retained revisions.
    pub async fn stop_actual(&self, object_id: &str) -> Result<String, ClientError> {
        let _write = self.actual_write.lock().await;
        self.stop_actual_inner(object_id).await
    }

    async fn stop_actual_inner(&self, object_id: &str) -> Result<String, ClientError> {
        let Some((_, record, head)) = self
            .local_store
            .schedule_records_with_heads()
            .await?
            .into_iter()
            .find(|(id, record, _)| id == object_id && matches!(record, ScheduleRecord::Actual(_)))
        else {
            return Err(ClientError::ItemNotFound {
                id: object_id.to_string(),
            });
        };
        let ScheduleRecord::Actual(actual) = record else {
            unreachable!("filtered to actual records above");
        };
        let clipper_schedule::ActualSpan::Running { started } = actual.span else {
            return Err(ClientError::InvalidArgument(
                "that timer has already been stopped".into(),
            ));
        };

        let mut stopped = *actual;
        stopped.span =
            clipper_schedule::ActualSpan::Complete(stopped_span(started, chrono::Utc::now())?);
        self.write_schedule_record(
            object_id,
            ScheduleRecord::Actual(Box::new(stopped)),
            EnvelopePlacement::Revise(head),
        )
        .await?;
        Ok(object_id.to_string())
    }

    /// Records of time spent that overlap `[from, to)`, plus any running timer.
    pub async fn actuals_between(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<ActualView>, ClientError> {
        let from = parse_instant(from, "actuals window start")?;
        let to = parse_instant(to, "actuals window end")?;
        TimeRange::new(from, to)
            .map_err(|error| ClientError::InvalidArgument(error.to_string()))?;
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let records = self.local_store.schedule_records_with_ids().await;
        let mut out = Vec::new();
        for (object_id, record) in &records {
            let ScheduleRecord::Actual(actual) = record else {
                continue;
            };
            let (start, end) = match actual.span {
                clipper_schedule::ActualSpan::Running { started } => (started, chrono::Utc::now()),
                clipper_schedule::ActualSpan::Complete(span) => (span.start(), span.end()),
            };
            if start >= to || end <= from {
                continue;
            }
            out.push(actual_view(
                object_id,
                actual,
                &self.actual_title(actual).await,
            ));
        }
        out.sort_by(|a, b| a.start.cmp(&b.start));
        let mut state = self.state.write().await;
        if self.history_epoch.load(Ordering::SeqCst) != epoch {
            return Err(ClientError::NotAuthenticated);
        }
        if let Some(running) = &mut state.running_actual
            && let Some(resolved) = out
                .iter()
                .find(|entry| entry.id == running.id && entry.running)
            && running.title != resolved.title
        {
            running.title = resolved.title.clone();
            drop(state);
            self.bump_version();
        }
        Ok(out)
    }

    /// The object id of the running timer, if one is running.
    async fn running_actual_id(&self) -> Option<String> {
        self.local_store
            .schedule_records_with_ids()
            .await
            .into_iter()
            .find(|(_, record)| {
                matches!(record, ScheduleRecord::Actual(actual)
                    if matches!(actual.span, clipper_schedule::ActualSpan::Running { .. }))
            })
            .map(|(object_id, _)| object_id)
    }

    /// Replace a schedule series with an edited version.
    ///
    /// Preserve identity while appending a new definition. The expected revision
    /// comes from the editor, not from whatever head arrived just before saving.
    pub async fn update_schedule_item(
        &self,
        object_id: &str,
        item: ScheduleItem,
        expected_revision: u64,
    ) -> Result<String, ClientError> {
        let records = self.local_store.schedule_records_with_heads().await?;
        let existing = records
            .iter()
            .find(|(id, record, _)| id == object_id && record.as_item().is_some())
            .cloned();
        let Some((_, previous, head)) = existing else {
            return Err(ClientError::ItemNotFound {
                id: object_id.to_string(),
            });
        };
        if head.revision != expected_revision {
            return Err(ClientError::InvalidArgument(
                "This schedule changed since the editor opened; reopen it before saving".into(),
            ));
        }
        if !previous
            .as_item()
            .expect("series")
            .overrides_compatible_with(&item)
            && records.iter().any(|(_, record, _)| {
                matches!(record,
                ScheduleRecord::Override(entry) if entry.base.object_id.to_string() == object_id)
            })
        {
            return Err(ClientError::InvalidArgument(
                "This schedule has occurrence overrides. Resolve them before changing its timing or recurrence".into(),
            ));
        }
        let previous_series = previous
            .as_item()
            .expect("filtered to series records above")
            .id;
        if item.id != previous_series {
            return Err(ClientError::InvalidArgument(
                "an edit must keep the series id; overrides and logged time reference it".into(),
            ));
        }

        self.write_schedule_record(
            object_id,
            ScheduleRecord::Item(Box::new(item)),
            EnvelopePlacement::Revise(head),
        )
        .await?;
        info!(object_id = %object_id, revision = head.revision + 1, "Schedule item edited");
        // The object id is stable across an edit now, so callers that used to
        // follow a replacement id get the same one back.
        Ok(object_id.to_string())
    }

    /// Refuse a served revision that is older than, or discontinuous with, the
    /// one this client already holds.
    ///
    /// Two distinct checks, and both need local state, which is why they cannot
    /// live in the stateless envelope verification:
    ///
    /// Rollback. A server can serve revision 3 while 7 exists. Every envelope
    /// in the chain is signed, so nothing about revision 3 looks wrong on its
    /// own. Only a client that remembers 7 can tell. A freshly
    /// installed device has nothing to remember and must trust what it is
    /// given: signatures alone cannot establish that a head is the newest one.
    ///
    /// Continuity. When the served revision is the immediate successor of the
    /// held one, its parent hash must be the held one's. A server that drops or
    /// substitutes a revision leaves a hash that does not match. A larger jump
    /// cannot be checked locally, because the revisions in between were never
    /// seen.
    ///
    /// The store owns the rules, because it owns the anchors: an object that is
    /// gone locally still has one, and a check driven by the held head alone
    /// would ignore it.
    async fn check_revision_advance(&self, item: &ObjectListItem) -> Result<(), ClientError> {
        self.local_store
            .validate_incoming_revision(&item.id.to_string(), &item.envelope.body)
            .await
            .map_err(Into::into)
    }

    /// The chain position this client holds for an object, or a typed error.
    async fn local_head(&self, object_id: &str) -> Result<LocalHead, ClientError> {
        self.local_store
            .local_head(object_id)
            .await?
            .ok_or_else(|| ClientError::ItemNotFound {
                id: object_id.to_string(),
            })
    }

    /// Delete a schedule object by appending a tombstone revision.
    ///
    /// Reversible on purpose: the chain behind the tombstone survives, so the
    /// object can be brought back by writing a revision that restores an
    /// earlier one. Reclaiming the bytes is a separate purge, which nothing in
    /// the UI calls yet.
    pub async fn delete_schedule_object(&self, object_id: &str) -> Result<(), ClientError> {
        let _write = self.calendar_write.lock().await;
        self.remove_calendar_imports(object_id).await?;
        self.tombstone_schedule_object(object_id).await
    }

    async fn tombstone_schedule_object(&self, object_id: &str) -> Result<(), ClientError> {
        let (deleted_seq, tombstone_head) = self
            .write_tombstone(object_id, ObjectKind::Schedule)
            .await?;
        let visible = self
            .local_store
            .apply_local_tombstone(
                ObjectKind::Schedule,
                object_id,
                deleted_seq,
                tombstone_head,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?;
        self.publish_visible_state(visible).await;
        info!(object_id = %object_id, "Schedule record deleted");
        Ok(())
    }

    /// Expand every cached series across `[from, to)` and return the
    /// occurrences that overlap it, including blocks starting before `from`.
    ///
    /// Occurrences are computed here rather than stored, and the window is
    /// the caller's choice rather than a fixed horizon — a grid asks for a
    /// week, an alarm scheduler asks for the next day.
    pub async fn expand_schedule(
        &self,
        from: &str,
        to: &str,
        observer_zone: &str,
    ) -> Result<Vec<OccurrenceView>, ClientError> {
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let from = parse_instant(from, "expansion window start")?;
        let to = parse_instant(to, "expansion window end")?;
        let window = TimeRange::new(from, to)
            .map_err(|error| ClientError::InvalidArgument(error.to_string()))?;
        let expansion = Expansion {
            window,
            observer: zone_or_utc(observer_zone),
        };

        let records = self.local_store.schedule_records_with_heads().await?;
        let source_names: HashMap<SourceId, &CalendarSource> = records
            .iter()
            .filter_map(|(_, record, _)| record.as_source())
            .map(|source| (source.id, source))
            .collect();

        let mut out = Vec::new();
        let mut warnings = Vec::new();
        let mut unavailable_imports = HashMap::<ObjectId, String>::new();

        let ready_sources = calendar_import::ready_sources(&records);
        for source in source_names.values() {
            if source.active_import.is_some() && !ready_sources.contains(&source.id) {
                warnings.push(format!(
                    "{}: waiting for a complete, consistent imported calendar",
                    source.name
                ));
            }
        }
        for (object_id, record, head) in &records {
            // An owned block and an ingested event expand identically; only
            // their labelling differs.
            let Some(series) = schedule_context::series(record) else {
                continue;
            };
            let (label_source, cancelled) = match record {
                ScheduleRecord::Ingested(event) => {
                    let Some(source) = source_names.get(&event.source) else {
                        // Removing a source hides its events, while retaining
                        // the records referenced by previously logged time.
                        continue;
                    };
                    if !ready_sources.contains(&event.source)
                        || !source.contains_event(object_id, event)
                    {
                        continue;
                    }
                    (
                        Some(source.name.as_str()),
                        event.status == IngestedStatus::Cancelled,
                    )
                }
                // Every other kind has no series, so it was skipped above.
                _ => (None, false),
            };
            let all_day = matches!(series.span, ScheduleSpan::AllDay { .. });
            let imported = match &series.recurrence {
                clipper_schedule::Recurrence::Imported { import, .. } => Some(*import),
                _ => None,
            };
            if let Some(error) = imported.and_then(|id| unavailable_imports.get(&id)) {
                warnings.push(format!("{}: {}", series.title, error));
                continue;
            }
            let engine = match self.recurrence_engine(&series.recurrence).await {
                Ok(engine) => engine,
                Err(error) => {
                    if let Some(import) = imported {
                        unavailable_imports.insert(import, error.to_string());
                    }
                    warnings.push(format!("{}: {}", series.title, error));
                    continue;
                }
            };
            let pin = revision_ref(object_id, *head)?;
            let effective = match self
                .effective_overrides(&series, pin, record, &records)
                .await
            {
                Ok(entries) => entries,
                Err(error) => {
                    warnings.push(format!("{}: {}", series.title, error));
                    continue;
                }
            };
            let effective_overrides: Vec<_> =
                effective.iter().map(|(entry, _)| entry.clone()).collect();
            match engine.overlapping_occurrences(&series, &effective_overrides, &expansion) {
                Ok(occurrences) => out.extend(occurrences.iter().map(|occurrence| {
                    occurrence_view(
                        occurrence,
                        OccurrenceLabel {
                            title: &series.title,
                            all_day,
                            source: label_source,
                            cancelled,
                        },
                        &clipper_schedule::PlannedRef {
                            item: series.id,
                            recurrence_id: occurrence.recurrence_id,
                            schedule: pin,
                            override_revision: effective
                                .iter()
                                .find(|(entry, _)| entry.recurrence_id == occurrence.recurrence_id)
                                .and_then(|(_, pin)| *pin),
                            observer: expansion.observer,
                            span: occurrence.span,
                        },
                    )
                })),
                // One malformed series must not blank the whole calendar.
                Err(error) => {
                    warnings.push(format!("{}: {}", series.title, error));
                    warn!(item = %series.id, "Failed to expand schedule series: {}", error)
                }
            }
        }
        out.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.title.cmp(&b.title)));
        warnings.sort();
        let mut state = self.state.write().await;
        if self.history_epoch.load(Ordering::SeqCst) != epoch {
            return Err(ClientError::NotAuthenticated);
        }
        if state.schedule_warnings != warnings {
            state.schedule_warnings = warnings;
            drop(state);
            self.bump_version();
        }
        Ok(out)
    }

    /// Every alarm due in the next `within_hours`, soonest first.
    ///
    /// The platform registers each as a one-shot exact alarm and never expands
    /// a recurrence itself, so there is only one implementation of a rule.
    ///
    /// An ingested event never rings on its own. Importing a calendar is not
    /// permission to ring on this device.
    pub async fn next_alarms(
        &self,
        within_hours: u32,
        observer_zone: &str,
    ) -> Result<Vec<AlarmView>, ClientError> {
        let now = chrono::Utc::now();
        if within_hours > 24 * 366 {
            return Err(ClientError::InvalidArgument(
                "alarm horizon cannot exceed one year".into(),
            ));
        }
        let until = now + chrono::TimeDelta::hours(i64::from(within_hours.max(1)));

        let records = self.local_store.schedule_records_with_heads().await?;
        let mut alarms = Vec::new();
        for (object_id, record, head) in &records {
            let Some(item) = record.as_item() else {
                continue;
            };
            let Some(policy) = item.alarm else {
                continue;
            };
            let engine = match self.recurrence_engine(&item.recurrence).await {
                Ok(engine) => engine,
                Err(error) => {
                    warn!(item = %item.id, %error, "Skipping alarms with unavailable recurrence");
                    continue;
                }
            };
            // Bound the window by fire time, not event time. For a two-hour
            // lead, tomorrow's 01:00 event must be included in today's alarms.
            let lead = chrono::TimeDelta::minutes(i64::from(policy.minutes_before));
            let expansion = Expansion {
                window: TimeRange::new(now + lead, until + lead)
                    .map_err(|error| ClientError::InvalidArgument(error.to_string()))?,
                observer: zone_or_utc(observer_zone),
            };
            let effective = match self
                .effective_overrides(item, revision_ref(object_id, *head)?, record, &records)
                .await
            {
                Ok(entries) => entries,
                Err(error) => {
                    warn!(item = %item.id, %error, "Skipping alarms for a schedule with unresolved overrides");
                    continue;
                }
            };
            let overrides: Vec<_> = effective.into_iter().map(|(entry, _)| entry).collect();
            match engine.occurrences(item, &overrides, &expansion) {
                // Every occurrence starts before `until + lead`, so every fire
                // time is already before `until`.
                Ok(occurrences) => alarms.extend(
                    clipper_schedule::plan_alarms(item, &occurrences, now)
                        .iter()
                        .map(|planned| AlarmView {
                            item_id: planned.item.to_string(),
                            occurrence_key: occurrence_key(&planned.recurrence_id),
                            label: planned.label.clone(),
                            fire_at_millis: planned.fire_at.timestamp_millis(),
                            occurrence_start_millis: planned.occurrence_start.timestamp_millis(),
                        }),
                ),
                // A series that will not expand must not silence every other
                // alarm on the device.
                Err(error) => warn!(item = %item.id, "Failed to plan alarms: {}", error),
            }
        }
        alarms.sort_by_key(|alarm| alarm.fire_at_millis);
        Ok(alarms)
    }

    /// Register a calendar to pull events from.
    pub async fn add_calendar_source(&self, name: &str, url: &str) -> Result<String, ClientError> {
        // Reject a URL the fetcher could never use, while the user is still
        // here to fix the typo.
        let mut parsed = url::Url::parse(url)
            .map_err(|error| ClientError::InvalidArgument(format!("calendar URL: {error}")))?;
        if !matches!(parsed.scheme(), "http" | "https" | "webcal") {
            return Err(ClientError::InvalidArgument(format!(
                "calendar URL scheme {:?} is not supported",
                parsed.scheme()
            )));
        }
        if parsed.host_str().is_none() {
            return Err(ClientError::InvalidArgument(
                "calendar URL needs a host".into(),
            ));
        }
        if parsed.scheme() == "webcal" {
            // `webcal` is not a special URL scheme, so Url::set_scheme cannot
            // convert it directly into a special (HTTPS) URL.
            parsed = url::Url::parse(&format!("https:{}", &parsed.as_str()[7..]))
                .map_err(|error| ClientError::InvalidArgument(format!("calendar URL: {error}")))?;
        }
        self.create_schedule_record(ScheduleRecord::Source(Box::new(CalendarSource {
            id: SourceId::new(),
            name: name.trim().to_string(),
            // `webcal:` is just `https:` wearing a hat; normalize it now so the
            // fetcher never has to know.
            kind: SourceKind::Ics {
                url: parsed.to_string(),
            },
            enabled: true,
            active_import: None,
            pending_import: None,
            retired_imports: Vec::new(),
        })))
        .await
    }

    /// Reconcile every schedule object from the encrypted-object listing.
    async fn snapshot_schedule(
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
                    Some(ObjectKind::Schedule),
                    Some(100),
                    Some(stream_start_seq),
                    after,
                )
                .await?;
            validate_snapshot_page(&page, after, stream_start_seq)?;
            for item in page.items {
                match self
                    .decrypt_schedule_object_item(api, &item, &encryption_key)
                    .await
                {
                    Ok((record, encrypted)) => {
                        if let Some(visible) = self
                            .local_store
                            .persist_snapshot_schedule_present_encrypted(
                                StoredObjectIdentity {
                                    object_id: &item.id.to_string(),
                                    created_at: &item.created_at,
                                    source_device_id: &item.source_device_id.to_string(),
                                },
                                record,
                                &encrypted,
                                item.created_seq,
                                generation,
                                RECENT_CLIPBOARD_LIMIT,
                            )
                            .await?
                        {
                            self.publish_visible_state(visible).await;
                        }
                    }
                    Err(error) => {
                        if let Err(error) = self.keep_held_revision(&item, generation, error).await
                        {
                            warn!(id = %item.id, "Failed to decrypt schedule object: {}", error);
                        }
                    }
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
                ObjectKind::Schedule,
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

    async fn decrypt_schedule_object_item(
        &self,
        api: &ApiClient,
        item: &ObjectListItem,
        encryption_key: &[u8; 32],
    ) -> Result<(ScheduleRecord, EncryptedInlineObject), ClientError> {
        verify_object_list_item_envelope(item)?;
        self.check_revision_advance(item).await?;
        // The meta is decrypted for its own sake: it authenticates that this
        // object really is a schedule record of the kind the payload claims.
        let meta = decrypt_schedule_meta(
            &item.meta_nonce,
            &item.meta_ciphertext,
            encryption_key,
            &item.envelope.body,
        )?;
        let payload = single_payload(item)?;
        check_payload_ciphertext_size(payload, MAX_SCHEDULE_PAYLOAD_CIPHERTEXT_BYTES)?;
        let encrypted_payload = api
            .download_object_payload(
                &item.id.to_string(),
                &payload.id.to_string(),
                payload.ciphertext_size,
            )
            .await?;
        verify_payload_hash(payload, &encrypted_payload)?;
        let record = decrypt_schedule_payload(
            &payload.nonce,
            &encrypted_payload,
            encryption_key,
            &item.envelope.body,
            payload.id,
        )?;
        if record.kind() != meta.record {
            return Err(ClientError::UnexpectedResponse(format!(
                "schedule object {} has a {} meta but a {} payload",
                item.id,
                meta.record,
                record.kind()
            )));
        }
        Ok((
            record,
            EncryptedInlineObject {
                object: encrypted_object_from_list_item(item),
                payload_ciphertext: encrypted_payload,
            },
        ))
    }

    // ── Collab docs ──

    /// Create a collab doc. The server suppresses the originating device's own
    /// WS `created` event, and the encrypted-object snapshot never lists collab
    /// objects, so the new doc is persisted into the local store here — otherwise
    /// it would never appear on the device that created it. The seq and
    /// `created_at` are assigned client-side (the create response carries neither,
    /// and the server seq is a wall-clock microsecond timestamp, so a local one
    /// of the same magnitude sorts correctly and is superseded by any later
    /// `deleted` event).
    pub async fn create_collab_doc(&self) -> Result<CollabItem, ClientError> {
        // Read before the network work, so the persist below can tell whether
        // the session that created the doc is still the one running.
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let response = self.api.create_collab_doc().await?;
        let item = collab_item_from_meta(&response.doc);
        let object_id = item.id.clone();
        let created_seq = collab_created_seq(&item.created_at);
        let _session = self.hold_session_for_write(epoch).await?;
        let visible = self
            .local_store
            .persist_local_collab_present(
                &item,
                "",
                created_seq,
                created_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?;
        self.publish_visible_state(visible).await;
        info!(object_id = %object_id, "Collab doc created");
        Ok(item)
    }

    /// Rename a collab doc. The server emits an `updated` event so the user's
    /// other devices re-read the meta; this device applies the response directly
    /// rather than waiting for its own event back (the server suppresses the
    /// originating device's broadcast, exactly as for `created`).
    pub async fn rename_collab_doc(
        &self,
        object_id: &str,
        title: &str,
    ) -> Result<CollabItem, ClientError> {
        // Read before the network work, so the persist below can tell whether
        // the session that renamed the doc is still the one running.
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let meta = self.api.rename_collab_doc(object_id, title).await?;
        if meta.object_id.to_string() != object_id {
            return Err(ClientError::UnexpectedResponse(format!(
                "renamed collab doc {object_id} returned mismatched identity"
            )));
        }
        let item = collab_item_from_meta(&meta);
        let created_seq = collab_created_seq(&item.created_at);
        // The rename must not reorder the list, so the record keeps its creation
        // seq. The event seq is a local wall-clock microsecond stamp, matching
        // what `create_collab_doc` and `delete_collab_doc` do — the rename
        // response carries no server seq. Caveat inherited from those paths: a
        // device clock running ahead of the server can stamp a seq high enough to
        // make a later remote delete look stale and be dropped, until the next
        // reconciliation sweep clears the record.
        let event_seq = chrono::Utc::now().timestamp_micros();
        let _session = self.hold_session_for_write(epoch).await?;
        let visible = self
            .local_store
            .persist_local_collab_present(&item, "", created_seq, event_seq, RECENT_CLIPBOARD_LIMIT)
            .await?;
        self.publish_visible_state(visible).await;
        info!(object_id = %object_id, "Collab doc renamed");
        Ok(item)
    }

    /// Delete a collab doc owned by the current user. Mirrors `delete_file`: the
    /// server's `deleted_seq` is not returned by this endpoint (it responds 204),
    /// so a local wall-clock microsecond seq tombstones the record; it is always
    /// later than the create seq, so the delete wins.
    pub async fn delete_collab_doc(&self, object_id: &str) -> Result<(), ClientError> {
        // Read before the network work, so the tombstone below can tell whether
        // the session that deleted the doc is still the one running.
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        self.api.delete_collab_doc(object_id).await?;
        let delete_seq = chrono::Utc::now().timestamp_micros();
        let _session = self.hold_session_for_write(epoch).await?;
        let visible = self
            .local_store
            .apply_local_delete(
                ObjectKind::Collab,
                object_id,
                delete_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?;
        self.publish_visible_state(visible).await;
        info!(object_id = %object_id, "Collab doc deleted");
        Ok(())
    }

    /// Fetch a collab doc's current metadata from the server. Used by the per-doc
    /// detail view, which needs the share token as the Y-sync WS credential.
    pub async fn get_collab_doc_meta(&self, object_id: &str) -> Result<CollabItem, ClientError> {
        let meta = self.api.get_collab_doc_meta(object_id).await?;
        Ok(collab_item_from_meta(&meta))
    }

    // ── Sync ──

    /// Ask the live WebSocket to reconnect, which restarts reconciliation.
    ///
    /// The counter is bumped in place. Reading the old value with `borrow()`
    /// inside the `send` call would hold the channel's read lock while `send`
    /// takes its write lock, and the caller would block there forever.
    pub async fn refresh(&self) -> Result<(), ClientError> {
        self.ws_restart_tx.send_modify(|requested| *requested += 1);
        Ok(())
    }

    async fn publish_visible_state(&self, mut visible: LocalVisibleState) {
        // No network work here: publication must not wait for historical reads
        // while newer sync snapshots are ready to publish.
        if let (Some(view), Some(planned)) = (&mut visible.running_actual, visible.running_plan) {
            let epoch = self.history_epoch.load(Ordering::SeqCst);
            if let Some(record) = self
                .schedule_history
                .lock()
                .await
                .get(&(epoch, planned.schedule))
                && let Some((id, title)) = record.planned_title()
                && id == planned.item
            {
                view.title = title.to_string();
            }
        }
        {
            let mut state = self.state.write().await;
            // Nothing to show without a session, and a straggling snapshot from
            // the previous one must not repopulate the screen after logout.
            if state.session.is_none() {
                return;
            }
            // Views are built under the store lock but published without one,
            // so an older view can arrive after a newer one. Drop it.
            if visible.stamp <= self.published_stamp.load(Ordering::SeqCst) {
                return;
            }
            self.published_stamp.store(visible.stamp, Ordering::SeqCst);
            state.clipboard_items = visible.clipboard_items;
            state.files = visible.files;
            state.collab_docs = visible.collab_docs;
            state.schedule_items = visible.schedule_items;
            state.calendar_sources = visible.calendar_sources;
            state.running_actual = visible.running_actual;
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
                file_engine
                    .end_refused_session_for(generation, &error)
                    .await;
            }
        });

        let clipboard_engine = Arc::clone(self);
        spawn_background(async move {
            if let Err(error) = clipboard_engine
                .snapshot_clipboard(generation, stream_start_seq)
                .await
            {
                warn!("Clipboard snapshot failed: {}", error);
                clipboard_engine
                    .end_refused_session_for(generation, &error)
                    .await;
            }
        });

        let collab_engine = Arc::clone(self);
        spawn_background(async move {
            if let Err(error) = collab_engine
                .snapshot_collab_docs(generation, stream_start_seq)
                .await
            {
                warn!("Collab doc snapshot failed: {}", error);
                collab_engine
                    .end_refused_session_for(generation, &error)
                    .await;
            }
        });

        let schedule_engine = Arc::clone(self);
        spawn_background(async move {
            if let Err(error) = schedule_engine
                .snapshot_schedule(generation, stream_start_seq)
                .await
            {
                warn!("Schedule snapshot failed: {}", error);
                schedule_engine
                    .end_refused_session_for(generation, &error)
                    .await;
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
                // The event's timestamp is not used: every object kind is
                // materialized from an endpoint that reports its own
                // authoritative `created_at`.
                created_at: _,
            }) => {
                debug!("WS event seq={} type={}", seq, event_type);
                match event_type {
                    ObjectEventType::Created => {
                        self.handle_created_event(generation, object_kind, object_id, seq)
                            .await?;
                    }
                    // A collab doc mutates in place, and only its plaintext
                    // metadata does, so it has its own handler.
                    ObjectEventType::Updated if object_kind == ObjectKind::Collab => {
                        self.handle_updated_collab_event(generation, object_id, seq);
                    }
                    // For every other kind an update means a new revision is
                    // the head. The ciphertext is still immutable; which
                    // ciphertext is current has moved, so the answer is the
                    // same as for a creation — fetch the object and replace the
                    // local copy with what comes back.
                    ObjectEventType::Updated
                        if object_kind == ObjectKind::File
                            || object_kind == ObjectKind::Schedule =>
                    {
                        self.handle_updated_object_event(generation, object_kind, object_id, seq)
                            .await?;
                    }
                    // Clipboard is the exception: it is replaced rather than
                    // revised, so an update for it is a server bug, not an edit.
                    ObjectEventType::Updated => {
                        warn!(
                            seq,
                            object_kind = %object_kind,
                            "Ignoring unsupported WS update event for object kind",
                        );
                    }
                    // File, collab and schedule are the deletable kinds.
                    // Clipboard items expire passively and never emit deletes.
                    ObjectEventType::Deleted
                        if object_kind == ObjectKind::File
                            || object_kind == ObjectKind::Collab
                            || object_kind == ObjectKind::Schedule =>
                    {
                        self.handle_deleted_event(generation, object_kind, object_id, seq)
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
            validate_snapshot_page(&page, after, stream_start_seq)?;
            for item in page.items {
                if let Err(error) = verify_object_list_item_envelope(&item) {
                    warn!(id = %item.id, "Rejected file object envelope: {}", error);
                    continue;
                }
                if let Err(error) = self.check_revision_advance(&item).await {
                    self.keep_held_revision(&item, generation, error).await?;
                    continue;
                }
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

    /// Reconcile the full set of collab docs from `GET /api/collab-docs`.
    ///
    /// Collab objects carry no ciphertext, so `GET /api/objects` filters them
    /// out and the file/clipboard snapshots never see them. Without this pass a
    /// device would only ever learn about docs it created itself or saw a live
    /// `created` event for — so a fresh install showed none, and a rename made
    /// while offline (past the event-log retention window) never landed.
    ///
    /// The listing is unpaged: it is bounded by the per-user object cap, and each
    /// row is a handful of short strings.
    async fn snapshot_collab_docs(
        self: &Arc<Self>,
        generation: u64,
        stream_start_seq: i64,
    ) -> Result<(), ClientError> {
        let listing = self.api.list_collab_docs().await?;
        for meta in &listing.docs {
            let item = collab_item_from_meta(meta);
            let created_seq = collab_created_seq(&item.created_at);
            self.persist_collab_snapshot_item(&item, created_seq, generation)
                .await?;
        }

        if let Some(visible) = self
            .local_store
            .sweep_kind(
                ObjectKind::Collab,
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
            validate_snapshot_page(&page, after, stream_start_seq)?;
            let mut objects = stream::iter(page.items)
                .map(|item| async move {
                    let created_seq = item.created_seq;
                    match self
                        .decrypt_clipboard_object_item_with_api(api, &item, encryption_key)
                        .await
                    {
                        Ok(object) => Ok((object, created_seq)),
                        Err(error) => Err((item, error)),
                    }
                })
                .buffer_unordered(CLIPBOARD_HYDRATION_CONCURRENCY);

            while let Some(loaded) = objects.next().await {
                match loaded {
                    Ok((object, created_seq)) => {
                        self.persist_clipboard_snapshot_item(&object, created_seq, generation)
                            .await?;
                    }
                    Err((item, error)) => {
                        if let Err(error) = self.keep_held_revision(&item, generation, error).await
                        {
                            warn!(id = %item.id, "Failed to load clipboard object: {}", error);
                        }
                    }
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

    /// Account for a snapshot item this pass will not install.
    ///
    /// A refused revision is an ordinary interleave: the page was built before
    /// a live event or this device's own write advanced the head. The object is
    /// still on the server, so it is marked as seen and the sweep leaves it
    /// alone, and the pass carries on. Any other error comes back unchanged.
    async fn keep_held_revision(
        &self,
        item: &ObjectListItem,
        generation: u64,
        error: ClientError,
    ) -> Result<(), ClientError> {
        let ClientError::RevisionRejected(reason) = &error else {
            return Err(error);
        };
        warn!(
            id = %item.id,
            served_revision = item.revision,
            "Kept the revision this device holds: {reason}",
        );
        self.local_store
            .mark_snapshot_seen(&item.id.to_string(), generation)
            .await?;
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

    async fn persist_collab_snapshot_item(
        &self,
        item: &CollabItem,
        created_seq: i64,
        generation: u64,
    ) -> Result<(), ClientError> {
        // The collab-doc meta endpoint does not expose the creating device, and
        // it is not surfaced in `CollabItem`, so the stored record carries an
        // empty source device id.
        if let Some(visible) = self
            .local_store
            .persist_snapshot_collab_present(
                item,
                "",
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
        self.check_revision_advance(item).await?;
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
            encrypted: EncryptedInlineObject {
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

    /// Refetch an object whose head moved to a new revision.
    ///
    /// Same materialisation as a creation: the object is pulled and the local
    /// copy replaced. It goes through `mark_pending_update`, because the
    /// create path ignores an object it already holds.
    async fn handle_updated_object_event(
        self: &Arc<Self>,
        generation: u64,
        kind: ObjectKind,
        object_id: ObjectId,
        event_seq: i64,
    ) -> Result<(), ClientError> {
        let object_id_text = object_id.to_string();
        let should_materialize = self
            .local_store
            .mark_pending_update(kind, &object_id_text, event_seq, generation)
            .await?;
        if !should_materialize {
            return Ok(());
        }

        let engine = Arc::clone(self);
        spawn_background(async move {
            if let Err(error) = engine
                .materialize_object(generation, kind, object_id, event_seq)
                .await
            {
                warn!(
                    object_id = %object_id,
                    event_seq,
                    "Failed to materialize revised object: {}",
                    error,
                );
            }
        });
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

    /// Re-read a renamed collab doc's metadata, off the event loop.
    ///
    /// Spawned rather than awaited, for the same two reasons `handle_created_event`
    /// spawns its materialization: an inline fetch would stall every later event
    /// behind it, and propagating a transient failure out of the receive loop
    /// would tear the WebSocket down. A rename missed this way is picked up by
    /// the collab snapshot on the next reconnect.
    fn handle_updated_collab_event(
        self: &Arc<Self>,
        generation: u64,
        object_id: ObjectId,
        event_seq: i64,
    ) {
        let engine = Arc::clone(self);
        spawn_background(async move {
            if let Err(error) = engine.materialize_collab(generation, object_id).await {
                warn!(
                    object_id = %object_id,
                    event_seq,
                    "Failed to re-read updated collab doc: {}",
                    error,
                );
            }
        });
    }

    async fn materialize_object(
        self: &Arc<Self>,
        generation: u64,
        kind: ObjectKind,
        object_id: ObjectId,
        event_seq: i64,
    ) -> Result<(), ClientError> {
        // Collab objects are server-visible documents, not end-to-end-encrypted
        // objects: they are never returned by the encrypted-object endpoints, so
        // they are materialized from the dedicated collab-doc meta endpoint
        // rather than `get_object`.
        if kind == ObjectKind::Collab {
            return self.materialize_collab(generation, object_id).await;
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
                let object = match self
                    .decrypt_clipboard_object_item_with_api(api, &item, &encryption_key)
                    .await
                {
                    Ok(object) => object,
                    Err(error) => return self.keep_held_revision(&item, generation, error).await,
                };
                self.persist_clipboard_snapshot_item(&object, item.created_seq, generation)
                    .await?;
            }
            ObjectKind::File => {
                if let Err(error) = self.check_revision_advance(&item).await {
                    return self.keep_held_revision(&item, generation, error).await;
                }
                let file = decrypt_file_object_item(&item, &encryption_key)?;
                self.persist_file_snapshot_item(
                    &file,
                    &encrypted_object_from_list_item(&item),
                    item.created_seq,
                    generation,
                )
                .await?;
            }
            ObjectKind::Schedule => {
                let (record, encrypted) = match self
                    .decrypt_schedule_object_item(api, &item, &encryption_key)
                    .await
                {
                    Ok(decrypted) => decrypted,
                    Err(error) => return self.keep_held_revision(&item, generation, error).await,
                };
                if let Some(visible) = self
                    .local_store
                    .persist_snapshot_schedule_present_encrypted(
                        StoredObjectIdentity {
                            object_id: &object_id_text,
                            created_at: &item.created_at,
                            source_device_id: &item.source_device_id.to_string(),
                        },
                        record,
                        &encrypted,
                        item.created_seq,
                        generation,
                        RECENT_CLIPBOARD_LIMIT,
                    )
                    .await?
                {
                    self.publish_visible_state(visible).await;
                }
            }
            // Routed to `materialize_collab` above before any network call; an
            // explicit arm keeps the match total without re-handling it.
            ObjectKind::Collab => {}
        }
        Ok(())
    }

    /// Materialize a collab doc from a live `created` or `updated` event. The
    /// encrypted-object endpoints exclude collab objects, so its metadata comes
    /// from `GET /api/collab-docs/:id/meta` instead. A 404 means the doc was
    /// deleted between the event and this fetch, so drop the local marker.
    async fn materialize_collab(
        self: &Arc<Self>,
        generation: u64,
        object_id: ObjectId,
    ) -> Result<(), ClientError> {
        let object_id_text = object_id.to_string();
        let meta = match self.api.get_collab_doc_meta(&object_id_text).await {
            Ok(meta) => meta,
            Err(error) if is_not_found_error(&error) => {
                self.remove_absent_object(generation, &object_id_text)
                    .await?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if meta.object_id != object_id {
            return Err(ClientError::UnexpectedResponse(format!(
                "materialized collab doc {object_id} returned mismatched identity"
            )));
        }
        let item = collab_item_from_meta(&meta);
        let created_seq = collab_created_seq(&item.created_at);
        self.persist_collab_snapshot_item(&item, created_seq, generation)
            .await?;
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

    /// Keep a WebSocket up for the session `epoch` names.
    ///
    /// Every login spawns one of these, so a logout followed by a new login
    /// leaves two running. The epoch check is how the older one stops: without
    /// it, it sees the new session's state and token and reconnects as the new
    /// user, and every event is then handled twice.
    #[cfg(not(target_family = "wasm"))]
    async fn ws_loop(self: &Arc<Self>, epoch: u64) {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        loop {
            if !self.session_is_current(epoch) {
                debug!("Stopping the WebSocket loop of a session that has ended");
                return;
            }
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

            match self.ws_connect(epoch).await {
                Ok(()) => {
                    backoff = Duration::from_secs(1);
                }
                Err(e) => {
                    warn!("WebSocket error: {}", e);
                    // A 401 here is the server saying this device's token is
                    // gone: removed from another device, or expired. Retrying
                    // would keep the account's keys and decrypted records
                    // resident until the process restarts.
                    if self.end_refused_session_for_epoch(epoch, &e).await {
                        return;
                    }
                    // The state belongs to whichever session is installed now.
                    // A socket whose session has ended must not mark the next
                    // session disconnected.
                    if !self.session_is_current(epoch) {
                        debug!("Stopping the WebSocket loop of a session that has ended");
                        return;
                    }
                    {
                        let mut state = self.state.write().await;
                        state.connection_status = ConnectionStatus::Disconnected;
                    }
                    self.bump_version();
                }
            }

            if !self.session_is_current(epoch) {
                debug!("Stopping the WebSocket loop of a session that has ended");
                return;
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
    async fn ws_connect(self: &Arc<Self>, epoch: u64) -> Result<(), ClientError> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite;

        // tokio-tungstenite builds its own rustls ClientConfig from the
        // process-default provider; ensure that's ring before connecting.
        crate::ensure_crypto_provider();

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

        let (ws_stream, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(websocket_handshake_error)?;

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

        // The session may have changed while the handshake was in flight.
        // Claiming the generation only for the session this socket
        // authenticated as keeps its events out of the next account.
        let generation = self.start_generation_for_session(epoch).await?;
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

    /// Keep a WebSocket up for the session `epoch` names.
    ///
    /// Every login spawns one of these, so a logout followed by a new login
    /// leaves two running. The epoch check is how the older one stops: without
    /// it, it sees the new session's state and token and reconnects as the new
    /// user, and every event is then handled twice.
    #[cfg(target_family = "wasm")]
    async fn ws_loop(self: &Arc<Self>, epoch: u64) {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        loop {
            if !self.session_is_current(epoch) {
                debug!("Stopping the WebSocket loop of a session that has ended");
                return;
            }
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

            match self.ws_connect(epoch).await {
                Ok(()) => {
                    backoff = Duration::from_secs(1);
                }
                Err(e) => {
                    warn!("WebSocket error: {}", e);
                    // A 401 here is the server saying this device's token is
                    // gone: removed from another device, or expired. Retrying
                    // would keep the account's keys and decrypted records
                    // resident until the process restarts.
                    if self.end_refused_session_for_epoch(epoch, &e).await {
                        return;
                    }
                    // The state belongs to whichever session is installed now.
                    // A socket whose session has ended must not mark the next
                    // session disconnected.
                    if !self.session_is_current(epoch) {
                        debug!("Stopping the WebSocket loop of a session that has ended");
                        return;
                    }
                    {
                        let mut state = self.state.write().await;
                        state.connection_status = ConnectionStatus::Disconnected;
                    }
                    self.bump_version();
                }
            }

            if !self.session_is_current(epoch) {
                debug!("Stopping the WebSocket loop of a session that has ended");
                return;
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
    async fn ws_connect(self: &Arc<Self>, epoch: u64) -> Result<(), ClientError> {
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

        // The session may have changed while the handshake was in flight.
        // Claiming the generation only for the session this socket
        // authenticated as keeps its events out of the next account.
        let generation = self.start_generation_for_session(epoch).await?;
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
) -> EncryptedInlineObject {
    EncryptedInlineObject {
        object: encrypted_object_from_init(init_req),
        payload_ciphertext,
    }
}

/// Parse a fetched feed.
///
fn parse_calendar_feed(
    text: &str,
    source: clipper_schedule::SourceId,
    import: ObjectId,
) -> Result<clipper_schedule::IngestOutcome, ClientError> {
    clipper_schedule::parse_ics(text, source, import)
        .map_err(|error| ClientError::Other(format!("calendar feed: {error}")))
}

/// Largest calendar feed the client will read. A feed is a remote document
/// fetched on a timer; without a ceiling a hostile or broken one could make the
/// client buffer arbitrarily many bytes.
///
/// Native-only, like the fetch it bounds — the browser build has no fetch to
/// bound, and an ungated constant is dead code there.
#[cfg(not(target_family = "wasm"))]
const MAX_CALENDAR_FEED_BYTES: usize = 8 * 1024 * 1024;

/// Fetch a calendar feed over plain HTTP.
///
/// Not available in the browser: a page cannot read an arbitrary third-party URL
/// without that server sending CORS headers, and calendar providers do not. This
/// is fine because each client decides which sources it is responsible
/// for — the desktop daemon and mobile can sync feeds, and the browser reads the
/// results like any other device.
#[cfg(not(target_family = "wasm"))]
async fn fetch_calendar_feed(url: &str) -> Result<String, ClientError> {
    crate::ensure_crypto_provider();
    let response = reqwest::Client::builder()
        .use_preconfigured_tls(crate::api_client::default_tls_config())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many calendar feed redirects")
            } else if attempt.url().scheme() != "https"
                && attempt.previous().iter().any(|url| url.scheme() == "https")
            {
                attempt.error("calendar feed redirect would downgrade HTTPS")
            } else {
                attempt.follow()
            }
        }))
        .build()?
        .get(url)
        .header("accept", "text/calendar, text/plain;q=0.9, */*;q=0.1")
        .send()
        .await
        .map_err(|error| ClientError::Http(error.without_url()))?
        .error_for_status()
        .map_err(|error| ClientError::Http(error.without_url()))?;

    if let Some(len) = response.content_length()
        && len > MAX_CALENDAR_FEED_BYTES as u64
    {
        return Err(ClientError::PayloadTooLarge {
            size: len as i64,
            limit: MAX_CALENDAR_FEED_BYTES as i64,
        });
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ClientError::Http(error.without_url()))?;
        let size = bytes.len().saturating_add(chunk.len());
        if size > MAX_CALENDAR_FEED_BYTES {
            return Err(ClientError::PayloadTooLarge {
                size: size as i64,
                limit: MAX_CALENDAR_FEED_BYTES as i64,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes)
        .map_err(|error| ClientError::Other(format!("calendar feed is not UTF-8: {error}")))
}

#[cfg(target_family = "wasm")]
async fn fetch_calendar_feed(_url: &str) -> Result<String, ClientError> {
    Err(ClientError::Unsupported(
        "Calendar feeds cannot be fetched from a browser: providers send no CORS \
         headers. Sync this source from the desktop or mobile app instead."
            .into(),
    ))
}

/// Parse an RFC 3339 instant supplied by a UI shell.
///
/// Shells pass timestamps as strings across the IPC, wasm and UniFFI
/// boundaries, so this is where a malformed one is caught and named.
fn parse_instant(
    text: &str,
    what: &'static str,
) -> Result<chrono::DateTime<chrono::Utc>, ClientError> {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|instant| instant.with_timezone(&chrono::Utc))
        .map_err(|error| ClientError::InvalidArgument(format!("{what}: {error}")))
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

fn encrypted_object_from_revise(req: &ObjectReviseRequest) -> EncryptedObject {
    EncryptedObject {
        meta_nonce: req.meta_nonce.clone(),
        meta_ciphertext: req.meta_ciphertext.clone(),
        payloads: req
            .payloads
            .iter()
            .map(|payload| ObjectPayloadDescriptor {
                id: payload.id,
                nonce: payload.nonce.clone(),
                ciphertext_size: payload.ciphertext_size,
                sha256_ciphertext: payload.sha256_ciphertext.clone(),
            })
            .collect(),
        created_at: req.envelope.body.created_at.clone(),
        source_device_id: req.envelope.body.source_device_id.to_string(),
        envelope: req.envelope.clone(),
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

/// The projection the AAD is computed from, before the ciphertexts exist.
///
/// It has to agree with the final envelope on every bound field — `revision`
/// and `parent_hash` included, which is why placement is threaded through here
/// rather than defaulted. Getting it wrong does not fail here; it fails as an
/// undecryptable object on some other device.
fn object_envelope_body_for_aad(
    object_id: ObjectId,
    kind: ObjectKind,
    placement: EnvelopePlacement,
    source_device_id: DeviceId,
    created_at: String,
    payload_ids: Vec<ObjectPayloadId>,
) -> ObjectEnvelopeBody {
    object_envelope_body(
        object_id,
        kind,
        placement,
        source_device_id,
        created_at,
        Vec::new(),
        Vec::new(),
        payload_ids
            .into_iter()
            .map(|id| ObjectEnvelopePayload {
                id,
                nonce: Vec::new(),
                ciphertext_size: 0,
                sha256_ciphertext: Vec::new(),
            })
            .collect(),
    )
}

/// Where a new envelope sits in its object's chain.
///
/// The revision number, the parent hash and the operation always move
/// together: a create has no parent, a revise and a tombstone both do. They
/// travel as one value rather than three arguments that could be combined into
/// something the server would reject.
#[derive(Debug, Clone, Copy)]
pub(crate) enum EnvelopePlacement {
    Create,
    Revise(LocalHead),
    Delete(LocalHead),
}

impl EnvelopePlacement {
    fn revision(self) -> u64 {
        match self {
            Self::Create => 1,
            Self::Revise(head) | Self::Delete(head) => head.revision + 1,
        }
    }

    fn parent_hash(self) -> Option<[u8; crypto::SHA256_BYTES]> {
        match self {
            Self::Create => None,
            Self::Revise(head) | Self::Delete(head) => Some(head.parent_hash),
        }
    }

    fn operation(self) -> ObjectEnvelopeOperation {
        match self {
            Self::Create => ObjectEnvelopeOperation::Create,
            Self::Revise(_) => ObjectEnvelopeOperation::Revise,
            Self::Delete(_) => ObjectEnvelopeOperation::Delete,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn object_envelope_body(
    object_id: ObjectId,
    kind: ObjectKind,
    placement: EnvelopePlacement,
    source_device_id: DeviceId,
    created_at: String,
    meta_nonce: Vec<u8>,
    sha256_meta_ciphertext: Vec<u8>,
    payloads: Vec<ObjectEnvelopePayload>,
) -> ObjectEnvelopeBody {
    ObjectEnvelopeBody {
        object_id,
        object_type: kind,
        envelope_version: crypto::OBJECT_ENVELOPE_VERSION,
        revision: placement.revision(),
        parent_hash: placement.parent_hash(),
        source_device_id,
        created_at,
        operation: placement.operation(),
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
        || body.envelope_version != crypto::OBJECT_ENVELOPE_VERSION
        // The revision is signed and also stated in the clear beside it; they
        // must agree, or the server could relabel which revision this is while
        // serving a genuinely signed body.
        || body.revision != item.revision
        // A listing serves live heads. A `Delete` here would mean the server
        // offered a tombstone as current content, and a `Create` above revision
        // 1 is a chain restarting on top of itself.
        || match body.operation {
            ObjectEnvelopeOperation::Create => body.revision != 1,
            ObjectEnvelopeOperation::Revise => body.revision < 2,
            ObjectEnvelopeOperation::Delete => true,
        }
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

/// The store owns the check; this keeps the envelope error type callers here
/// already handle.
fn verify_payload_hash(
    payload: &ObjectPayloadDescriptor,
    ciphertext: &[u8],
) -> Result<(), ClientError> {
    verify_payload_ciphertext(payload, ciphertext)
        .map_err(|error| object_envelope_error(error.to_string()))
}

/// The span a stopped timer records.
///
/// Clamped to at least one second, because a `TimeRange` must be non-empty and
/// the clock can be behind the start: a device whose time moved backwards, or a
/// timer started on a device running ahead, would otherwise be impossible to
/// stop until wall-clock time caught up. Stopping clamps
/// (`docs/schedule-model-review.md`).
fn stopped_span(
    started: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<TimeRange, ClientError> {
    let end = now.max(started + chrono::Duration::seconds(1));
    TimeRange::new(started, end).map_err(|error| ClientError::InvalidArgument(error.to_string()))
}

/// Refuse a file this client will not upload.
///
/// Checked before encryption: the whole ciphertext is built in memory, so an
/// oversized file should fail immediately rather than after the work.
fn check_upload_plaintext_size(size: usize) -> Result<(), ClientError> {
    if size > MAX_FILE_UPLOAD_PLAINTEXT_BYTES {
        return Err(ClientError::InvalidArgument(format!(
            "file is {size} bytes, over the {MAX_FILE_UPLOAD_PLAINTEXT_BYTES}-byte upload limit"
        )));
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

/// Build a display `CollabItem` from server meta.
fn collab_item_from_meta(meta: &CollabDocMeta) -> CollabItem {
    CollabItem {
        id: meta.object_id.to_string(),
        title: meta.title.clone(),
        share_token: meta.share_token.clone(),
        share_url: meta.share_url.clone(),
        created_at: meta.created_at.clone(),
        updated_at: meta.updated_at.clone(),
    }
}

/// Ordering key for a collab doc, derived from its server `created_at`.
///
/// Collab docs have no `created_seq` of their own: the meta and list endpoints
/// report timestamps, not seqs. A server seq is an application-assigned
/// microsecond wall clock, so microseconds-since-epoch from `created_at` sorts
/// consistently against one — and, unlike the event seq of whatever event
/// happened to surface the doc, it does not change when the doc is renamed. An
/// unparseable timestamp falls back to now, which sorts the doc newest rather
/// than dropping it.
fn collab_created_seq(created_at: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .map(|parsed| parsed.timestamp_micros())
        .unwrap_or_else(|_| {
            warn!(created_at, "Collab doc has an unparseable created_at");
            chrono::Utc::now().timestamp_micros()
        })
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

fn is_not_found_error(error: &ClientError) -> bool {
    matches!(error, ClientError::Api { status, .. } if *status == 404)
}

/// The server refused this session's token. Only an HTTP 401 says that: a
/// transport error, a closed WebSocket, or any other status is a reason to
/// retry, not to sign out.
fn session_refused(error: &ClientError) -> bool {
    matches!(error, ClientError::Api { status, .. } if *status == 401)
}

/// Turn a WebSocket handshake failure into the error the rest of the client
/// reasons about.
///
/// A handshake the server rejected carries an HTTP response, and `/api/ws`
/// sits behind the same auth middleware as every other private route: a
/// revoked or expired token is answered with 401 there too. Flattening that
/// into a string would lose the status, and the loop would keep retrying a
/// session the server has already ended.
#[cfg(not(target_family = "wasm"))]
fn websocket_handshake_error(error: tokio_tungstenite::tungstenite::Error) -> ClientError {
    use tokio_tungstenite::tungstenite;

    let tungstenite::Error::Http(response) = error else {
        return ClientError::WebSocket(error.to_string());
    };
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(tungstenite::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    crate::api_client::api_error_from_parts(
        status,
        content_type,
        response.body().as_deref().unwrap_or_default(),
    )
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

#[cfg(all(test, not(target_family = "wasm")))]
#[path = "schedule_integration_tests.rs"]
mod schedule_integration_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_pages_must_advance_inside_the_watermark() {
        let mut item = signed_item_with_payload_count(1);
        item.created_seq = 10;
        let cursor = ObjectListCursor {
            created_seq: 10,
            id: item.id,
        };
        let mut page = ObjectListResponse {
            items: vec![item],
            next_after: Some(cursor),
        };
        validate_snapshot_page(&page, None, 10).expect("valid first page");
        assert!(validate_snapshot_page(&page, Some(cursor), 10).is_err());
        assert!(validate_snapshot_page(&page, None, 9).is_err());
        page.next_after = Some(ObjectListCursor {
            created_seq: 9,
            ..cursor
        });
        assert!(validate_snapshot_page(&page, None, 10).is_err());
        page.items.clear();
        assert!(validate_snapshot_page(&page, None, 10).is_err());
        page.next_after = None;
        validate_snapshot_page(&page, Some(cursor), 10).expect("empty last page");
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn calendar_fetch_errors_do_not_expose_the_private_url() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let addr = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.expect("read") > 0);
            socket
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("respond");
        });
        let error = fetch_calendar_feed(&format!(
            "http://{addr}/private-calendar-token.ics?secret=bearer"
        ))
        .await
        .expect_err("forbidden");
        assert!(!error.to_string().contains("private-calendar-token"));
        assert!(!format!("{error:?}").contains("bearer"));
        server.await.expect("server");
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn calendar_fetch_bounds_chunked_bodies_before_reading_to_end() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let addr = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.expect("read") > 0);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .expect("header");
            let chunk = vec![b'x'; 1024 * 1024];
            for _ in 0..9 {
                if socket.write_all(b"100000\r\n").await.is_err()
                    || socket.write_all(&chunk).await.is_err()
                    || socket.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
            }
            // Never finish the body. A check performed only after bytes()
            // would wait forever instead of enforcing the bound.
            std::future::pending::<()>().await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            fetch_calendar_feed(&format!("http://{addr}/feed.ics")),
        )
        .await
        .expect("reject before EOF");
        assert!(matches!(result, Err(ClientError::PayloadTooLarge { .. })));
        server.abort();
    }

    fn descriptor(id: ObjectPayloadId) -> ObjectPayloadDescriptor {
        ObjectPayloadDescriptor {
            id,
            nonce: vec![0_u8; crypto::XCHACHA20_NONCE_BYTES],
            ciphertext_size: 0,
            sha256_ciphertext: vec![0_u8; crypto::SHA256_BYTES],
        }
    }

    fn envelope_payload(id: ObjectPayloadId) -> ObjectEnvelopePayload {
        ObjectEnvelopePayload {
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
        signed_item_with_version(count, crypto::OBJECT_ENVELOPE_VERSION)
    }

    fn signed_item_with_version(count: usize, version: u64) -> ObjectListItem {
        let object_id: ObjectId = uuid::Uuid::now_v7().into();
        let device_id: DeviceId = uuid::Uuid::now_v7().into();
        let signing_key = crypto::generate_device_signing_secret_key();
        let public_key = crypto::device_signing_public_key(&signing_key);
        let payload_ids: Vec<ObjectPayloadId> =
            (0..count).map(|_| uuid::Uuid::now_v7().into()).collect();
        let body = ObjectEnvelopeBody {
            object_id,
            object_type: ObjectKind::Clipboard,
            envelope_version: version,
            revision: 1,
            parent_hash: None,
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
            revision: 1,
            created_seq: 1,
            meta_nonce: vec![0_u8; crypto::XCHACHA20_NONCE_BYTES],
            meta_ciphertext: Vec::new(),
            payloads: payload_ids.iter().copied().map(descriptor).collect(),
            created_at: "2026-06-13T00:00:00Z".into(),
            source_device_id: device_id,
            source_device_signing_public_key: Some(public_key.to_vec()),
            envelope: ObjectEnvelope { body, signature },
        }
    }

    #[test]
    fn historical_reads_require_the_exact_pinned_signed_body() {
        let mut item = signed_item_with_payload_count(1);
        let key = crypto::generate_device_signing_secret_key();
        item.kind = ObjectKind::Schedule;
        item.envelope.body.object_type = ObjectKind::Schedule;
        item.source_device_signing_public_key =
            Some(crypto::device_signing_public_key(&key).to_vec());
        item.envelope.signature =
            crypto::sign_object_envelope_body(&key, &item.envelope.body).unwrap();
        let pin = clipper_schedule::ObjectRevisionRef {
            object_id: item.id,
            revision: item.revision,
            body_hash: crypto::object_envelope_parent_hash(&item.envelope.body).unwrap(),
        };
        schedule_context::verify_pin(&item, pin).unwrap();
        let mut wrong = pin;
        wrong.revision += 1;
        assert!(schedule_context::verify_pin(&item, wrong).is_err());
        wrong = pin;
        wrong.object_id = uuid::Uuid::new_v4().into();
        assert!(schedule_context::verify_pin(&item, wrong).is_err());
        // Even a correctly re-signed replacement is not the accepted content.
        item.created_at = "2027-01-01T00:00:00Z".into();
        item.envelope.body.created_at = item.created_at.clone();
        item.envelope.signature =
            crypto::sign_object_envelope_body(&key, &item.envelope.body).unwrap();
        assert!(schedule_context::verify_pin(&item, pin).is_err());
    }

    #[test]
    fn envelope_verification_accepts_initial_format_and_rejects_unknown_versions() {
        assert_eq!(crypto::OBJECT_ENVELOPE_VERSION, 1);
        verify_object_list_item_envelope(&signed_item_with_version(1, 1))
            .expect("initial format must verify");
        for version in [0, 2, u64::MAX] {
            // Sign the actual unsupported version so this exercises format
            // rejection, not rejection of a tampered signature.
            verify_object_list_item_envelope(&signed_item_with_version(1, version))
                .expect_err("unsupported format must be rejected");
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

    fn visible_state(stamp: u64, text: &str) -> LocalVisibleState {
        LocalVisibleState {
            stamp,
            clipboard_items: vec![DecryptedClipboardItem {
                id: "11111111-1111-4111-8111-111111111111".into(),
                text: text.into(),
                mime_type: "text/plain".into(),
                payload_size: text.len() as i64,
                created_at: "2026-01-22T00:00:00+00:00".into(),
                source_device_id: "22222222-2222-4222-8222-222222222222".into(),
            }],
            files: Vec::new(),
            collab_docs: Vec::new(),
            schedule_items: Vec::new(),
            calendar_sources: Vec::new(),
            running_actual: None,
            running_plan: None,
        }
    }

    async fn open_session(engine: &Arc<SyncEngine>) {
        engine.state.write().await.session = Some(AuthenticatedSession {
            username: "tester".into(),
            device_id: "22222222-2222-4222-8222-222222222222".into(),
            device_name: "test".into(),
            server_url: "http://127.0.0.1:8787".into(),
        });
    }

    #[test]
    fn stopping_a_timer_clamps_a_clock_that_runs_behind() {
        let now = chrono::Utc::now();
        // A timer started five minutes in the future by a device running ahead.
        let span = stopped_span(now + chrono::Duration::minutes(5), now).expect("clamped span");
        assert_eq!(span.start(), now + chrono::Duration::minutes(5));
        assert_eq!(span.end() - span.start(), chrono::Duration::seconds(1));
        // An ordinary stop still ends now.
        let span = stopped_span(now - chrono::Duration::minutes(5), now).expect("span");
        assert_eq!(span.end(), now);
    }

    #[tokio::test]
    async fn logout_fences_sync_writes_that_are_still_in_flight() {
        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", temp.path());
        engine.local_store.set_profile("profile-a".into());
        open_session(&engine).await;
        let generation = engine.local_store.start_generation().await;

        engine.logout().await.expect("logout clears local state");

        assert!(
            !engine
                .local_store
                .mark_pending_create(
                    ObjectKind::Clipboard,
                    "33333333-3333-4333-8333-333333333333",
                    1,
                    generation,
                )
                .await
                .expect("marker"),
            "a write from the logged-out session must not land",
        );
        engine
            .publish_visible_state(visible_state(1, "stale"))
            .await;
        assert!(
            engine.get_state().await.clipboard_items.is_empty(),
            "a straggling snapshot must not repopulate the screen",
        );
    }

    /// A snapshot writer that has already passed its generation check and is
    /// waiting on the database must not put the signed-out account's records
    /// back into memory after logout has cleared it.
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn logout_clears_memory_after_a_writer_already_past_its_generation_check() {
        use super::adversarial_history_tests::{
            HISTORY_TEST_DEVICE_ID, HISTORY_TEST_KEY, encrypted_schedule_object,
        };

        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:1", temp.path());
        engine.local_store.set_profile("profile-a".into());
        open_session(&engine).await;
        *engine.encryption_key.write().await = Some(Zeroizing::new(HISTORY_TEST_KEY));
        let generation = engine.local_store.start_generation().await;

        // Hold the database, so the persist below parks between its generation
        // check and the row it writes.
        let (entered, held) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let holder = {
            let engine = Arc::clone(&engine);
            tokio::spawn(async move {
                engine
                    .local_store
                    .hold_database_for_test(entered, released)
                    .await;
            })
        };
        held.await.expect("the database is held");

        let object_id = uuid::Uuid::now_v7().to_string();
        let record = ScheduleRecord::Item(Box::new(ScheduleItem {
            id: clipper_schedule::ScheduleItemId::new(),
            title: "signed-out secret".into(),
            span: ScheduleSpan::Timed {
                start: clipper_schedule::TimedStart::Floating(
                    (chrono::Utc::now() + chrono::TimeDelta::hours(1)).naive_utc(),
                ),
                duration: clipper_schedule::BlockDuration::from_minutes(30).expect("duration"),
            },
            recurrence: clipper_schedule::Recurrence::Once,
            reference: None,
            alarm: Some(clipper_schedule::AlarmPolicy::at_start()),
        }));
        let encrypted = encrypted_schedule_object(&record, &object_id, 1, None);
        let persist = engine
            .local_store
            .persist_snapshot_schedule_present_encrypted(
                StoredObjectIdentity {
                    object_id: &object_id,
                    created_at: "2026-09-12T00:00:00Z",
                    source_device_id: HISTORY_TEST_DEVICE_ID,
                },
                record,
                &encrypted,
                10,
                generation,
                RECENT_CLIPBOARD_LIMIT,
            );
        tokio::pin!(persist);
        assert!(
            futures_util::poll!(&mut persist).is_pending(),
            "the writer must be inside the store, past its generation check",
        );

        let clearing = engine.clear_local_session();
        tokio::pin!(clearing);
        assert!(
            futures_util::poll!(&mut clearing).is_pending(),
            "logout must wait for that writer rather than clear around it",
        );
        assert!(engine.encryption_key.read().await.is_none());

        release.send(()).expect("release the database");
        holder.await.expect("holder");
        persist.await.expect("persist");
        clearing.await;

        assert!(!engine.get_state().await.is_logged_in());
        assert!(
            engine.local_store.schedule_records_with_ids().await.is_empty(),
            "logout must leave no record of the signed-out account in memory",
        );
        assert!(
            engine.next_alarms(3, "UTC").await.expect("alarms").is_empty(),
            "and no alarm of that account can still be read without a session",
        );
    }


    /// A refresh has to return. It runs on the caller's task — the daemon's
    /// IPC handler, or the mobile and browser bridges — and a blocked one
    /// takes that thread down with it. This test drives it on a thread of its
    /// own, so a blocked refresh fails the test instead of hanging the binary.
    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn refresh_does_not_block_on_the_restart_channel() {
        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", temp.path());
        let (refreshed, refreshing) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime");
            runtime.block_on(engine.refresh()).expect("refresh");
            refreshed.send(()).expect("report the refresh");
        });

        refreshing
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("refresh must return rather than block on its own borrow");
    }

    const RETAIN_TEST_KEY: [u8; 32] = [7; 32];

    /// A signed file list item whose meta decrypts under `RETAIN_TEST_KEY`, so
    /// a retention can run end to end without a server.
    fn signed_file_item(filename: &str) -> ObjectListItem {
        let object_id: ObjectId = uuid::Uuid::now_v7().into();
        let payload_id: ObjectPayloadId = uuid::Uuid::now_v7().into();
        let device_id: DeviceId = uuid::Uuid::now_v7().into();
        let signing_key = crypto::generate_device_signing_secret_key();
        let public_key = crypto::device_signing_public_key(&signing_key);
        let created_at = "2026-09-12T00:00:00Z".to_string();
        let aad_body = object_envelope_body_for_aad(
            object_id,
            ObjectKind::File,
            EnvelopePlacement::Create,
            device_id,
            created_at.clone(),
            vec![payload_id],
        );
        let meta = FileMeta {
            filename: filename.into(),
            mime_type: "text/plain".into(),
            size: Some(0),
        };
        let (meta_nonce, meta_ciphertext) =
            encrypt_file_meta_bytes(&meta, &RETAIN_TEST_KEY, &aad_body).expect("meta encrypt");
        let payload = ObjectEnvelopePayload {
            id: payload_id,
            nonce: vec![0_u8; crypto::XCHACHA20_NONCE_BYTES],
            ciphertext_size: 0,
            sha256_ciphertext: crypto::sha256(&[]).to_vec(),
        };
        let body = object_envelope_body(
            object_id,
            ObjectKind::File,
            EnvelopePlacement::Create,
            device_id,
            created_at.clone(),
            meta_nonce.clone(),
            crypto::sha256(&meta_ciphertext).to_vec(),
            vec![payload.clone()],
        );
        let signature = crypto::sign_object_envelope_body(&signing_key, &body).expect("sign");
        ObjectListItem {
            id: object_id,
            kind: ObjectKind::File,
            revision: 1,
            created_seq: 1,
            meta_nonce,
            meta_ciphertext,
            payloads: vec![ObjectPayloadDescriptor {
                id: payload_id,
                nonce: payload.nonce,
                ciphertext_size: payload.ciphertext_size,
                sha256_ciphertext: payload.sha256_ciphertext,
            }],
            created_at,
            source_device_id: device_id,
            source_device_signing_public_key: Some(public_key.to_vec()),
            envelope: ObjectEnvelope { body, signature },
        }
    }

    #[tokio::test]
    async fn a_download_that_outlives_its_session_is_not_retained() {
        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", temp.path());
        engine.local_store.set_profile("profile-a".into());
        open_session(&engine).await;
        let epoch = engine.history_epoch.load(Ordering::SeqCst);

        // The download was started here; the user then logged out and logged
        // in as someone else while the bytes were still coming.
        engine.logout().await.expect("logout clears local state");
        engine.local_store.set_profile("profile-b".into());
        open_session(&engine).await;

        let item = signed_file_item("account-a-secret.txt");
        engine
            .retain_downloaded_file(&item, &RETAIN_TEST_KEY, epoch)
            .await
            .expect("a fenced retention is not a failure");

        assert!(
            engine
                .local_store
                .local_head(&item.id.to_string())
                .await
                .expect("local head")
                .is_none(),
            "the record must not land in the profile of the session that replaced it",
        );
        assert!(
            engine.get_state().await.files.is_empty(),
            "the filename must not be published into the new session's files",
        );
    }

    #[tokio::test]
    async fn a_download_that_finishes_inside_its_session_is_retained() {
        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", temp.path());
        engine.local_store.set_profile("profile-a".into());
        open_session(&engine).await;
        let epoch = engine.history_epoch.load(Ordering::SeqCst);

        let item = signed_file_item("still-mine.txt");
        engine
            .retain_downloaded_file(&item, &RETAIN_TEST_KEY, epoch)
            .await
            .expect("retention");

        assert_eq!(
            engine
                .local_store
                .local_head(&item.id.to_string())
                .await
                .expect("local head")
                .map(|head| head.revision),
            Some(1),
            "an unchanged session must still advance the revision anchor",
        );
        assert_eq!(
            engine
                .get_state()
                .await
                .files
                .first()
                .map(|file| file.filename.clone()),
            Some("still-mine.txt".to_string()),
        );
    }

    /// The upload's HTTP round trip is held open while the user logs out and
    /// logs in as someone else, so the response comes back into a session that
    /// is no longer the one that encrypted the file.
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn an_upload_that_outlives_its_session_is_not_persisted() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let temp = tempfile::tempdir().expect("tempdir");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let engine = SyncEngine::new_with_data_dir(
            &format!("http://{}", listener.local_addr().expect("address")),
            temp.path(),
        );
        engine.local_store.set_profile("profile-a".into());
        open_session(&engine).await;
        *engine.encryption_key.write().await = Some(Zeroizing::new([7; 32]));
        *engine.device_signing_key.write().await =
            Some(crypto::generate_device_signing_secret_key().into());
        engine.api.restore_token("session-a".into());

        let (sent, received) = tokio::sync::oneshot::channel();
        let (release, resumed) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            let body = loop {
                let read = socket.read(&mut buffer).await.expect("read");
                assert!(read > 0, "the upload request ended before its body");
                request.extend_from_slice(&buffer[..read]);
                let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .expect("a content length")
                    .trim()
                    .parse()
                    .expect("a numeric content length");
                if request.len() >= end + 4 + length {
                    break request[end + 4..end + 4 + length].to_vec();
                }
            };
            let init: ObjectInitRequest = postcard::from_bytes(&body).expect("an init request");
            sent.send(init.id.to_string()).expect("report the object id");

            // Answer only once the replacement session is installed.
            resumed.await.expect("release");
            let response = postcard::to_allocvec(&ObjectInitResponse::Complete { created_seq: 100 })
                .expect("encode the response");
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {POSTCARD_CONTENT_TYPE}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response.len(),
                    )
                    .as_bytes(),
                )
                .await
                .expect("response headers");
            socket.write_all(&response).await.expect("response body");
        });

        let writer = {
            let engine = Arc::clone(&engine);
            tokio::spawn(async move {
                engine
                    .upload_file_bytes("account-a-secret.txt", None, b"private account A")
                    .await
            })
        };
        let file_id = received.await.expect("the request reached the server");

        {
            let _calendar = engine.calendar_write.lock().await;
            engine.clear_local_session().await;
            engine.api.restore_token("session-b".into());
            engine
                .finish_auth(
                    "device-b",
                    "account-b".into(),
                    uuid::Uuid::now_v7().to_string(),
                    Zeroizing::new([8; 32]),
                    Zeroizing::new([9; 32]),
                    DeviceSigningIdentity {
                        device_id: None,
                        signing_secret_key: crypto::generate_device_signing_secret_key().into(),
                    },
                )
                .await
                .expect("the replacement session signs in");
        }
        release.send(()).expect("answer the upload");

        assert!(
            matches!(
                writer.await.expect("the upload task"),
                Err(ClientError::NotAuthenticated),
            ),
            "an upload whose session ended must fail instead of persisting",
        );
        server.await.expect("server");
        assert!(
            engine
                .local_store
                .local_head(&file_id)
                .await
                .expect("local head")
                .is_none(),
            "the record must not land in the profile of the session that replaced it",
        );
        let state = engine.get_state().await;
        assert_eq!(
            state.session.expect("the replacement session").username,
            "account-b",
        );
        assert!(
            state.files.is_empty(),
            "the filename must not be published into the new session's files",
        );
    }

    #[tokio::test]
    async fn a_refusal_from_a_replaced_session_does_not_sign_out_the_new_one() {
        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", temp.path());
        open_session(&engine).await;
        let refused = ClientError::Api {
            status: 401,
            error: ErrorResponse::new(ApiErrorCode::Unauthorized, "expired"),
        };

        // The snapshot request went out under this generation, and a new
        // session claimed the store before its 401 came back.
        let refused_generation = engine.local_store.start_generation().await;
        let current_generation = engine.local_store.start_generation().await;
        assert!(
            !engine
                .end_refused_session_for(refused_generation, &refused)
                .await,
            "a refusal aimed at a replaced session must not sign the new one out",
        );
        assert!(engine.get_state().await.session.is_some());

        // A refusal that does belong to the current session still ends it.
        assert!(
            engine
                .end_refused_session_for(current_generation, &refused)
                .await,
        );
        assert!(engine.get_state().await.session.is_none());
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn a_refusal_waits_for_a_session_change_already_running() {
        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", temp.path());
        open_session(&engine).await;
        let generation = engine.local_store.start_generation().await;
        let refused = ClientError::Api {
            status: 401,
            error: ErrorResponse::new(ApiErrorCode::Unauthorized, "expired"),
        };

        let held = engine.calendar_write.lock().await;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                engine.end_refused_session_for(generation, &refused),
            )
            .await
            .is_err(),
            "a refusal must not tear down a session while a login or logout is running",
        );
        drop(held);
        assert!(engine.get_state().await.session.is_some());
    }

    #[tokio::test]
    async fn a_socket_from_a_replaced_session_does_not_claim_the_store() {
        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", temp.path());
        let epoch = engine.history_epoch.load(Ordering::SeqCst);
        let generation = engine.local_store.start_generation().await;

        // The handshake finished after a logout and a new login.
        engine.history_epoch.fetch_add(1, Ordering::SeqCst);
        assert!(
            matches!(
                engine.start_generation_for_session(epoch).await,
                Err(ClientError::NotAuthenticated),
            ),
            "a socket of an ended session must not be given a generation",
        );
        assert_eq!(
            engine.local_store.current_generation().await,
            generation,
            "and the store must still be fenced on the current session's generation",
        );

        let current = engine.history_epoch.load(Ordering::SeqCst);
        assert_eq!(
            engine
                .start_generation_for_session(current)
                .await
                .expect("the current session gets a generation"),
            generation + 1,
        );
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn the_websocket_loop_of_an_ended_session_stops_instead_of_reconnecting() {
        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", temp.path());
        let epoch = engine.history_epoch.load(Ordering::SeqCst);
        open_session(&engine).await;
        // A logout and a new login happened after this loop was spawned, so
        // the session it is still holding state for belongs to someone else.
        engine.history_epoch.fetch_add(1, Ordering::SeqCst);
        let version_before = engine.state_version();

        tokio::time::timeout(Duration::from_millis(500), engine.ws_loop(epoch))
            .await
            .expect("the loop of an ended session must return, not reconnect");
        assert_eq!(
            engine.state_version(),
            version_before,
            "and it must not report a connection attempt on the new session's behalf",
        );
    }

    #[tokio::test]
    async fn a_late_view_does_not_replace_a_newer_one() {
        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", temp.path());
        open_session(&engine).await;

        engine
            .publish_visible_state(visible_state(2, "newer"))
            .await;
        engine
            .publish_visible_state(visible_state(1, "older"))
            .await;

        let state = engine.get_state().await;
        assert_eq!(
            state.clipboard_items.first().map(|item| item.text.clone()),
            Some("newer".to_string()),
        );
    }

    #[test]
    fn only_a_refused_token_ends_the_session() {
        assert!(session_refused(&ClientError::Api {
            status: 401,
            error: ErrorResponse::new(ApiErrorCode::Unauthorized, "expired"),
        }));
        // Everything else is a reason to retry, not to sign out.
        assert!(!session_refused(&ClientError::Api {
            status: 403,
            error: ErrorResponse::new(ApiErrorCode::Unknown, "forbidden"),
        }));
        assert!(!session_refused(&ClientError::WebSocket("closed".into())));
        assert!(!session_refused(&ClientError::NotAuthenticated));
    }

    /// The handshake is the one place a 401 arrives as a tungstenite error
    /// rather than an API response, and the status has to survive the
    /// conversion or the loop retries a session the server has ended.
    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn a_refused_websocket_handshake_is_a_refused_session() {
        use tokio_tungstenite::tungstenite;

        let refused = tungstenite::http::Response::builder()
            .status(401)
            .header("content-type", "application/json")
            .body(Some(
                br#"{"code":"unauthorized","message":"Unauthorized"}"#.to_vec(),
            ))
            .expect("a rejected handshake response");
        let error = websocket_handshake_error(tungstenite::Error::Http(Box::new(refused)));
        assert!(
            session_refused(&error),
            "a 401 handshake must end the session instead of being retried: {error:?}",
        );

        // Everything else is still a reason to retry.
        let unavailable = tungstenite::http::Response::builder()
            .status(503)
            .body(None)
            .expect("a rejected handshake response");
        assert!(!session_refused(&websocket_handshake_error(
            tungstenite::Error::Http(Box::new(unavailable)),
        )));
        assert!(!session_refused(&websocket_handshake_error(
            tungstenite::Error::ConnectionClosed,
        )));
    }

    #[tokio::test]
    async fn a_websocket_refusal_from_a_replaced_session_does_not_sign_out_the_new_one() {
        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", temp.path());
        open_session(&engine).await;
        let refused = ClientError::Api {
            status: 401,
            error: ErrorResponse::new(ApiErrorCode::Unauthorized, "expired"),
        };

        // The socket handshook under this epoch, and a new session was
        // installed before its 401 came back.
        let replaced = engine.history_epoch.load(Ordering::SeqCst);
        engine.history_epoch.fetch_add(1, Ordering::SeqCst);
        assert!(
            !engine
                .end_refused_session_for_epoch(replaced, &refused)
                .await,
            "a refusal aimed at a replaced session must not sign the new one out",
        );
        assert!(engine.get_state().await.session.is_some());

        // A refusal that does belong to the current session still ends it.
        let current = engine.history_epoch.load(Ordering::SeqCst);
        assert!(
            engine
                .end_refused_session_for_epoch(current, &refused)
                .await,
        );
        assert!(engine.get_state().await.session.is_none());
    }

    /// A device whose token the owner revoked from another device is refused
    /// at the upgrade. The loop has to sign out rather than sit on the keys
    /// showing "logged in, disconnected".
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn a_websocket_the_server_refuses_ends_the_session() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let temp = tempfile::tempdir().expect("tempdir");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let engine = SyncEngine::new_with_data_dir(
            &format!("http://{}", listener.local_addr().expect("address")),
            temp.path(),
        );
        open_session(&engine).await;
        engine.api.restore_token("revoked".into());
        let epoch = engine.history_epoch.load(Ordering::SeqCst);

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.expect("read") > 0);
            socket
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .expect("refuse the upgrade");
        });

        tokio::time::timeout(Duration::from_secs(5), engine.ws_loop(epoch))
            .await
            .expect("a refused socket must end the loop, not reconnect forever");
        server.await.expect("server");

        let state = engine.get_state().await;
        assert!(
            !state.is_logged_in(),
            "a refused session must be torn down, not left disconnected",
        );
        assert!(engine.encryption_key.read().await.is_none());
    }

    #[tokio::test]
    async fn a_refused_session_is_torn_down_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", temp.path());
        open_session(&engine).await;
        let refused = ClientError::Api {
            status: 401,
            error: ErrorResponse::new(ApiErrorCode::Unauthorized, "expired"),
        };
        assert!(engine.end_refused_session(&refused).await);
        assert!(engine.get_state().await.session.is_none());

        open_session(&engine).await;
        assert!(
            !engine
                .end_refused_session(&ClientError::WebSocket("closed".into()))
                .await,
        );
        assert!(engine.get_state().await.session.is_some());
    }

    #[test]
    fn an_oversized_upload_is_refused_before_encryption() {
        check_upload_plaintext_size(MAX_FILE_UPLOAD_PLAINTEXT_BYTES).expect("at the limit");
        let error = check_upload_plaintext_size(MAX_FILE_UPLOAD_PLAINTEXT_BYTES + 1)
            .expect_err("over the limit");
        assert!(
            matches!(error, ClientError::InvalidArgument(ref message) if message.contains("upload limit")),
            "unexpected error: {error:?}",
        );
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

#[cfg(all(test, not(target_family = "wasm")))]
mod adversarial_history_tests {
    use super::*;

    pub(super) const HISTORY_TEST_KEY: [u8; 32] = [1; 32];
    pub(super) const HISTORY_TEST_DEVICE_ID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    fn source_record(name: &str) -> ScheduleRecord {
        ScheduleRecord::Source(Box::new(CalendarSource {
            id: SourceId::new(),
            name: name.into(),
            kind: SourceKind::Ics {
                url: "https://example.invalid/calendar".into(),
            },
            enabled: true,
            active_import: None,
            pending_import: None,
            retired_imports: Vec::new(),
        }))
    }

    pub(super) fn encrypted_schedule_object(
        record: &ScheduleRecord,
        object_id: &str,
        revision: u64,
        parent_hash: Option<[u8; crypto::SHA256_BYTES]>,
    ) -> EncryptedInlineObject {
        let object_id_typed: ObjectId = object_id.parse().expect("object id");
        let device_id: DeviceId = HISTORY_TEST_DEVICE_ID.parse().expect("device id");
        let payload_id: ObjectPayloadId = uuid::Uuid::now_v7().into();
        let aad_body = ObjectEnvelopeBody {
            object_id: object_id_typed,
            object_type: ObjectKind::Schedule,
            envelope_version: crypto::OBJECT_ENVELOPE_VERSION,
            revision,
            parent_hash,
            source_device_id: device_id,
            created_at: "2026-09-12T00:00:00Z".into(),
            operation: if revision == 1 {
                ObjectEnvelopeOperation::Create
            } else {
                ObjectEnvelopeOperation::Revise
            },
            meta_nonce: Vec::new(),
            sha256_meta_ciphertext: Vec::new(),
            payloads: vec![ObjectEnvelopePayload {
                id: payload_id,
                nonce: Vec::new(),
                ciphertext_size: 0,
                sha256_ciphertext: Vec::new(),
            }],
        };
        let (meta_nonce, meta_ciphertext) =
            encrypt_schedule_meta(&record.meta(), &HISTORY_TEST_KEY, &aad_body)
                .expect("meta encrypt");
        let (payload_nonce, payload_ciphertext) =
            encrypt_schedule_payload(record, &HISTORY_TEST_KEY, &aad_body, payload_id)
                .expect("payload encrypt");
        let envelope_payload = ObjectEnvelopePayload {
            id: payload_id,
            nonce: payload_nonce.clone(),
            ciphertext_size: payload_ciphertext.len() as i64,
            sha256_ciphertext: crypto::sha256(&payload_ciphertext).to_vec(),
        };
        let body = ObjectEnvelopeBody {
            meta_nonce: meta_nonce.clone(),
            sha256_meta_ciphertext: crypto::sha256(&meta_ciphertext).to_vec(),
            payloads: vec![envelope_payload],
            ..aad_body
        };
        EncryptedInlineObject {
            object: EncryptedObject {
                meta_nonce,
                meta_ciphertext,
                payloads: vec![ObjectPayloadDescriptor {
                    id: payload_id,
                    nonce: payload_nonce,
                    ciphertext_size: payload_ciphertext.len() as i64,
                    sha256_ciphertext: crypto::sha256(&payload_ciphertext).to_vec(),
                }],
                created_at: "2026-09-12T00:00:00Z".into(),
                source_device_id: HISTORY_TEST_DEVICE_ID.into(),
                envelope: ObjectEnvelope {
                    body,
                    signature: vec![0; crypto::OBJECT_ENVELOPE_SIGNATURE_BYTES],
                },
            },
            payload_ciphertext,
        }
    }

    /// The 64-entry cap drops everything already cached when a new read lands.
    /// Stale-era entries must not survive that eviction, and the new read must
    /// still be served.
    #[tokio::test]
    async fn history_cache_eviction_clears_stale_entries_without_losing_the_new_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:1", dir.path());
        *engine.encryption_key.write().await = Some(Zeroizing::new(HISTORY_TEST_KEY));
        let epoch = engine.history_epoch.load(Ordering::SeqCst);

        // Fill the cache to its documented bound with entries of this session.
        for i in 0..64u8 {
            let pin = clipper_schedule::ObjectRevisionRef {
                object_id: uuid::Uuid::new_v4().into(),
                revision: 1,
                body_hash: [i; 32],
            };
            engine
                .schedule_history
                .lock()
                .await
                .insert((epoch, pin), source_record("cached"));
        }

        // One real object whose local head matches the pin, so the read is
        // served locally and repopulates the cache (no network on 127.0.0.1:1).
        let record = source_record("current");
        let object_id = uuid::Uuid::new_v4().to_string();
        let encrypted = encrypted_schedule_object(&record, &object_id, 1, None);
        engine.local_store.set_profile("profile-a".into());
        engine
            .local_store
            .persist_local_schedule_present_encrypted(
                StoredObjectIdentity {
                    object_id: &object_id,
                    created_at: "2026-09-12T00:00:00Z",
                    source_device_id: HISTORY_TEST_DEVICE_ID,
                },
                record,
                &encrypted,
                1,
                1,
                10,
            )
            .await
            .expect("persist schedule object");
        let head = engine
            .local_store
            .local_head(&object_id)
            .await
            .expect("local head")
            .expect("a head for the persisted object");
        let pin = clipper_schedule::ObjectRevisionRef {
            object_id: object_id.parse().expect("object id"),
            revision: head.revision,
            body_hash: head.parent_hash,
        };

        let loaded = engine.schedule_revision(pin).await.expect("cached read");
        assert_eq!(loaded.as_source().expect("a source").name, "current");

        let cache = engine.schedule_history.lock().await;
        assert_eq!(
            cache.len(),
            1,
            "eviction at the cap must drop the stale entries, not keep 64"
        );
        assert!(
            cache.contains_key(&(epoch, pin)),
            "the freshly read entry must be the one that survived"
        );
    }

    /// Logout must clear the historical read cache and advance the session
    /// epoch, even when the server cannot be reached.
    #[tokio::test]
    async fn logout_clears_history_cache_and_advances_the_epoch_offline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:1", dir.path());
        *engine.encryption_key.write().await = Some(Zeroizing::new(HISTORY_TEST_KEY));
        let epoch_before = engine.history_epoch.load(Ordering::SeqCst);
        let pin = clipper_schedule::ObjectRevisionRef {
            object_id: uuid::Uuid::new_v4().into(),
            revision: 1,
            body_hash: [2; 32],
        };
        engine
            .schedule_history
            .lock()
            .await
            .insert((epoch_before, pin), source_record("leftover"));

        engine.logout().await.expect("logout clears local state");

        assert!(
            engine.schedule_history.lock().await.is_empty(),
            "logout must empty the historical read cache"
        );
        assert!(
            engine.history_epoch.load(Ordering::SeqCst) > epoch_before,
            "logout must advance the session epoch"
        );
        assert!(
            engine.encryption_key.read().await.is_none(),
            "logout must drop the data key"
        );
    }
}
