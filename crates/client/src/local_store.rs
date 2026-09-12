//! Client-side local cache.
//!
//! Persisted clipboard/file object cache records contain only encrypted object
//! material. Decrypted display state keeps only bounded clipboard previews in
//! memory; full clipboard payload bytes are decrypted from the local ciphertext
//! record only for the operation that needs them.

#[cfg(not(target_family = "wasm"))]
mod sqlite;

#[cfg(not(target_family = "wasm"))]
use std::path::Path;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{RwLock, atomic},
};

use clipper_app_types::{
    ActualView, CalendarSourceView, CollabItem, DecryptedClipboardItem, DecryptedFileItem,
    ScheduleItemView,
};
use clipper_core::{
    crypto,
    models::{ObjectEnvelope, ObjectEnvelopeBody, ObjectKind, ObjectPayloadDescriptor},
};
use serde::{Deserialize, Serialize};
#[cfg(not(target_family = "wasm"))]
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use crate::{
    api_client::{decrypt_clipboard_meta, decrypt_clipboard_payload, decrypt_file_meta_bytes},
    schedule::{ScheduleRecord, actual_view, decrypt_schedule_payload, item_view, source_view},
};

const DEFAULT_PROFILE: &str = "default";
const CLIPBOARD_TEXT_PREVIEW_MAX_CHARS: usize = 512;
#[cfg(not(target_family = "wasm"))]
const DEVICE_IDENTITY_FILE_PREFIX: &str = "device-identity-v1";
const DEVICE_IDENTITY_RECORD_VERSION_V3: u64 = 3;
#[cfg(target_family = "wasm")]
const OBJECT_INDEX_LIMIT: usize = 1_000;

#[derive(Debug, Clone)]
pub struct DeviceSigningIdentity {
    pub device_id: Option<String>,
    pub signing_secret_key: Zeroizing<[u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalObjectRecord {
    pub id: String,
    pub seen_generation: Option<u64>,
    pub event_seq: i64,
    pub created_seq: i64,
    pub created_at: String,
    pub source_device_id: String,
    pub data: LocalObjectData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "record", rename_all = "snake_case")]
pub enum LocalObjectData {
    Clipboard(LocalClipboardRecord),
    File(LocalFileRecord),
    Collab(LocalCollabRecord),
    Schedule(LocalScheduleRecord),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalClipboardRecord {
    pub text: String,
    pub mime_type: String,
    pub payload_size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalFileRecord {
    pub filename: String,
    pub mime_type: String,
    pub blob_size: i64,
}

/// A decrypted schedule object, held in whole.
///
/// Unlike clipboard and file, the record itself is cached rather than a preview
/// of it: schedule records are a few hundred bytes, and every consumer — the
/// list, the grid, the alarm scheduler — needs the structured form, not a
/// summary string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalScheduleRecord {
    pub record: ScheduleRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalCollabRecord {
    pub title: String,
    pub share_token: String,
    pub share_url: Option<String>,
    pub updated_at: String,
}

/// The chain position of a locally-held object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalHead {
    /// Which revision this client holds.
    pub revision: u64,
    /// SHA-256 of that revision's canonical envelope body — what the next
    /// revision must carry as `parent_hash`.
    pub parent_hash: [u8; crypto::SHA256_BYTES],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedObject {
    pub meta_nonce: Vec<u8>,
    pub meta_ciphertext: Vec<u8>,
    pub payloads: Vec<ObjectPayloadDescriptor>,
    pub created_at: String,
    pub source_device_id: String,
    pub envelope: ObjectEnvelope,
}

/// An encrypted object whose single payload is small enough to travel and be
/// cached inline. Clipboard and schedule objects both work this way. A
/// calendar import file is too large, so it gets its own native ciphertext
/// cache that keeps rule resolution working offline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedInlineObject {
    pub object: EncryptedObject,
    pub payload_ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "sync_state", content = "record", rename_all = "snake_case")]
enum StoredObjectRecord {
    Present(Box<StoredPresentObjectRecord>),
    PendingCreate(StoredSyncMarkerRecord),
    Deleted(StoredSyncMarkerRecord),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPresentObjectRecord {
    id: String,
    kind: ObjectKind,
    seen_generation: Option<u64>,
    event_seq: i64,
    created_seq: i64,
    content: StoredPresentContent,
}

/// The present-state payload of a stored object. Clipboard and file objects are
/// end-to-end encrypted, so they persist their encrypted material; collab docs
/// are server-visible (no ciphertext), so they persist plaintext metadata.
/// Modelling this as an enum keeps a collab record from ever carrying ciphertext
/// fields (and vice versa).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "content_kind", content = "data", rename_all = "snake_case")]
enum StoredPresentContent {
    Encrypted(EncryptedObject),
    Collab(StoredCollabRecord),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredCollabRecord {
    #[serde(default)]
    title: String,
    share_token: String,
    /// The server-built public share link, cached so the doc list can offer
    /// "copy link" offline. Refreshed whenever the doc's meta is re-read.
    #[serde(default)]
    share_url: Option<String>,
    created_at: String,
    source_device_id: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSyncMarkerRecord {
    id: String,
    kind: ObjectKind,
    seen_generation: Option<u64>,
    event_seq: i64,
    created_seq: i64,
    /// The newest signed chain position this device accepted before the object
    /// became a marker. Keeping it here is what makes rollback protection
    /// survive deletes, live-event materialization, and reconciliation sweeps.
    #[serde(default)]
    revision_anchor: Option<StoredRevisionAnchor>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct StoredRevisionAnchor {
    head: LocalHead,
    kind: StoredRevisionAnchorKind,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredRevisionAnchorKind {
    /// The server omitted the object from a snapshot or returned 404. The same
    /// accepted head may legitimately reappear; only older/different history
    /// is forbidden.
    #[default]
    Absent,
    /// A delete event arrived without its signed tombstone body. `head` is the
    /// preceding visible revision, so a future live head must be at least two
    /// revisions newer.
    ObservedDelete,
    /// `head` is the locally-created signed tombstone itself.
    Tombstone,
}

impl StoredObjectRecord {
    fn id(&self) -> &str {
        match self {
            Self::Present(record) => &record.id,
            Self::PendingCreate(record) | Self::Deleted(record) => &record.id,
        }
    }

    fn event_seq(&self) -> i64 {
        match self {
            Self::Present(record) => record.event_seq,
            Self::PendingCreate(record) | Self::Deleted(record) => record.event_seq,
        }
    }
}

#[derive(Debug, Default)]
struct MemoryState {
    records: HashMap<String, LocalObjectRecord>,
}

#[derive(Debug, Clone)]
pub struct LocalVisibleState {
    /// When this view was built, counted by the store. A publisher drops a view
    /// stamped lower than one it has already published, so two concurrent
    /// snapshots cannot leave the older one on screen.
    pub stamp: u64,
    pub clipboard_items: Vec<DecryptedClipboardItem>,
    pub files: Vec<DecryptedFileItem>,
    pub collab_docs: Vec<CollabItem>,
    pub schedule_items: Vec<ScheduleItemView>,
    pub calendar_sources: Vec<CalendarSourceView>,
    pub running_actual: Option<ActualView>,
    pub running_plan: Option<clipper_schedule::PlannedRef>,
}

#[derive(Debug, Default)]
struct LocalSyncControl {
    generation: u64,
}

/// The identity fields every stored object carries, independent of its kind.
///
/// Grouped because they always travel together and always come from the same
/// place — the object listing or the init request that created it.
#[derive(Debug, Clone, Copy)]
pub struct StoredObjectIdentity<'a> {
    pub object_id: &'a str,
    pub created_at: &'a str,
    pub source_device_id: &'a str,
}

#[derive(Debug, Clone, Copy)]
struct StoredObjectSyncMeta {
    created_seq: i64,
    event_seq: i64,
    seen_generation: Option<u64>,
}

#[derive(Debug)]
pub struct LocalStore {
    base_dir: PathBuf,
    profile_id: RwLock<Option<String>>,
    sync: Mutex<LocalSyncControl>,
    memory: Mutex<MemoryState>,
    /// Counts the views built, so a publisher can tell which of two views is
    /// the newer one. Incremented while the sync lock is held.
    visible_stamp: atomic::AtomicU64,
    /// Opened on first use, because the database lives under the profile
    /// directory and the profile is not known when the store is constructed.
    #[cfg(not(target_family = "wasm"))]
    database: Mutex<Option<OpenDatabase>>,
}

#[cfg(not(target_family = "wasm"))]
#[derive(Debug)]
struct OpenDatabase {
    profile_id: String,
    connection: rusqlite::Connection,
}

impl LocalStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            profile_id: RwLock::new(None),
            sync: Mutex::new(LocalSyncControl::default()),
            memory: Mutex::new(MemoryState::default()),
            visible_stamp: atomic::AtomicU64::new(0),
            #[cfg(not(target_family = "wasm"))]
            database: Mutex::new(None),
        }
    }

    pub fn set_profile(&self, profile_id: String) {
        let mut current = self
            .profile_id
            .write()
            .expect("local store profile lock poisoned");
        *current = Some(profile_id);
    }

    pub async fn start_generation(&self) -> u64 {
        let mut sync = self.sync.lock().await;
        sync.generation += 1;
        sync.generation
    }

    /// Account for an object a pass listed but did not write.
    ///
    /// A refused revision still proves the object is on the server, so the
    /// sweep at the end of the pass must not treat it as gone.
    pub async fn mark_snapshot_seen(
        &self,
        object_id: &str,
        generation: u64,
    ) -> Result<(), LocalStoreError> {
        let object_id = validate_item_id(object_id)?;
        let sync = self.sync.lock().await;
        if sync.generation != generation {
            return Ok(());
        }
        self.refresh_seen_generation(&object_id, generation).await
    }

    /// Keep the revision this device already accepted.
    ///
    /// A snapshot page can list a revision older than the local head whenever a
    /// live event or this device's own write lands while the page is in flight,
    /// so a refusal is an ordinary interleave and must not abort the pass. Any
    /// other error is the caller's.
    async fn keep_retained_revision(
        &self,
        object_id: &str,
        generation: u64,
        error: LocalStoreError,
    ) -> Result<(), LocalStoreError> {
        let LocalStoreError::RevisionRejected(reason) = error else {
            return Err(error);
        };
        tracing::warn!(object_id = %object_id, "Kept the revision this device holds: {reason}");
        self.refresh_seen_generation(object_id, generation).await
    }

    /// Mark a held record as accounted for by this pass without changing it.
    /// The caller holds the sync lock.
    async fn refresh_seen_generation(
        &self,
        object_id: &str,
        generation: u64,
    ) -> Result<(), LocalStoreError> {
        let Some(StoredObjectRecord::Present(mut record)) =
            self.stored_object_record(object_id).await?
        else {
            return Ok(());
        };
        record.seen_generation = Some(generation);
        self.write_stored_object_record(&StoredObjectRecord::Present(record))
            .await
    }

    pub async fn clear_memory(&self) {
        *self.memory.lock().await = MemoryState::default();
    }

    pub async fn persist_local_clipboard_present_encrypted(
        &self,
        item: &DecryptedClipboardItem,
        payload: &[u8],
        encrypted: &EncryptedInlineObject,
        created_seq: i64,
        event_seq: i64,
        visible_clipboard_limit: usize,
    ) -> Result<LocalVisibleState, LocalStoreError> {
        let item_id = validate_item_id(&item.id)?;
        let sync = self.sync.lock().await;
        self.persist_clipboard_present_encrypted_inner(
            &item_id,
            item,
            payload,
            encrypted,
            StoredObjectSyncMeta {
                created_seq,
                event_seq,
                seen_generation: Some(sync.generation),
            },
        )
        .await?;
        self.visible_state_inner(visible_clipboard_limit).await
    }

    pub async fn persist_snapshot_clipboard_present_encrypted(
        &self,
        item: &DecryptedClipboardItem,
        payload: &[u8],
        encrypted: &EncryptedInlineObject,
        created_seq: i64,
        generation: u64,
        visible_clipboard_limit: usize,
    ) -> Result<Option<LocalVisibleState>, LocalStoreError> {
        let item_id = validate_item_id(&item.id)?;
        let sync = self.sync.lock().await;
        if sync.generation != generation {
            return Ok(None);
        }
        if let Err(error) = self
            .persist_clipboard_present_encrypted_inner(
                &item_id,
                item,
                payload,
                encrypted,
                StoredObjectSyncMeta {
                    created_seq,
                    event_seq: created_seq,
                    seen_generation: Some(generation),
                },
            )
            .await
        {
            self.keep_retained_revision(&item_id, generation, error)
                .await?;
            return Ok(None);
        }
        self.visible_state_inner(visible_clipboard_limit)
            .await
            .map(Some)
    }

    pub async fn persist_local_schedule_present_encrypted(
        &self,
        identity: StoredObjectIdentity<'_>,
        record: ScheduleRecord,
        encrypted: &EncryptedInlineObject,
        created_seq: i64,
        event_seq: i64,
        visible_clipboard_limit: usize,
    ) -> Result<LocalVisibleState, LocalStoreError> {
        validate_item_id(identity.object_id)?;
        let sync = self.sync.lock().await;
        self.persist_schedule_present_encrypted_inner(
            identity,
            record,
            encrypted,
            StoredObjectSyncMeta {
                created_seq,
                event_seq,
                seen_generation: Some(sync.generation),
            },
        )
        .await?;
        self.visible_state_inner(visible_clipboard_limit).await
    }

    pub async fn persist_snapshot_schedule_present_encrypted(
        &self,
        identity: StoredObjectIdentity<'_>,
        record: ScheduleRecord,
        encrypted: &EncryptedInlineObject,
        created_seq: i64,
        generation: u64,
        visible_clipboard_limit: usize,
    ) -> Result<Option<LocalVisibleState>, LocalStoreError> {
        validate_item_id(identity.object_id)?;
        let sync = self.sync.lock().await;
        if sync.generation != generation {
            return Ok(None);
        }
        if let Err(error) = self
            .persist_schedule_present_encrypted_inner(
                identity,
                record,
                encrypted,
                StoredObjectSyncMeta {
                    created_seq,
                    event_seq: created_seq,
                    seen_generation: Some(generation),
                },
            )
            .await
        {
            self.keep_retained_revision(identity.object_id, generation, error)
                .await?;
            return Ok(None);
        }
        self.visible_state_inner(visible_clipboard_limit)
            .await
            .map(Some)
    }

    async fn persist_schedule_present_encrypted_inner(
        &self,
        identity: StoredObjectIdentity<'_>,
        record: ScheduleRecord,
        encrypted: &EncryptedInlineObject,
        sync_meta: StoredObjectSyncMeta,
    ) -> Result<(), LocalStoreError> {
        let object_id = identity.object_id;
        self.validate_encrypted_revision_advance(object_id, &encrypted.object.envelope.body)
            .await?;
        // A delete that landed after this create wins: re-persisting would
        // resurrect a record the user already removed on another device.
        if let Some(StoredObjectRecord::Deleted(deleted)) =
            self.stored_object_record(object_id).await?
            && deleted.event_seq > sync_meta.event_seq
        {
            return Ok(());
        }

        let local_record = LocalObjectRecord {
            id: object_id.to_string(),
            seen_generation: sync_meta.seen_generation,
            event_seq: sync_meta.event_seq,
            created_seq: sync_meta.created_seq,
            created_at: identity.created_at.to_string(),
            source_device_id: identity.source_device_id.to_string(),
            data: LocalObjectData::Schedule(LocalScheduleRecord { record }),
        };
        let stored_record = StoredObjectRecord::Present(Box::new(StoredPresentObjectRecord {
            id: object_id.to_string(),
            kind: ObjectKind::Schedule,
            seen_generation: sync_meta.seen_generation,
            event_seq: sync_meta.event_seq,
            created_seq: sync_meta.created_seq,
            content: StoredPresentContent::Encrypted(encrypted.object.clone()),
        }));
        self.write_stored_object_record_with_payload(&stored_record, &encrypted.payload_ciphertext)
            .await?;
        self.write_memory_record(local_record).await
    }

    /// The accepted metadata of a live file. A deleted object's anchor never
    /// resolves an import.
    pub(crate) async fn import_file_object(
        &self,
        object_id: &str,
    ) -> Result<Option<EncryptedObject>, LocalStoreError> {
        let _sync = self.sync.lock().await;
        let Some(StoredObjectRecord::Present(record)) =
            self.stored_object_record(object_id).await?
        else {
            return Ok(None);
        };
        if record.kind != ObjectKind::File {
            return Ok(None);
        }
        match &record.content {
            StoredPresentContent::Encrypted(object) => Ok(Some(object.clone())),
            _ => Ok(None),
        }
    }

    /// The cached ciphertext of a whole import, so a native client can
    /// resolve rules offline. Matching the expected head stops a concurrent
    /// replacement or deletion from handing back the wrong bytes.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) async fn import_file_ciphertext(
        &self,
        object_id: &str,
        expected: LocalHead,
    ) -> Result<Option<Vec<u8>>, LocalStoreError> {
        let _sync = self.sync.lock().await;
        let Some(StoredObjectRecord::Present(record)) =
            self.stored_object_record(object_id).await?
        else {
            return Ok(None);
        };
        if record.kind != ObjectKind::File || local_head_from_present(&record)? != expected {
            return Ok(None);
        }
        self.stored_object_payload_ciphertext(object_id).await
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) async fn cache_import_file_ciphertext(
        &self,
        object_id: &str,
        expected: LocalHead,
        ciphertext: &[u8],
    ) -> Result<(), LocalStoreError> {
        let _sync = self.sync.lock().await;
        let Some(StoredObjectRecord::Present(record)) =
            self.stored_object_record(object_id).await?
        else {
            return Ok(());
        };
        if record.kind != ObjectKind::File || local_head_from_present(&record)? != expected {
            return Ok(());
        }
        let StoredPresentContent::Encrypted(object) = &record.content else {
            return Ok(());
        };
        verify_payload_ciphertext(single_payload(object)?, ciphertext)?;
        self.write_stored_object_record_with_payload(
            &StoredObjectRecord::Present(record),
            ciphertext,
        )
        .await
    }

    pub async fn persist_local_file_present_encrypted(
        &self,
        item: &DecryptedFileItem,
        encrypted: &EncryptedObject,
        created_seq: i64,
        event_seq: i64,
        visible_clipboard_limit: usize,
    ) -> Result<LocalVisibleState, LocalStoreError> {
        let item_id = validate_item_id(&item.id)?;
        let sync = self.sync.lock().await;
        self.persist_file_present_encrypted_inner(
            &item_id,
            item,
            encrypted,
            created_seq,
            event_seq,
            Some(sync.generation),
        )
        .await?;
        self.visible_state_inner(visible_clipboard_limit).await
    }

    pub async fn persist_snapshot_file_present_encrypted(
        &self,
        item: &DecryptedFileItem,
        encrypted: &EncryptedObject,
        created_seq: i64,
        generation: u64,
        visible_clipboard_limit: usize,
    ) -> Result<Option<LocalVisibleState>, LocalStoreError> {
        let item_id = validate_item_id(&item.id)?;
        let sync = self.sync.lock().await;
        if sync.generation != generation {
            return Ok(None);
        }
        if let Err(error) = self
            .persist_file_present_encrypted_inner(
                &item_id,
                item,
                encrypted,
                created_seq,
                created_seq,
                Some(generation),
            )
            .await
        {
            self.keep_retained_revision(&item_id, generation, error)
                .await?;
            return Ok(None);
        }
        self.visible_state_inner(visible_clipboard_limit)
            .await
            .map(Some)
    }

    /// Persist a collab doc materialized from a live `created` event. Mirrors
    /// `persist_local_file_present_encrypted`; collab docs carry plaintext
    /// metadata (server-visible) rather than encrypted material.
    pub async fn persist_local_collab_present(
        &self,
        item: &CollabItem,
        source_device_id: &str,
        created_seq: i64,
        event_seq: i64,
        visible_clipboard_limit: usize,
    ) -> Result<LocalVisibleState, LocalStoreError> {
        let item_id = validate_item_id(&item.id)?;
        let sync = self.sync.lock().await;
        self.persist_collab_present_inner(
            &item_id,
            item,
            source_device_id,
            created_seq,
            event_seq,
            Some(sync.generation),
        )
        .await?;
        self.visible_state_inner(visible_clipboard_limit).await
    }

    /// Persist a collab doc seen during a reconciliation snapshot. Mirrors
    /// `persist_snapshot_file_present_encrypted`: a stale-generation write is
    /// dropped so a superseded snapshot cannot resurrect a since-changed object.
    pub async fn persist_snapshot_collab_present(
        &self,
        item: &CollabItem,
        source_device_id: &str,
        created_seq: i64,
        generation: u64,
        visible_clipboard_limit: usize,
    ) -> Result<Option<LocalVisibleState>, LocalStoreError> {
        let item_id = validate_item_id(&item.id)?;
        let sync = self.sync.lock().await;
        if sync.generation != generation {
            return Ok(None);
        }
        self.persist_collab_present_inner(
            &item_id,
            item,
            source_device_id,
            created_seq,
            created_seq,
            Some(generation),
        )
        .await?;
        self.visible_state_inner(visible_clipboard_limit)
            .await
            .map(Some)
    }

    pub async fn hydrate_ciphertext_cache(
        &self,
        encryption_key: &[u8; 32],
        visible_clipboard_limit: usize,
    ) -> Result<LocalVisibleState, LocalStoreError> {
        #[cfg(not(target_family = "wasm"))]
        self.sweep_orphaned_temp_files().await;
        let mut memory = MemoryState::default();
        for record in self.live_stored_object_records().await? {
            match self
                .decrypt_stored_object_record_preview(&record, encryption_key)
                .await
            {
                Ok(Some(local_record)) => {
                    memory.records.insert(local_record.id.clone(), local_record);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        object_id = %record.id(),
                        "Failed to decrypt local cache record: {}",
                        error
                    );
                    self.discard_unreadable_cache_entry(&record).await?;
                }
            }
        }

        *self.memory.lock().await = memory;
        self.visible_state_inner(visible_clipboard_limit).await
    }

    pub async fn mark_pending_create(
        &self,
        kind: ObjectKind,
        object_id: &str,
        created_seq: i64,
        generation: u64,
    ) -> Result<bool, LocalStoreError> {
        let object_id = validate_item_id(object_id)?;
        let sync = self.sync.lock().await;
        if sync.generation != generation {
            return Ok(false);
        }
        self.mark_pending_create_inner(kind, &object_id, created_seq, generation)
            .await
    }

    /// Mark an object as needing a refetch because a new revision is its head.
    ///
    /// The difference from `mark_pending_create` is the one case that matters:
    /// an object already held locally. A create event for one of those is a
    /// duplicate and is ignored, which was right while objects were immutable.
    /// An update event for one means the content changed underneath the same
    /// id, so it has to be fetched again.
    pub async fn mark_pending_update(
        &self,
        kind: ObjectKind,
        object_id: &str,
        event_seq: i64,
        generation: u64,
    ) -> Result<bool, LocalStoreError> {
        let object_id = validate_item_id(object_id)?;
        let sync = self.sync.lock().await;
        if sync.generation != generation {
            return Ok(false);
        }
        match self.stored_object_record(&object_id).await? {
            // A delete that landed after this update wins, exactly as for a
            // create: re-fetching would resurrect a removed object.
            Some(StoredObjectRecord::Deleted(record)) if record.event_seq > event_seq => Ok(false),
            // Already at or past this revision; nothing to do.
            Some(StoredObjectRecord::Present(record)) if record.event_seq >= event_seq => Ok(false),
            Some(StoredObjectRecord::Present(mut record)) => {
                record.event_seq = event_seq;
                record.created_seq = event_seq;
                record.seen_generation = Some(generation);
                self.write_stored_object_record(&StoredObjectRecord::Present(record))
                    .await?;
                Ok(true)
            }
            // Never seen, or seen only as a marker: the create path already
            // does the right thing for both.
            _ => {
                self.mark_pending_create_inner(kind, &object_id, event_seq, generation)
                    .await
            }
        }
    }

    pub async fn apply_local_delete(
        &self,
        kind: ObjectKind,
        object_id: &str,
        event_seq: i64,
        visible_clipboard_limit: usize,
    ) -> Result<LocalVisibleState, LocalStoreError> {
        let object_id = validate_item_id(object_id)?;
        let sync = self.sync.lock().await;
        self.apply_delete_inner(kind, &object_id, event_seq, sync.generation, None)
            .await?;
        self.visible_state_inner(visible_clipboard_limit).await
    }

    /// Apply a deletion initiated on this device while retaining the signed
    /// tombstone as the newest durable chain anchor.
    pub async fn apply_local_tombstone(
        &self,
        kind: ObjectKind,
        object_id: &str,
        event_seq: i64,
        tombstone_head: LocalHead,
        visible_clipboard_limit: usize,
    ) -> Result<LocalVisibleState, LocalStoreError> {
        let object_id = validate_item_id(object_id)?;
        let sync = self.sync.lock().await;
        self.apply_delete_inner(
            kind,
            &object_id,
            event_seq,
            sync.generation,
            Some(tombstone_head),
        )
        .await?;
        self.visible_state_inner(visible_clipboard_limit).await
    }

    pub async fn apply_live_delete(
        &self,
        kind: ObjectKind,
        object_id: &str,
        event_seq: i64,
        generation: u64,
        visible_clipboard_limit: usize,
    ) -> Result<Option<LocalVisibleState>, LocalStoreError> {
        let object_id = validate_item_id(object_id)?;
        let sync = self.sync.lock().await;
        if sync.generation != generation {
            return Ok(None);
        }
        self.apply_delete_inner(kind, &object_id, event_seq, generation, None)
            .await?;
        self.visible_state_inner(visible_clipboard_limit)
            .await
            .map(Some)
    }

    pub async fn sweep_kind(
        &self,
        kind: ObjectKind,
        generation: u64,
        stream_start_seq: i64,
        visible_clipboard_limit: usize,
    ) -> Result<Option<LocalVisibleState>, LocalStoreError> {
        let sync = self.sync.lock().await;
        if sync.generation != generation {
            return Ok(None);
        }
        self.sweep_kind_inner(kind, generation, stream_start_seq)
            .await?;
        self.visible_state_inner(visible_clipboard_limit)
            .await
            .map(Some)
    }

    pub async fn remove_absent_object(
        &self,
        object_id: &str,
        generation: u64,
        visible_clipboard_limit: usize,
    ) -> Result<Option<LocalVisibleState>, LocalStoreError> {
        let object_id = validate_item_id(object_id)?;
        let sync = self.sync.lock().await;
        if sync.generation != generation {
            return Ok(None);
        }
        self.mark_object_absent_inner(&object_id).await?;
        self.visible_state_inner(visible_clipboard_limit)
            .await
            .map(Some)
    }

    pub async fn clipboard_payload(
        &self,
        id: &str,
        encryption_key: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, LocalStoreError> {
        let item_id = validate_item_id(id)?;
        let _sync = self.sync.lock().await;
        self.clipboard_payload_inner(&item_id, encryption_key).await
    }

    pub async fn load_or_create_device_signing_identity(
        &self,
        profile_id: &str,
        wrapping_key: &[u8; 32],
    ) -> Result<DeviceSigningIdentity, LocalStoreError> {
        self.load_or_create_device_signing_identity_inner(profile_id, wrapping_key)
            .await
    }

    /// Load an existing device signing identity for `profile_id`, or `None` if
    /// none is stored. Unlike
    /// [`LocalStore::load_or_create_device_signing_identity`] this never mints a
    /// new identity — session resume must re-mount the device the persisted keys
    /// already name rather than enroll a fresh one.
    pub async fn load_device_signing_identity(
        &self,
        profile_id: &str,
        wrapping_key: &[u8; 32],
    ) -> Result<Option<DeviceSigningIdentity>, LocalStoreError> {
        self.load_device_signing_identity_inner(profile_id, wrapping_key)
            .await
    }

    pub async fn persist_device_signing_identity(
        &self,
        profile_id: &str,
        identity: &DeviceSigningIdentity,
        wrapping_key: &[u8; 32],
    ) -> Result<(), LocalStoreError> {
        if let Some(device_id) = identity.device_id.as_deref() {
            validate_device_id(device_id)?;
        }
        self.persist_device_signing_identity_inner(profile_id, identity, wrapping_key)
            .await
    }

    async fn persist_clipboard_present_encrypted_inner(
        &self,
        item_id: &str,
        item: &DecryptedClipboardItem,
        payload: &[u8],
        encrypted: &EncryptedInlineObject,
        sync_meta: StoredObjectSyncMeta,
    ) -> Result<(), LocalStoreError> {
        self.validate_encrypted_revision_advance(item_id, &encrypted.object.envelope.body)
            .await?;
        if let Some(StoredObjectRecord::Deleted(record)) =
            self.stored_object_record(item_id).await?
            && record.event_seq > sync_meta.event_seq
        {
            return Ok(());
        }

        let local_record = LocalObjectRecord {
            id: item.id.clone(),
            seen_generation: sync_meta.seen_generation,
            event_seq: sync_meta.event_seq,
            created_seq: sync_meta.created_seq,
            created_at: item.created_at.clone(),
            source_device_id: item.source_device_id.clone(),
            data: LocalObjectData::Clipboard(local_clipboard_record_from_payload(
                &item.mime_type,
                payload,
                item.payload_size,
            )),
        };
        debug_assert_eq!(item_id, item.id);
        let stored_record = StoredObjectRecord::Present(Box::new(StoredPresentObjectRecord {
            id: item.id.clone(),
            kind: ObjectKind::Clipboard,
            seen_generation: sync_meta.seen_generation,
            event_seq: sync_meta.event_seq,
            created_seq: sync_meta.created_seq,
            content: StoredPresentContent::Encrypted(encrypted.object.clone()),
        }));
        self.write_stored_object_record_with_payload(&stored_record, &encrypted.payload_ciphertext)
            .await?;
        self.write_memory_record(local_record).await
    }

    async fn persist_file_present_encrypted_inner(
        &self,
        item_id: &str,
        item: &DecryptedFileItem,
        encrypted: &EncryptedObject,
        created_seq: i64,
        event_seq: i64,
        seen_generation: Option<u64>,
    ) -> Result<(), LocalStoreError> {
        self.validate_encrypted_revision_advance(item_id, &encrypted.envelope.body)
            .await?;
        if let Some(StoredObjectRecord::Deleted(record)) =
            self.stored_object_record(item_id).await?
            && record.event_seq > event_seq
        {
            return Ok(());
        }

        let local_record = LocalObjectRecord {
            id: item.id.clone(),
            seen_generation,
            event_seq,
            created_seq,
            created_at: item.created_at.clone(),
            source_device_id: item.source_device_id.clone(),
            data: LocalObjectData::File(LocalFileRecord {
                filename: item.filename.clone(),
                mime_type: item.mime_type.clone(),
                blob_size: item.blob_size,
            }),
        };
        debug_assert_eq!(item_id, item.id);
        let stored_record = StoredObjectRecord::Present(Box::new(StoredPresentObjectRecord {
            id: item.id.clone(),
            kind: ObjectKind::File,
            seen_generation,
            event_seq,
            created_seq,
            content: StoredPresentContent::Encrypted(encrypted.clone()),
        }));
        self.write_stored_object_record(&stored_record).await?;
        self.write_memory_record(local_record).await
    }

    async fn persist_collab_present_inner(
        &self,
        item_id: &str,
        item: &CollabItem,
        source_device_id: &str,
        created_seq: i64,
        event_seq: i64,
        seen_generation: Option<u64>,
    ) -> Result<(), LocalStoreError> {
        let existing = self.stored_object_record(item_id).await?;
        // A collab listing is server-visible metadata with no signed chain, so
        // it proves nothing about an encrypted object held under the same id.
        // Writing it would replace that object's record and drop the anchor
        // with it, after which the server could replay an older revision.
        if let Some(record) = existing.as_ref()
            && revision_anchor_for_record(record)?.is_some()
        {
            tracing::warn!(
                object_id = %item_id,
                "Ignoring a collab listing for an object that holds a revision anchor",
            );
            return Ok(());
        }
        if let Some(StoredObjectRecord::Deleted(record)) = existing.as_ref()
            && record.event_seq > event_seq
        {
            return Ok(());
        }

        let local_record = LocalObjectRecord {
            id: item.id.clone(),
            seen_generation,
            event_seq,
            created_seq,
            created_at: item.created_at.clone(),
            source_device_id: source_device_id.to_string(),
            data: LocalObjectData::Collab(LocalCollabRecord {
                title: item.title.clone(),
                share_token: item.share_token.clone(),
                share_url: item.share_url.clone(),
                updated_at: item.updated_at.clone(),
            }),
        };
        debug_assert_eq!(item_id, item.id);
        let stored_record = StoredObjectRecord::Present(Box::new(StoredPresentObjectRecord {
            id: item.id.clone(),
            kind: ObjectKind::Collab,
            seen_generation,
            event_seq,
            created_seq,
            content: StoredPresentContent::Collab(StoredCollabRecord {
                title: item.title.clone(),
                share_token: item.share_token.clone(),
                share_url: item.share_url.clone(),
                created_at: item.created_at.clone(),
                source_device_id: source_device_id.to_string(),
                updated_at: item.updated_at.clone(),
            }),
        }));
        self.write_stored_object_record(&stored_record).await?;
        self.write_memory_record(local_record).await
    }

    async fn mark_pending_create_inner(
        &self,
        kind: ObjectKind,
        object_id: &str,
        created_seq: i64,
        generation: u64,
    ) -> Result<bool, LocalStoreError> {
        match self.stored_object_record(object_id).await? {
            Some(StoredObjectRecord::Deleted(record)) if record.event_seq > created_seq => {
                Ok(false)
            }
            Some(StoredObjectRecord::Present(record)) if record.event_seq >= created_seq => {
                Ok(false)
            }
            Some(StoredObjectRecord::Present(mut record)) => {
                record.event_seq = created_seq;
                record.created_seq = created_seq;
                record.seen_generation = Some(generation);
                self.write_stored_object_record(&StoredObjectRecord::Present(record))
                    .await?;
                Ok(false)
            }
            // A fetch is already outstanding for this object. Starting a second
            // one for a duplicate event just races the first.
            Some(StoredObjectRecord::PendingCreate(mut record)) => {
                if created_seq >= record.event_seq {
                    record.event_seq = created_seq;
                    record.created_seq = created_seq;
                    record.seen_generation = Some(generation);
                    self.write_stored_object_record(&StoredObjectRecord::PendingCreate(record))
                        .await?;
                }
                Ok(false)
            }
            Some(StoredObjectRecord::Deleted(record)) => {
                let pending = StoredObjectRecord::PendingCreate(StoredSyncMarkerRecord {
                    id: object_id.to_string(),
                    kind,
                    seen_generation: Some(generation),
                    event_seq: created_seq,
                    created_seq,
                    revision_anchor: record.revision_anchor,
                });
                self.write_stored_object_record(&pending).await?;
                Ok(true)
            }
            _ => {
                let record = StoredObjectRecord::PendingCreate(StoredSyncMarkerRecord {
                    id: object_id.to_string(),
                    kind,
                    seen_generation: Some(generation),
                    event_seq: created_seq,
                    created_seq,
                    revision_anchor: None,
                });
                self.write_stored_object_record(&record).await?;
                Ok(true)
            }
        }
    }

    async fn apply_delete_inner(
        &self,
        kind: ObjectKind,
        object_id: &str,
        event_seq: i64,
        generation: u64,
        tombstone_head: Option<LocalHead>,
    ) -> Result<(), LocalStoreError> {
        let existing = self.stored_object_record(object_id).await?;
        if let Some(record) = existing.as_ref()
            && record.event_seq() >= event_seq
        {
            if record.event_seq() == event_seq
                && let Some(head) = tombstone_head
                && matches!(record, StoredObjectRecord::Deleted(_))
            {
                let StoredObjectRecord::Deleted(mut marker) = record.clone() else {
                    unreachable!();
                };
                let may_upgrade = marker
                    .revision_anchor
                    .is_none_or(|anchor| head.revision >= anchor.head.revision);
                if may_upgrade {
                    marker.revision_anchor = Some(StoredRevisionAnchor {
                        head,
                        kind: StoredRevisionAnchorKind::Tombstone,
                    });
                    self.write_stored_object_record(&StoredObjectRecord::Deleted(marker))
                        .await?;
                }
            }
            return Ok(());
        }
        let retained_anchor = match existing.as_ref() {
            Some(record) => revision_anchor_for_record(record)?,
            None => None,
        };
        let revision_anchor = match tombstone_head {
            // A tombstone this device signed still has to follow the anchor it
            // already holds. A delete response that arrives after another
            // device's later revision was accepted carries an older head, and
            // taking it would lower the anchor and let the revisions in
            // between be replayed.
            Some(head) => {
                if let Some(anchor) = retained_anchor {
                    if head.revision < anchor.head.revision {
                        return Err(revision_anchor_error(
                            object_id,
                            head.revision,
                            "rolls back the retained revision anchor",
                        ));
                    }
                    if head.revision == anchor.head.revision
                        && head.parent_hash != anchor.head.parent_hash
                    {
                        return Err(revision_anchor_error(
                            object_id,
                            head.revision,
                            "changes the already accepted revision body",
                        ));
                    }
                }
                Some(StoredRevisionAnchor {
                    head,
                    kind: StoredRevisionAnchorKind::Tombstone,
                })
            }
            None => retained_anchor.map(|mut anchor| {
                anchor.kind = StoredRevisionAnchorKind::ObservedDelete;
                anchor
            }),
        };
        self.discard_cached_payload(object_id).await?;
        self.remove_memory_record(object_id).await;
        let record = StoredObjectRecord::Deleted(StoredSyncMarkerRecord {
            id: object_id.to_string(),
            kind,
            seen_generation: Some(generation),
            event_seq,
            created_seq: event_seq,
            revision_anchor,
        });
        self.write_stored_object_record(&record).await
    }

    async fn sweep_kind_inner(
        &self,
        kind: ObjectKind,
        generation: u64,
        stream_start_seq: i64,
    ) -> Result<(), LocalStoreError> {
        for object_id in self
            .stale_stored_object_ids(kind, generation, stream_start_seq)
            .await?
        {
            let Some(record) = self.stored_object_record(&object_id).await? else {
                continue;
            };
            match &record {
                StoredObjectRecord::Present(_) => self.mark_record_absent(&record).await?,
                StoredObjectRecord::PendingCreate(stored) => {
                    if stored.revision_anchor.is_some() {
                        self.write_stored_object_record(&StoredObjectRecord::Deleted(
                            stored.clone(),
                        ))
                        .await?;
                    } else {
                        self.remove_stored_object_record_and_payloads(&record)
                            .await?;
                    }
                }
                // Nothing that is already gone can be swept.
                StoredObjectRecord::Deleted(_) => {}
            }
        }
        Ok(())
    }

    async fn mark_record_absent(&self, record: &StoredObjectRecord) -> Result<(), LocalStoreError> {
        let StoredObjectRecord::Present(present) = record else {
            return Ok(());
        };
        if present.kind == ObjectKind::Collab {
            return self.remove_stored_object_record_and_payloads(record).await;
        }
        let anchor = StoredRevisionAnchor {
            head: local_head_from_present(present)?,
            kind: StoredRevisionAnchorKind::Absent,
        };
        self.discard_cached_payload(&present.id).await?;
        self.remove_memory_record(&present.id).await;
        self.write_stored_object_record(&StoredObjectRecord::Deleted(StoredSyncMarkerRecord {
            id: present.id.clone(),
            kind: present.kind,
            seen_generation: present.seen_generation,
            event_seq: present.event_seq,
            created_seq: present.created_seq,
            revision_anchor: Some(anchor),
        }))
        .await
    }

    /// Throw away content this device can no longer read, without throwing
    /// away what it already accepted.
    ///
    /// A record that fails to decrypt is a broken cache entry, and the answer
    /// to a broken cache entry is to fetch it again. It says nothing about the
    /// chain: the envelope sitting beside the unreadable content is still
    /// signed and still names the revision this device accepted. Deleting the
    /// record would take that anchor with it. A payload file can go missing
    /// for reasons that have nothing to do with the chain, such as a partially
    /// restored backup or a stray cleaner, and losing the anchor there would
    /// let a server replay an older revision unchallenged.
    ///
    /// Downgrading to an `Absent` marker keeps the anchor and still allows the
    /// same head back, which is what the refetch will bring.
    async fn discard_unreadable_cache_entry(
        &self,
        record: &StoredObjectRecord,
    ) -> Result<(), LocalStoreError> {
        if revision_anchor_for_record(record).is_ok_and(|anchor| anchor.is_some()) {
            return self.mark_record_absent(record).await;
        }
        // Nothing to protect: a collab doc has no revision chain, and a record
        // too damaged to yield its envelope has no anchor left to salvage.
        self.remove_stored_object_record_and_payloads(record).await
    }

    async fn mark_object_absent_inner(&self, object_id: &str) -> Result<(), LocalStoreError> {
        let Some(record) = self.stored_object_record(object_id).await? else {
            self.remove_memory_record(object_id).await;
            return Ok(());
        };
        match &record {
            StoredObjectRecord::Present(_) => self.mark_record_absent(&record).await,
            StoredObjectRecord::PendingCreate(marker) if marker.revision_anchor.is_some() => {
                self.discard_cached_payload(object_id).await?;
                self.remove_memory_record(object_id).await;
                self.write_stored_object_record(&StoredObjectRecord::Deleted(marker.clone()))
                    .await
            }
            StoredObjectRecord::Deleted(_) => Ok(()),
            StoredObjectRecord::PendingCreate(_) => {
                self.remove_stored_object_record_and_payloads(&record).await
            }
        }
    }

    fn recent_clipboard_items_inner(
        records: &[LocalObjectRecord],
        limit: usize,
    ) -> Vec<DecryptedClipboardItem> {
        let mut items = records
            .iter()
            .filter_map(clipboard_item_from_record)
            .collect::<Vec<_>>();
        items.truncate(limit);
        items
    }

    fn file_items_inner(records: &[LocalObjectRecord]) -> Vec<DecryptedFileItem> {
        records.iter().filter_map(file_item_from_record).collect()
    }

    fn collab_items_inner(records: &[LocalObjectRecord]) -> Vec<CollabItem> {
        records.iter().filter_map(collab_item_from_record).collect()
    }

    async fn clipboard_payload_inner(
        &self,
        item_id: &str,
        encryption_key: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, LocalStoreError> {
        let Some(record) = self.stored_object_record(item_id).await? else {
            return Ok(None);
        };
        self.decrypt_stored_clipboard_payload(&record, encryption_key)
            .await
    }

    async fn decrypt_stored_object_record_preview(
        &self,
        record: &StoredObjectRecord,
        encryption_key: &[u8; 32],
    ) -> Result<Option<LocalObjectRecord>, LocalStoreError> {
        let StoredObjectRecord::Present(record) = record else {
            return Ok(None);
        };

        match record.kind {
            ObjectKind::Clipboard => self
                .decrypt_clipboard_record_preview(record, encryption_key)
                .await
                .map(Some),
            ObjectKind::File => decrypt_file_record(record, encryption_key).map(Some),
            // Collab docs are server-visible: there is nothing to decrypt, so
            // rebuild the display record straight from the stored plaintext
            // metadata. (Mismatched content is a corrupt record and is dropped.)
            ObjectKind::Collab => Ok(collab_record_from_present(record)),
            ObjectKind::Schedule => self
                .decrypt_schedule_record(record, encryption_key)
                .await
                .map(Some),
        }
    }

    /// Rebuild a schedule record from its stored ciphertext.
    ///
    /// The record lives in the payload rather than the meta, so this needs the
    /// cached payload ciphertext — the same path clipboard uses.
    async fn decrypt_schedule_record(
        &self,
        record: &StoredPresentObjectRecord,
        encryption_key: &[u8; 32],
    ) -> Result<LocalObjectRecord, LocalStoreError> {
        let encrypted = present_encrypted_object(record)?;
        let payload = single_payload(encrypted)?;
        let Some(ciphertext) = self.stored_object_payload_ciphertext(&record.id).await? else {
            return Err(LocalStoreError::EncryptedCache(
                "missing schedule payload".into(),
            ));
        };
        verify_payload_ciphertext(payload, &ciphertext)?;
        let decoded = decrypt_schedule_payload(
            &payload.nonce,
            &ciphertext,
            encryption_key,
            &encrypted.envelope.body,
            payload.id,
        )
        .map_err(|error| LocalStoreError::EncryptedCache(error.to_string()))?;
        Ok(LocalObjectRecord {
            id: record.id.clone(),
            seen_generation: record.seen_generation,
            event_seq: record.event_seq,
            created_seq: record.created_seq,
            created_at: encrypted.created_at.clone(),
            source_device_id: encrypted.source_device_id.clone(),
            data: LocalObjectData::Schedule(LocalScheduleRecord { record: decoded }),
        })
    }

    /// The schedule rows, each stamped with the revision it was read at.
    ///
    /// The kind is matched first because reading a head is a database read and
    /// a JSON parse. Asking for one per held object of any kind made every
    /// publish cost a read per object, and `local_head` fails outright on a
    /// collab record, which has no chain.
    async fn schedule_items_inner(
        &self,
        records: &[LocalObjectRecord],
    ) -> Result<Vec<ScheduleItemView>, LocalStoreError> {
        let mut views = Vec::new();
        for record in records {
            if !matches!(record.data, LocalObjectData::Schedule(_)) {
                continue;
            }
            if let Some(head) = self.local_head(&record.id).await?
                && let Some(view) = schedule_item_view_from_record(record, head.revision)
            {
                views.push(view);
            }
        }
        Ok(views)
    }

    fn calendar_sources_inner(records: &[LocalObjectRecord]) -> Vec<CalendarSourceView> {
        records
            .iter()
            .filter_map(|record| {
                let LocalObjectData::Schedule(schedule) = &record.data else {
                    return None;
                };
                let source = schedule.record.as_source()?;
                Some(source_view(
                    &record.id,
                    source,
                    records.iter().filter(|record| {
                        matches!(&record.data, LocalObjectData::Schedule(schedule)
                            if schedule.record.as_ingested().is_some_and(|event| source.contains_event(&record.id, event)))
                    }).count() as u32,
                    source.active_import.as_ref().is_some_and(|batch| records.iter().any(|record|
                        record.id == batch.object_id.to_string() && matches!(&record.data, LocalObjectData::File(_)))),
                ))
            })
            .collect()
    }

    /// The timer currently running, if any.
    ///
    /// Lives in visible state rather than behind a windowed call: a running
    /// timer is relevant on every screen, and there is at most one.
    async fn running_actual_inner(
        &self,
        records: &[LocalObjectRecord],
    ) -> Option<(ActualView, Option<clipper_schedule::PlannedRef>)> {
        for record in records {
            let LocalObjectData::Schedule(schedule) = &record.data else {
                continue;
            };
            let ScheduleRecord::Actual(actual) = &schedule.record else {
                continue;
            };
            if !matches!(actual.span, clipper_schedule::ActualSpan::Running { .. }) {
                continue;
            }
            let mut title = if actual.planned.is_some() {
                "Historical plan unavailable".to_string()
            } else {
                "Unplanned".to_string()
            };
            // Current content is also historical content when the complete pin
            // matches. This avoids a placeholder on ordinary offline restarts.
            if let Some(planned) = actual.planned
                && let Some(target) = records
                    .iter()
                    .find(|entry| entry.id == planned.schedule.object_id.to_string())
                && let LocalObjectData::Schedule(schedule) = &target.data
                && let Ok(Some(head)) = self.local_head(&target.id).await
                && head.revision == planned.schedule.revision
                && head.parent_hash == planned.schedule.body_hash
                && let Some((id, name)) = schedule.record.planned_title()
                && id == planned.item
            {
                title = name.to_string();
            }
            return Some((actual_view(&record.id, actual, &title), actual.planned));
        }
        None
    }

    /// Where the local copy of an object sits in its chain.
    ///
    /// A new revision has to name the head it follows, and that is the head
    /// this client saw, not whatever the server currently holds. Rebasing onto
    /// the server's head would silently absorb another device's edit instead
    /// of colliding with it, which is what the parent hash exists to prevent.
    ///
    /// `None` means there is no local copy to follow, which is a caller error
    /// rather than a reason to fall back to asking the server.
    pub async fn local_head(&self, object_id: &str) -> Result<Option<LocalHead>, LocalStoreError> {
        match self.stored_object_record(object_id).await? {
            Some(StoredObjectRecord::Present(record)) => local_head_from_present(&record).map(Some),
            Some(StoredObjectRecord::Deleted(record)) => Ok(record
                .revision_anchor
                .filter(|anchor| matches!(anchor.kind, StoredRevisionAnchorKind::Tombstone))
                .map(|anchor| anchor.head)),
            Some(StoredObjectRecord::PendingCreate(_)) | None => Ok(None),
        }
    }

    /// Check a served revision against the durable anchor before the caller
    /// commits to it.
    ///
    /// Takes the sync lock, so callers that are not already inside a store
    /// write use this. The rules are the ones every encrypted write obeys, so
    /// a revision that passes here is one persistence will also accept.
    pub async fn validate_incoming_revision(
        &self,
        object_id: &str,
        body: &ObjectEnvelopeBody,
    ) -> Result<(), LocalStoreError> {
        let _sync = self.sync.lock().await;
        self.validate_encrypted_revision_advance(object_id, body)
            .await
    }

    /// Re-check chain monotonicity while the caller holds the sync lock.
    ///
    /// Network materialization performs the same check before decrypting, but
    /// another live event can land between that check and persistence. This
    /// storage-boundary check closes that race and makes every encrypted write
    /// obey the durable anchor, regardless of which engine path called it.
    async fn validate_encrypted_revision_advance(
        &self,
        object_id: &str,
        incoming: &ObjectEnvelopeBody,
    ) -> Result<(), LocalStoreError> {
        let Some(record) = self.stored_object_record(object_id).await? else {
            return Ok(());
        };
        let Some(anchor) = revision_anchor_for_record(&record)? else {
            return Ok(());
        };

        if let StoredObjectRecord::Deleted(marker) | StoredObjectRecord::PendingCreate(marker) =
            &record
            && marker.revision_anchor.is_some_and(|stored| {
                matches!(stored.kind, StoredRevisionAnchorKind::ObservedDelete)
            })
        {
            // A remote delete event proves that at least one tombstone
            // followed the last visible head, although the event does not
            // carry that tombstone's signed body. A restored live object
            // therefore has to be two or more revisions beyond that head.
            let minimum = anchor.head.revision.checked_add(2).ok_or_else(|| {
                LocalStoreError::EncryptedCache("object revision counter overflowed".into())
            })?;
            if incoming.revision < minimum {
                return Err(revision_anchor_error(
                    object_id,
                    incoming.revision,
                    "does not follow the retained delete marker",
                ));
            }
            return Ok(());
        }

        let require_newer = matches!(
            &record,
            StoredObjectRecord::Deleted(marker) | StoredObjectRecord::PendingCreate(marker)
                if marker.revision_anchor.is_some_and(|anchor| {
                    matches!(anchor.kind, StoredRevisionAnchorKind::Tombstone)
                })
        );
        validate_revision_against_head(object_id, incoming, anchor.head, require_newer)
    }

    pub async fn schedule_records_with_ids(&self) -> Vec<(String, ScheduleRecord)> {
        self.all_memory_records()
            .await
            .iter()
            .filter_map(|record| match &record.data {
                LocalObjectData::Schedule(schedule) => {
                    Some((record.id.clone(), schedule.record.clone()))
                }
                LocalObjectData::Clipboard(_)
                | LocalObjectData::File(_)
                | LocalObjectData::Collab(_) => None,
            })
            .collect()
    }

    /// Read records and their revision heads under the same sync lock. A write
    /// derived from a record must follow that record's head, even if live sync
    /// receives another device's edit while the caller is working.
    pub async fn schedule_records_with_heads(
        &self,
    ) -> Result<Vec<(String, ScheduleRecord, LocalHead)>, LocalStoreError> {
        let _sync = self.sync.lock().await;
        let mut records = Vec::new();
        for (id, record) in self.schedule_records_with_ids().await {
            if let Some(head) = self.local_head(&id).await? {
                records.push((id, record, head));
            }
        }
        Ok(records)
    }

    async fn decrypt_clipboard_record_preview(
        &self,
        record: &StoredPresentObjectRecord,
        encryption_key: &[u8; 32],
    ) -> Result<LocalObjectRecord, LocalStoreError> {
        let encrypted = present_encrypted_object(record)?;
        let meta = decrypt_clipboard_meta(
            &encrypted.meta_nonce,
            &encrypted.meta_ciphertext,
            encryption_key,
            &encrypted.envelope.body,
        )
        .map_err(|error| LocalStoreError::EncryptedCache(error.to_string()))?;
        let plaintext = self
            .decrypt_present_clipboard_payload(record, encryption_key)
            .await?;
        let payload_size = meta.size.unwrap_or(plaintext.len() as i64);
        let local_record = LocalObjectRecord {
            id: record.id.clone(),
            seen_generation: record.seen_generation,
            event_seq: record.event_seq,
            created_seq: record.created_seq,
            created_at: encrypted.created_at.clone(),
            source_device_id: encrypted.source_device_id.clone(),
            data: LocalObjectData::Clipboard(local_clipboard_record_from_payload(
                &meta.mime_type,
                &plaintext,
                payload_size,
            )),
        };
        Ok(local_record)
    }

    async fn decrypt_stored_clipboard_payload(
        &self,
        record: &StoredObjectRecord,
        encryption_key: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, LocalStoreError> {
        let StoredObjectRecord::Present(record) = record else {
            return Ok(None);
        };
        if record.kind != ObjectKind::Clipboard {
            return Ok(None);
        }
        self.decrypt_present_clipboard_payload(record, encryption_key)
            .await
            .map(Some)
    }

    async fn decrypt_present_clipboard_payload(
        &self,
        record: &StoredPresentObjectRecord,
        encryption_key: &[u8; 32],
    ) -> Result<Vec<u8>, LocalStoreError> {
        let encrypted = present_encrypted_object(record)?;
        let payload = single_payload(encrypted)?;
        let Some(ciphertext) = self.stored_object_payload_ciphertext(&record.id).await? else {
            return Err(LocalStoreError::EncryptedCache(
                "missing clipboard payload".into(),
            ));
        };
        verify_payload_ciphertext(payload, &ciphertext)?;
        let plaintext = decrypt_clipboard_payload(
            &payload.nonce,
            &ciphertext,
            encryption_key,
            &encrypted.envelope.body,
            payload.id,
        )
        .map_err(|error| LocalStoreError::EncryptedCache(error.to_string()))?;
        Ok(plaintext)
    }

    async fn write_memory_record(&self, record: LocalObjectRecord) -> Result<(), LocalStoreError> {
        let mut memory = self.memory.lock().await;
        memory.records.insert(record.id.clone(), record);
        Ok(())
    }

    async fn remove_memory_record(&self, object_id: &str) {
        let mut memory = self.memory.lock().await;
        memory.records.remove(object_id);
    }

    async fn all_memory_records(&self) -> Vec<LocalObjectRecord> {
        self.memory.lock().await.records.values().cloned().collect()
    }

    /// Build every view from one read of the held records.
    ///
    /// The records are cloned out of memory and sorted once: each view used to
    /// take its own deep copy, so a publish copied the working set six times.
    async fn visible_state_inner(
        &self,
        visible_clipboard_limit: usize,
    ) -> Result<LocalVisibleState, LocalStoreError> {
        let mut records = self.all_memory_records().await;
        sort_records_desc(&mut records);
        let running = self.running_actual_inner(&records).await;
        Ok(LocalVisibleState {
            clipboard_items: Self::recent_clipboard_items_inner(&records, visible_clipboard_limit),
            files: Self::file_items_inner(&records),
            collab_docs: Self::collab_items_inner(&records),
            schedule_items: self.schedule_items_inner(&records).await?,
            calendar_sources: Self::calendar_sources_inner(&records),
            running_plan: running.as_ref().and_then(|(_, planned)| *planned),
            running_actual: running.map(|(view, _)| view),
            stamp: self.visible_stamp.fetch_add(1, atomic::Ordering::SeqCst) + 1,
        })
    }

    fn profile_id(&self) -> String {
        self.profile_id
            .read()
            .expect("local store profile lock poisoned")
            .clone()
            .unwrap_or_else(|| DEFAULT_PROFILE.to_string())
    }
}

#[cfg(not(target_family = "wasm"))]
impl LocalStore {
    async fn load_or_create_device_signing_identity_inner(
        &self,
        profile_id: &str,
        wrapping_key: &[u8; 32],
    ) -> Result<DeviceSigningIdentity, LocalStoreError> {
        if let Some(record) = self.read_device_identity_record(profile_id).await? {
            match device_identity_from_record(record, profile_id, wrapping_key) {
                Ok(identity) => return Ok(identity),
                Err(
                    error @ (LocalStoreError::DeviceIdentityDecrypt(_)
                    | LocalStoreError::UnsupportedDeviceIdentityVersion(_)
                    | LocalStoreError::InvalidDeviceId(_)),
                ) => return Err(error),
                Err(error) => {
                    tracing::warn!("Replacing invalid local device identity: {}", error);
                }
            }
        }

        let identity = new_device_signing_identity(None);
        self.write_device_identity(profile_id, &identity, wrapping_key)
            .await?;
        Ok(identity)
    }

    async fn persist_device_signing_identity_inner(
        &self,
        profile_id: &str,
        identity: &DeviceSigningIdentity,
        wrapping_key: &[u8; 32],
    ) -> Result<(), LocalStoreError> {
        self.write_device_identity(profile_id, identity, wrapping_key)
            .await
    }

    async fn load_device_signing_identity_inner(
        &self,
        profile_id: &str,
        wrapping_key: &[u8; 32],
    ) -> Result<Option<DeviceSigningIdentity>, LocalStoreError> {
        let Some(record) = self.read_device_identity_record(profile_id).await? else {
            return Ok(None);
        };
        device_identity_from_record(record, profile_id, wrapping_key).map(Some)
    }

    async fn read_device_identity_record(
        &self,
        profile_id: &str,
    ) -> Result<Option<DeviceIdentityEncryptedRecord>, LocalStoreError> {
        let path = self.device_identity_path(profile_id);
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn write_device_identity(
        &self,
        profile_id: &str,
        identity: &DeviceSigningIdentity,
        wrapping_key: &[u8; 32],
    ) -> Result<(), LocalStoreError> {
        ensure_private_dir(&self.base_dir).await?;
        let record = encrypted_device_identity_record(identity, profile_id, wrapping_key)?;
        let bytes = serde_json::to_vec_pretty(&record)?;
        write_private_file_atomic(&self.device_identity_path(profile_id), &bytes).await
    }

    /// Best-effort removal of orphaned atomic-write temp files beside the
    /// device identity, so a crash or I/O error mid-write cannot leak
    /// ciphertext temps that accumulate unboundedly. The identity is the last
    /// thing here still written as a file; everything else is a database row.
    async fn sweep_orphaned_temp_files(&self) {
        sweep_orphaned_temp_files(&self.base_dir).await;
    }

    /// The store database for the current profile, opening it if needed.
    ///
    /// The profile can change within one process (a second user logging in on
    /// the same machine), and each profile has its own database, so the slot
    /// is re-opened rather than assumed.
    async fn with_database<T>(
        &self,
        operation: impl FnOnce(&mut rusqlite::Connection) -> Result<T, LocalStoreError>,
    ) -> Result<T, LocalStoreError> {
        let profile_id = self.profile_id();
        let mut slot = self.database.lock().await;
        if slot
            .as_ref()
            .is_none_or(|open| open.profile_id != profile_id)
        {
            let connection = self.open_database().await?;
            *slot = Some(OpenDatabase {
                profile_id,
                connection,
            });
        }
        let open = slot.as_mut().expect("database opened above");
        operation(&mut open.connection)
    }

    async fn open_database(&self) -> Result<rusqlite::Connection, LocalStoreError> {
        ensure_private_dir(&self.profile_root()).await?;
        let connection = sqlite::open(&self.database_path())?;
        self.discard_legacy_file_store().await;
        Ok(connection)
    }

    /// Delete the directory-of-JSON-files store this database replaces.
    ///
    /// A cutover, not a migration: the project keeps no local compatibility,
    /// and everything those files held is either a cache the server can serve
    /// again or an anchor whose loss costs one round of rollback protection on
    /// objects that predate the change. Left in place, the ciphertext would sit
    /// there unreferenced and unswept forever.
    async fn discard_legacy_file_store(&self) {
        for directory in [self.legacy_object_dir(), self.legacy_clipboard_dir()] {
            match tokio::fs::remove_dir_all(&directory).await {
                Ok(()) => tracing::info!(
                    directory = %directory.display(),
                    "Discarded the legacy file-based local store"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(
                    directory = %directory.display(),
                    "Failed to discard the legacy local store: {}",
                    error
                ),
            }
        }
    }

    async fn stored_object_record(
        &self,
        object_id: &str,
    ) -> Result<Option<StoredObjectRecord>, LocalStoreError> {
        self.with_database(|connection| sqlite::read_record(connection, object_id))
            .await
    }

    async fn write_stored_object_record(
        &self,
        record: &StoredObjectRecord,
    ) -> Result<(), LocalStoreError> {
        self.with_database(|connection| sqlite::write_record(connection, record, None))
            .await
    }

    /// Persist a record together with the payload ciphertext it describes.
    ///
    /// One call rather than two because the two are one fact: a record whose
    /// payload never landed is content this device cannot read, and the old
    /// store could produce exactly that by crashing between the writes.
    async fn write_stored_object_record_with_payload(
        &self,
        record: &StoredObjectRecord,
        ciphertext: &[u8],
    ) -> Result<(), LocalStoreError> {
        self.with_database(|connection| sqlite::write_record(connection, record, Some(ciphertext)))
            .await
    }

    async fn stored_object_payload_ciphertext(
        &self,
        object_id: &str,
    ) -> Result<Option<Vec<u8>>, LocalStoreError> {
        self.with_database(|connection| sqlite::read_payload(connection, object_id))
            .await
    }

    async fn live_stored_object_records(&self) -> Result<Vec<StoredObjectRecord>, LocalStoreError> {
        self.with_database(|connection| sqlite::live_records(connection))
            .await
    }

    async fn stale_stored_object_ids(
        &self,
        kind: ObjectKind,
        generation: u64,
        stream_start_seq: i64,
    ) -> Result<Vec<String>, LocalStoreError> {
        self.with_database(|connection| {
            sqlite::stale_object_ids(connection, kind, generation, stream_start_seq)
        })
        .await
    }

    /// Drop the cached payload of an object that is becoming a marker.
    ///
    /// Nothing to do here: the marker write deletes the object row in one
    /// transaction and the payload row cascades with it. A separate delete
    /// would be a second transaction for work already done.
    async fn discard_cached_payload(&self, _object_id: &str) -> Result<(), LocalStoreError> {
        Ok(())
    }

    #[cfg(test)]
    async fn remove_payloads_for_object(&self, object_id: &str) -> Result<(), LocalStoreError> {
        self.with_database(|connection| sqlite::delete_payload(connection, object_id))
            .await
    }

    async fn remove_stored_object_record_and_payloads(
        &self,
        record: &StoredObjectRecord,
    ) -> Result<(), LocalStoreError> {
        let object_id = record.id();
        self.with_database(|connection| sqlite::forget_object(connection, object_id))
            .await?;
        self.remove_memory_record(object_id).await;
        Ok(())
    }

    fn profile_root(&self) -> PathBuf {
        self.base_dir.join(self.profile_id())
    }

    fn database_path(&self) -> PathBuf {
        self.profile_root().join("store.sqlite3")
    }

    /// Where the pre-SQLite store kept its records and payload sidecars. Only
    /// `discard_legacy_file_store` has any use for these.
    fn legacy_object_dir(&self) -> PathBuf {
        self.profile_root().join("objects")
    }

    fn legacy_clipboard_dir(&self) -> PathBuf {
        self.profile_root().join("clipboard")
    }

    /// The device signing identity is AEAD-wrapped with a per-user key, so its
    /// on-disk slot must also be keyed per-profile: otherwise a second user on
    /// the same OS account reads the first user's record, fails to unwrap it,
    /// and is locked out. See the matching `storage_prefix` profile scoping.
    fn device_identity_path(&self, profile_id: &str) -> PathBuf {
        self.base_dir
            .join(format!("{DEVICE_IDENTITY_FILE_PREFIX}.{profile_id}.json"))
    }
}

#[cfg(target_family = "wasm")]
impl LocalStore {
    async fn load_or_create_device_signing_identity_inner(
        &self,
        profile_id: &str,
        wrapping_key: &[u8; 32],
    ) -> Result<DeviceSigningIdentity, LocalStoreError> {
        let storage = browser_storage()?;
        if let Some(json) = storage
            .get_item(&self.device_identity_key(profile_id))
            .map_err(storage_error)?
        {
            let record = serde_json::from_str::<DeviceIdentityEncryptedRecord>(&json)
                .map_err(LocalStoreError::from)?;
            match device_identity_from_record(record, profile_id, wrapping_key) {
                Ok(identity) => return Ok(identity),
                Err(
                    error @ (LocalStoreError::DeviceIdentityDecrypt(_)
                    | LocalStoreError::UnsupportedDeviceIdentityVersion(_)
                    | LocalStoreError::InvalidDeviceId(_)),
                ) => return Err(error),
                Err(error) => {
                    tracing::warn!("Replacing invalid local device identity: {}", error);
                }
            }
        }

        let identity = new_device_signing_identity(None);
        self.write_browser_device_identity(&storage, profile_id, &identity, wrapping_key)?;
        Ok(identity)
    }

    async fn persist_device_signing_identity_inner(
        &self,
        profile_id: &str,
        identity: &DeviceSigningIdentity,
        wrapping_key: &[u8; 32],
    ) -> Result<(), LocalStoreError> {
        let storage = browser_storage()?;
        self.write_browser_device_identity(&storage, profile_id, identity, wrapping_key)
    }

    async fn load_device_signing_identity_inner(
        &self,
        profile_id: &str,
        wrapping_key: &[u8; 32],
    ) -> Result<Option<DeviceSigningIdentity>, LocalStoreError> {
        let storage = browser_storage()?;
        let Some(json) = storage
            .get_item(&self.device_identity_key(profile_id))
            .map_err(storage_error)?
        else {
            return Ok(None);
        };
        let record = serde_json::from_str::<DeviceIdentityEncryptedRecord>(&json)
            .map_err(LocalStoreError::from)?;
        device_identity_from_record(record, profile_id, wrapping_key).map(Some)
    }

    fn write_browser_device_identity(
        &self,
        storage: &web_sys::Storage,
        profile_id: &str,
        identity: &DeviceSigningIdentity,
        wrapping_key: &[u8; 32],
    ) -> Result<(), LocalStoreError> {
        let record = encrypted_device_identity_record(identity, profile_id, wrapping_key)?;
        let json = serde_json::to_string(&record)?;
        storage
            .set_item(&self.device_identity_key(profile_id), &json)
            .map_err(storage_error)?;
        Ok(())
    }

    async fn stored_object_record(
        &self,
        object_id: &str,
    ) -> Result<Option<StoredObjectRecord>, LocalStoreError> {
        let storage = browser_storage()?;
        let json = storage
            .get_item(&self.object_record_key(object_id))
            .map_err(storage_error)?;
        json.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }

    async fn write_stored_object_record(
        &self,
        record: &StoredObjectRecord,
    ) -> Result<(), LocalStoreError> {
        let storage = browser_storage()?;
        let json = serde_json::to_string(record)?;
        storage
            .set_item(&self.object_record_key(record.id()), &json)
            .map_err(storage_error)?;
        let mut index = self.read_object_index(&storage)?;
        index.retain(|id| id != record.id());
        index.push(record.id().to_string());
        // The index is oldest-first (the newest id is pushed to the tail). Evict
        // the OLDEST ids once over the cap: `truncate` would instead keep the
        // front (oldest) and drop the just-pushed newest id. For each evicted id
        // also drop its record and payload siblings — enumeration is index-only
        // (there is no Storage key scan), so an off-index entry would otherwise
        // leak its ciphertext forever.
        if index.len() > OBJECT_INDEX_LIMIT {
            let overflow = index.len() - OBJECT_INDEX_LIMIT;
            for evicted_id in index.drain(..overflow) {
                let _ = storage.remove_item(&self.object_record_key(&evicted_id));
                let _ = storage.remove_item(&self.object_payload_ciphertext_key(&evicted_id));
                let _ = storage.remove_item(&self.legacy_clipboard_payload_key(&evicted_id));
            }
        }
        let index_json = serde_json::to_string(&index)?;
        storage
            .set_item(&self.object_index_key(), &index_json)
            .map_err(storage_error)?;
        Ok(())
    }

    /// The browser has no transaction to put these in, so this is still two
    /// writes; the record goes last so a failure between them leaves an
    /// unreferenced payload rather than a record that cannot be read.
    async fn write_stored_object_record_with_payload(
        &self,
        record: &StoredObjectRecord,
        ciphertext: &[u8],
    ) -> Result<(), LocalStoreError> {
        let storage = browser_storage()?;
        let json = serde_json::to_string(ciphertext)?;
        storage
            .set_item(&self.object_payload_ciphertext_key(record.id()), &json)
            .map_err(storage_error)?;
        self.write_stored_object_record(record).await
    }

    async fn stored_object_payload_ciphertext(
        &self,
        object_id: &str,
    ) -> Result<Option<Vec<u8>>, LocalStoreError> {
        let storage = browser_storage()?;
        let json = storage
            .get_item(&self.object_payload_ciphertext_key(object_id))
            .map_err(storage_error)?;
        json.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }

    /// Every object still held or still being fetched. Delete markers are
    /// filtered out to match the native store, where they are not records at
    /// all; no caller has ever wanted one.
    async fn live_stored_object_records(&self) -> Result<Vec<StoredObjectRecord>, LocalStoreError> {
        let storage = browser_storage()?;
        let mut records = Vec::new();
        for object_id in self.read_object_index(&storage)? {
            match self.stored_object_record_from_storage(&storage, &object_id) {
                Ok(Some(StoredObjectRecord::Deleted(_))) | Ok(None) => {}
                Ok(Some(record)) => records.push(record),
                Err(e) => {
                    tracing::warn!(object_id = %object_id, "Failed to read local object record: {}", e)
                }
            }
        }
        Ok(records)
    }

    /// There is no index to query, so this scans. The browser store is
    /// capped, which keeps the scan bounded.
    async fn stale_stored_object_ids(
        &self,
        kind: ObjectKind,
        generation: u64,
        stream_start_seq: i64,
    ) -> Result<Vec<String>, LocalStoreError> {
        Ok(self
            .live_stored_object_records()
            .await?
            .into_iter()
            .filter_map(|record| {
                let stored = match &record {
                    StoredObjectRecord::Present(stored) => (
                        stored.kind,
                        stored.created_seq,
                        stored.seen_generation,
                        &stored.id,
                    ),
                    StoredObjectRecord::PendingCreate(stored) => (
                        stored.kind,
                        stored.created_seq,
                        stored.seen_generation,
                        &stored.id,
                    ),
                    StoredObjectRecord::Deleted(_) => return None,
                };
                let (record_kind, created_seq, seen_generation, id) = stored;
                (record_kind == kind
                    && created_seq <= stream_start_seq
                    && seen_generation != Some(generation))
                .then(|| id.clone())
            })
            .collect())
    }

    /// The browser store has no cascade, so a marker write leaves the payload
    /// behind unless it is removed here.
    async fn discard_cached_payload(&self, object_id: &str) -> Result<(), LocalStoreError> {
        self.remove_payloads_for_object(object_id).await
    }

    /// Removal is by id, never by kind. A schedule object caches a payload
    /// too, so a kind check here would leave those behind for good.
    async fn remove_payloads_for_object(&self, object_id: &str) -> Result<(), LocalStoreError> {
        let storage = browser_storage()?;
        storage
            .remove_item(&self.object_payload_ciphertext_key(object_id))
            .map_err(storage_error)?;
        storage
            .remove_item(&self.legacy_clipboard_payload_key(object_id))
            .map_err(storage_error)?;
        Ok(())
    }

    async fn remove_stored_object_record_and_payloads(
        &self,
        record: &StoredObjectRecord,
    ) -> Result<(), LocalStoreError> {
        let storage = browser_storage()?;
        self.remove_payloads_for_object(record.id()).await?;
        storage
            .remove_item(&self.object_record_key(record.id()))
            .map_err(storage_error)?;
        let mut index = self.read_object_index(&storage)?;
        index.retain(|id| id != record.id());
        let index_json = serde_json::to_string(&index)?;
        storage
            .set_item(&self.object_index_key(), &index_json)
            .map_err(storage_error)?;
        self.remove_memory_record(record.id()).await;
        Ok(())
    }

    fn stored_object_record_from_storage(
        &self,
        storage: &web_sys::Storage,
        object_id: &str,
    ) -> Result<Option<StoredObjectRecord>, LocalStoreError> {
        let json = storage
            .get_item(&self.object_record_key(object_id))
            .map_err(storage_error)?;
        json.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }

    fn read_object_index(
        &self,
        storage: &web_sys::Storage,
    ) -> Result<Vec<String>, LocalStoreError> {
        let index_json = storage
            .get_item(&self.object_index_key())
            .map_err(storage_error)?;
        let Some(index_json) = index_json else {
            return Ok(Vec::new());
        };
        let mut index: Vec<String> = serde_json::from_str(&index_json)?;
        index.retain(|id| validate_item_id(id).is_ok());
        Ok(index)
    }

    fn storage_prefix(&self) -> String {
        format!(
            "clipper.client.v1.{}.{}",
            self.base_dir.display(),
            self.profile_id()
        )
    }

    fn legacy_clipboard_payload_key(&self, item_id: &str) -> String {
        format!("{}.clipboard_payload.{item_id}", self.storage_prefix())
    }

    fn object_payload_ciphertext_key(&self, item_id: &str) -> String {
        format!(
            "{}.object_payload_ciphertext.{item_id}",
            self.storage_prefix()
        )
    }

    fn object_index_key(&self) -> String {
        format!("{}.objects.index", self.storage_prefix())
    }

    fn object_record_key(&self, object_id: &str) -> String {
        format!("{}.objects.{object_id}", self.storage_prefix())
    }

    /// Keyed per-profile for the same reason as `device_identity_path`: the
    /// record is AEAD-wrapped with a per-user key, so a second profile in the
    /// same browser must not read (and fail to unwrap) the first's record.
    fn device_identity_key(&self, profile_id: &str) -> String {
        format!(
            "clipper.client.v1.{}.{profile_id}.device_identity_v1",
            self.base_dir.display()
        )
    }
}

/// The on-disk/local-storage device-identity record. Only the AEAD-wrapped
/// shape is accepted: an attacker who can write this storage slot but does not
/// hold the wrapping key (derived from the in-memory OPAQUE export key) cannot
/// forge a valid record, since a non-wrapped/forged record fails to
/// deserialize (required `version` + `wrapped_signing_secret_key`) and is
/// rejected fail-closed rather than silently adopted.
#[derive(Debug, Serialize, Deserialize)]
struct DeviceIdentityEncryptedRecord {
    version: u64,
    #[serde(default)]
    device_id: Option<String>,
    wrapped_signing_secret_key: Vec<u8>,
}

/// Extract the encrypted object from a present record, rejecting a collab
/// record routed here by mistake. Clipboard/file decode paths require it; a
/// `Collab` content here means a corrupt or mis-routed record.
fn present_encrypted_object(
    record: &StoredPresentObjectRecord,
) -> Result<&EncryptedObject, LocalStoreError> {
    match &record.content {
        StoredPresentContent::Encrypted(encrypted) => Ok(encrypted),
        StoredPresentContent::Collab(_) => Err(LocalStoreError::EncryptedCache(
            "expected an encrypted object but found a collab record".into(),
        )),
    }
}

fn local_head_from_present(
    record: &StoredPresentObjectRecord,
) -> Result<LocalHead, LocalStoreError> {
    let body = &present_encrypted_object(record)?.envelope.body;
    Ok(LocalHead {
        revision: body.revision,
        parent_hash: crypto::object_envelope_parent_hash(body)
            .map_err(|error| LocalStoreError::EncryptedCache(error.to_string()))?,
    })
}

fn revision_anchor_for_record(
    record: &StoredObjectRecord,
) -> Result<Option<StoredRevisionAnchor>, LocalStoreError> {
    match record {
        StoredObjectRecord::Present(present) => present_revision_anchor(present),
        StoredObjectRecord::PendingCreate(marker) | StoredObjectRecord::Deleted(marker) => {
            Ok(marker.revision_anchor)
        }
    }
}

/// The chain position a held object proves.
///
/// `None` for a kind that has no chain: collab docs are server-visible and
/// carry no signed envelope, so there is nothing for a later revision to
/// contradict.
fn present_revision_anchor(
    record: &StoredPresentObjectRecord,
) -> Result<Option<StoredRevisionAnchor>, LocalStoreError> {
    match &record.content {
        StoredPresentContent::Encrypted(_) => Ok(Some(StoredRevisionAnchor {
            head: local_head_from_present(record)?,
            kind: StoredRevisionAnchorKind::Absent,
        })),
        StoredPresentContent::Collab(_) => Ok(None),
    }
}

fn validate_revision_against_head(
    object_id: &str,
    incoming: &ObjectEnvelopeBody,
    head: LocalHead,
    require_newer: bool,
) -> Result<(), LocalStoreError> {
    if incoming.revision < head.revision || (require_newer && incoming.revision == head.revision) {
        return Err(revision_anchor_error(
            object_id,
            incoming.revision,
            "rolls back the retained revision anchor",
        ));
    }
    if incoming.revision == head.revision
        && crypto::object_envelope_parent_hash(incoming)
            .map_err(|error| LocalStoreError::EncryptedCache(error.to_string()))?
            != head.parent_hash
    {
        return Err(revision_anchor_error(
            object_id,
            incoming.revision,
            "changes the already accepted revision body",
        ));
    }
    if head.revision.checked_add(1) == Some(incoming.revision)
        && incoming.parent_hash != Some(head.parent_hash)
    {
        return Err(revision_anchor_error(
            object_id,
            incoming.revision,
            "does not chain to the retained revision anchor",
        ));
    }
    Ok(())
}

fn revision_anchor_error(object_id: &str, revision: u64, reason: &str) -> LocalStoreError {
    LocalStoreError::RevisionRejected(format!(
        "revision {revision} of object {object_id} {reason}",
    ))
}

/// Rebuild a collab display record from a stored present record. Returns `None`
/// for a non-collab content variant (a corrupt record), so a mismatched record
/// is dropped rather than surfaced.
fn collab_record_from_present(record: &StoredPresentObjectRecord) -> Option<LocalObjectRecord> {
    let StoredPresentContent::Collab(collab) = &record.content else {
        return None;
    };
    Some(LocalObjectRecord {
        id: record.id.clone(),
        seen_generation: record.seen_generation,
        event_seq: record.event_seq,
        created_seq: record.created_seq,
        created_at: collab.created_at.clone(),
        source_device_id: collab.source_device_id.clone(),
        data: LocalObjectData::Collab(LocalCollabRecord {
            title: collab.title.clone(),
            share_token: collab.share_token.clone(),
            share_url: collab.share_url.clone(),
            updated_at: collab.updated_at.clone(),
        }),
    })
}

fn decrypt_file_record(
    record: &StoredPresentObjectRecord,
    encryption_key: &[u8; 32],
) -> Result<LocalObjectRecord, LocalStoreError> {
    let encrypted = present_encrypted_object(record)?;
    let meta = decrypt_file_meta_bytes(
        &encrypted.meta_nonce,
        &encrypted.meta_ciphertext,
        encryption_key,
        &encrypted.envelope.body,
    )
    .map_err(|error| LocalStoreError::EncryptedCache(error.to_string()))?;
    let blob_size = meta.size.unwrap_or_else(|| {
        encrypted
            .payloads
            .iter()
            .fold(0_i64, |total, payload| {
                total.saturating_add(payload.ciphertext_size.max(0))
            })
    });
    let local_record = LocalObjectRecord {
        id: record.id.clone(),
        seen_generation: record.seen_generation,
        event_seq: record.event_seq,
        created_seq: record.created_seq,
        created_at: encrypted.created_at.clone(),
        source_device_id: encrypted.source_device_id.clone(),
        data: LocalObjectData::File(LocalFileRecord {
            filename: meta.filename,
            mime_type: meta.mime_type,
            blob_size,
        }),
    };
    Ok(local_record)
}

/// Render a cached schedule record as a list row, skipping records that are
/// not series definitions — an override or an actual has no row of its own.
fn schedule_item_view_from_record(
    record: &LocalObjectRecord,
    revision: u64,
) -> Option<ScheduleItemView> {
    let LocalObjectData::Schedule(schedule) = &record.data else {
        return None;
    };
    schedule
        .record
        .as_item()
        .map(|item| item_view(item, &record.id, &record.created_at, revision))
}

fn sort_records_desc(records: &mut [LocalObjectRecord]) {
    records.sort_by(|a, b| {
        b.created_seq
            .cmp(&a.created_seq)
            .then_with(|| b.id.cmp(&a.id))
    });
}

fn clipboard_item_from_record(record: &LocalObjectRecord) -> Option<DecryptedClipboardItem> {
    let LocalObjectData::Clipboard(clipboard) = &record.data else {
        return None;
    };
    Some(DecryptedClipboardItem {
        id: record.id.clone(),
        text: clipboard.text.clone(),
        mime_type: clipboard.mime_type.clone(),
        payload_size: clipboard.payload_size,
        created_at: record.created_at.clone(),
        source_device_id: record.source_device_id.clone(),
    })
}

fn file_item_from_record(record: &LocalObjectRecord) -> Option<DecryptedFileItem> {
    let LocalObjectData::File(file) = &record.data else {
        return None;
    };
    Some(DecryptedFileItem {
        id: record.id.clone(),
        filename: file.filename.clone(),
        mime_type: file.mime_type.clone(),
        blob_size: file.blob_size,
        created_at: record.created_at.clone(),
        source_device_id: record.source_device_id.clone(),
    })
}

fn collab_item_from_record(record: &LocalObjectRecord) -> Option<CollabItem> {
    let LocalObjectData::Collab(collab) = &record.data else {
        return None;
    };
    Some(CollabItem {
        id: record.id.clone(),
        title: collab.title.clone(),
        share_token: collab.share_token.clone(),
        share_url: collab.share_url.clone(),
        created_at: record.created_at.clone(),
        updated_at: collab.updated_at.clone(),
    })
}

fn local_clipboard_record_from_payload(
    mime_type: &str,
    data: &[u8],
    payload_size: i64,
) -> LocalClipboardRecord {
    LocalClipboardRecord {
        text: clipboard_display_text(mime_type, data),
        mime_type: mime_type.to_string(),
        payload_size,
    }
}

fn single_payload(
    encrypted: &EncryptedObject,
) -> Result<&ObjectPayloadDescriptor, LocalStoreError> {
    match encrypted.payloads.as_slice() {
        [payload] => Ok(payload),
        _ => Err(LocalStoreError::EncryptedCache(
            "expected exactly one cached payload".into(),
        )),
    }
}

/// Check payload bytes against the descriptor that names them.
///
/// One copy for both sides: cached bytes read back and bytes just downloaded
/// are the same question, and the engine wraps this for its own error type.
pub(crate) fn verify_payload_ciphertext(
    payload: &ObjectPayloadDescriptor,
    ciphertext: &[u8],
) -> Result<(), LocalStoreError> {
    if payload.ciphertext_size >= 0 && ciphertext.len() as i64 != payload.ciphertext_size {
        return Err(LocalStoreError::EncryptedCache(
            "payload size does not match the object envelope".into(),
        ));
    }
    if crypto::sha256(ciphertext).as_slice() != payload.sha256_ciphertext.as_slice() {
        return Err(LocalStoreError::EncryptedCache(
            "payload hash does not match the object envelope".into(),
        ));
    }
    Ok(())
}

/// The display text for a clipboard payload, bounded so a huge or hostile
/// payload cannot become the preview itself.
pub(crate) fn clipboard_display_text(mime_type: &str, data: &[u8]) -> String {
    if is_text_mime_type(mime_type) {
        bounded_text_preview(&String::from_utf8_lossy(data))
    } else {
        format!("{mime_type} clipboard payload ({} bytes)", data.len())
    }
}

fn bounded_text_preview(text: &str) -> String {
    let mut chars = text.chars();
    let preview = chars
        .by_ref()
        .take(CLIPBOARD_TEXT_PREVIEW_MAX_CHARS)
        .collect::<String>();
    if chars.next().is_none() {
        return preview;
    }

    let marker = "...";
    let keep = CLIPBOARD_TEXT_PREVIEW_MAX_CHARS.saturating_sub(marker.len());
    let mut preview = text.chars().take(keep).collect::<String>();
    preview.push_str(marker);
    preview
}

pub(crate) fn is_text_mime_type(mime_type: &str) -> bool {
    top_level_mime_type(mime_type) == "text"
}

pub(crate) fn top_level_mime_type(mime_type: &str) -> String {
    normalized_clipboard_mime_type(mime_type)
        .split('/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// The bare type, without parameters or case, which is what every comparison
/// here means by "the same MIME type".
pub(crate) fn normalized_clipboard_mime_type(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .unwrap_or(mime_type)
        .trim()
        .to_ascii_lowercase()
}

fn validate_item_id(id: &str) -> Result<String, LocalStoreError> {
    let uuid = uuid::Uuid::parse_str(id).map_err(|_| LocalStoreError::InvalidId(id.to_string()))?;
    Ok(uuid.to_string())
}

fn validate_device_id(id: &str) -> Result<String, LocalStoreError> {
    let uuid =
        uuid::Uuid::parse_str(id).map_err(|_| LocalStoreError::InvalidDeviceId(id.to_string()))?;
    Ok(uuid.to_string())
}

fn new_device_signing_identity(device_id: Option<String>) -> DeviceSigningIdentity {
    DeviceSigningIdentity {
        device_id,
        signing_secret_key: Zeroizing::new(crypto::generate_device_signing_secret_key()),
    }
}

/// AAD for the wrapped device signing secret.
///
/// The record's cleartext header is authenticated alongside the secret. A
/// rewritten `device_id`, a changed version, or a record copied from another
/// profile therefore fails to unwrap instead of being accepted.
fn device_identity_record_aad(
    version: u64,
    device_id: Option<&str>,
    profile_id: &str,
) -> Result<Vec<u8>, LocalStoreError> {
    #[derive(Serialize)]
    struct DeviceIdentityRecordAad<'a> {
        label: &'a [u8],
        version: u64,
        device_id: Option<&'a str>,
        profile_id: &'a str,
    }

    postcard::to_allocvec(&DeviceIdentityRecordAad {
        label: crypto::AAD_WRAP_DEVICE_SIGNING_SECRET_V1,
        version,
        device_id,
        profile_id,
    })
    .map_err(|error| LocalStoreError::DeviceIdentityEncrypt(format!("aad: {error}")))
}

fn encrypted_device_identity_record(
    identity: &DeviceSigningIdentity,
    profile_id: &str,
    wrapping_key: &[u8; 32],
) -> Result<DeviceIdentityEncryptedRecord, LocalStoreError> {
    let aad = device_identity_record_aad(
        DEVICE_IDENTITY_RECORD_VERSION_V3,
        identity.device_id.as_deref(),
        profile_id,
    )?;
    Ok(DeviceIdentityEncryptedRecord {
        version: DEVICE_IDENTITY_RECORD_VERSION_V3,
        device_id: identity.device_id.clone(),
        wrapped_signing_secret_key: crypto::wrap_with_key(
            wrapping_key,
            identity.signing_secret_key.as_ref(),
            &aad,
        )
        .map_err(|error| LocalStoreError::DeviceIdentityEncrypt(error.to_string()))?,
    })
}

fn device_identity_from_record(
    record: DeviceIdentityEncryptedRecord,
    profile_id: &str,
    wrapping_key: &[u8; 32],
) -> Result<DeviceSigningIdentity, LocalStoreError> {
    if record.version != DEVICE_IDENTITY_RECORD_VERSION_V3 {
        return Err(LocalStoreError::UnsupportedDeviceIdentityVersion(
            record.version,
        ));
    }
    // A malformed id is a tampered or corrupt record, not a reason to mint a
    // new identity: re-minting would lose the registered device.
    let device_id = record
        .device_id
        .as_deref()
        .map(validate_device_id)
        .transpose()?;
    // Bind the id exactly as the record stores it, which is what the write
    // path bound.
    let aad = device_identity_record_aad(record.version, record.device_id.as_deref(), profile_id)?;
    let plaintext = crypto::unwrap_with_key(wrapping_key, &record.wrapped_signing_secret_key, &aad)
        .map_err(|error| LocalStoreError::DeviceIdentityDecrypt(error.to_string()))?;
    let signing_secret_key = device_signing_secret_key_from_vec(plaintext)?;
    Ok(DeviceSigningIdentity {
        device_id,
        signing_secret_key: Zeroizing::new(signing_secret_key),
    })
}

fn device_signing_secret_key_from_vec(
    bytes: Vec<u8>,
) -> Result<[u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES], LocalStoreError> {
    bytes
        .try_into()
        .map_err(|_| LocalStoreError::InvalidDeviceSigningKey)
}

#[cfg(not(target_family = "wasm"))]
async fn ensure_private_dir(path: &Path) -> Result<(), LocalStoreError> {
    tokio::fs::create_dir_all(path).await?;
    // Inspect without following symlinks: a pre-positioned symlink here could
    // redirect plaintext cache or signing-key writes to an attacker-chosen
    // location.
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a directory", path.display()),
        )
        .into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() } as u32;
        if metadata.uid() != euid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} is owned by uid {}, expected uid {euid}",
                    path.display(),
                    metadata.uid(),
                ),
            )
            .into());
        }
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}

#[cfg(not(target_family = "wasm"))]
async fn write_private_file_atomic(path: &Path, bytes: &[u8]) -> Result<(), LocalStoreError> {
    let tmp_path = path.with_extension(format!(
        "{}.{}.tmp",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("file"),
        uuid::Uuid::now_v7()
    ));
    // Create the temp file restricted from the start (mode 0600 at open time,
    // matching the keychain pattern) so the ciphertext is never briefly
    // world-readable in the create-then-chmod gap.
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&tmp_path).await?;
    // If any step fails the uniquely-named temp would otherwise be orphaned
    // forever (no committed record is ever named `*.tmp`, and the dir scans
    // skip non-canonical names), so unlink it on error before propagating.
    if let Err(error) = write_and_commit_temp_file(&mut file, &tmp_path, path, bytes).await {
        drop(file);
        _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(error);
    }
    Ok(())
}

#[cfg(not(target_family = "wasm"))]
async fn write_and_commit_temp_file(
    file: &mut tokio::fs::File,
    tmp_path: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<(), LocalStoreError> {
    file.write_all(bytes).await?;
    file.flush().await?;
    file.sync_all().await?;
    tokio::fs::rename(tmp_path, path).await?;
    Ok(())
}

/// Remove stale atomic-write temp files (`*.tmp`) left behind by a crash or an
/// I/O error mid-write. A `*.tmp` by definition was never promoted to a
/// committed record, so deleting it is always safe; without this they
/// accumulate forever because every directory scan filters to canonical names.
#[cfg(not(target_family = "wasm"))]
async fn sweep_orphaned_temp_files(dir: &Path) {
    let mut read_dir = match tokio::fs::read_dir(dir).await {
        Ok(read_dir) => read_dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(dir = %dir.display(), "Failed to sweep temp files: {}", error);
            return;
        }
    };
    loop {
        match read_dir.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("tmp")
                    && let Err(error) = tokio::fs::remove_file(&path).await
                {
                    tracing::warn!(
                        path = %path.display(),
                        "Failed to remove stale temp file: {}",
                        error
                    );
                }
            }
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(dir = %dir.display(), "Failed to enumerate temp files: {}", error);
                break;
            }
        }
    }
}

#[cfg(target_family = "wasm")]
fn browser_storage() -> Result<web_sys::Storage, LocalStoreError> {
    let window =
        web_sys::window().ok_or_else(|| LocalStoreError::BrowserStorage("no window".into()))?;
    window
        .local_storage()
        .map_err(storage_error)?
        .ok_or_else(|| LocalStoreError::BrowserStorage("localStorage is not available".into()))
}

#[cfg(target_family = "wasm")]
fn storage_error(error: wasm_bindgen::JsValue) -> LocalStoreError {
    LocalStoreError::BrowserStorage(
        error
            .as_string()
            .unwrap_or_else(|| "browser storage operation failed".into()),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum LocalStoreError {
    #[error("local store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("local store JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[cfg(not(target_family = "wasm"))]
    #[error("local store database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("local clipboard payload is not UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("local clipboard payload decode failed: {0}")]
    PayloadDecode(String),
    #[error("invalid local clipboard item id: {0}")]
    InvalidId(String),
    #[error("invalid local device id: {0}")]
    InvalidDeviceId(String),
    #[error("invalid local device signing key")]
    InvalidDeviceSigningKey,
    #[error("unsupported local device identity version: {0}")]
    UnsupportedDeviceIdentityVersion(u64),
    #[error("encrypted local device identity error: {0}")]
    DeviceIdentityEncrypt(String),
    #[error("local device identity decrypt failed: {0}")]
    DeviceIdentityDecrypt(String),
    #[error("encrypted local cache error: {0}")]
    EncryptedCache(String),
    /// A served revision contradicts the retained anchor. The device keeps what
    /// it holds; a reconciliation pass skips the object rather than failing.
    #[error("{0}")]
    RevisionRejected(String),
    #[error("browser local storage error: {0}")]
    BrowserStorage(String),
}

#[cfg(test)]
mod tests {
    use clipper_core::models::{
        ClipboardMeta, FileMeta, OBJECT_ENVELOPE_SIGNATURE_BYTES, ObjectEnvelopeBody,
        ObjectEnvelopeOperation, ObjectEnvelopePayload,
    };

    use super::*;
    use crate::api_client::{
        encrypt_clipboard_meta, encrypt_clipboard_payload, encrypt_file_meta_bytes,
    };

    const TEST_KEY: [u8; 32] = [7; 32];
    const TEST_DEVICE_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn item(id: &str, text: &str, created_at: &str) -> DecryptedClipboardItem {
        DecryptedClipboardItem {
            id: id.into(),
            text: text.into(),
            mime_type: "text/plain".into(),
            payload_size: text.len() as i64,
            created_at: created_at.into(),
            source_device_id: TEST_DEVICE_ID.into(),
        }
    }

    fn encrypted_clipboard(item: &DecryptedClipboardItem, payload: &[u8]) -> EncryptedInlineObject {
        encrypted_clipboard_at(item, payload, 1, None, ObjectEnvelopeOperation::Create)
    }

    fn encrypted_clipboard_at(
        item: &DecryptedClipboardItem,
        payload: &[u8],
        revision: u64,
        parent_hash: Option<[u8; crypto::SHA256_BYTES]>,
        operation: ObjectEnvelopeOperation,
    ) -> EncryptedInlineObject {
        let object_id = item.id.parse().expect("object id");
        let payload_id = uuid::Uuid::now_v7().into();
        let source_device_id = item.source_device_id.parse().expect("device id");
        let aad_body = ObjectEnvelopeBody {
            object_id,
            object_type: ObjectKind::Clipboard,
            envelope_version: crypto::OBJECT_ENVELOPE_VERSION,
            revision,
            parent_hash,
            source_device_id,
            created_at: item.created_at.clone(),
            operation,
            meta_nonce: Vec::new(),
            sha256_meta_ciphertext: Vec::new(),
            payloads: vec![ObjectEnvelopePayload {
                id: payload_id,
                nonce: Vec::new(),
                ciphertext_size: 0,
                sha256_ciphertext: Vec::new(),
            }],
        };
        let meta = ClipboardMeta {
            mime_type: item.mime_type.clone(),
            size: Some(payload.len() as i64),
        };
        let (meta_nonce, meta_ciphertext) =
            encrypt_clipboard_meta(&meta, &TEST_KEY, &aad_body).expect("meta encrypt");
        let (payload_nonce, payload_ciphertext) =
            encrypt_clipboard_payload(payload, &TEST_KEY, &aad_body, payload_id)
                .expect("payload encrypt");
        let envelope_payload = ObjectEnvelopePayload {
            id: payload_id,
            nonce: payload_nonce.clone(),
            ciphertext_size: payload_ciphertext.len() as i64,
            sha256_ciphertext: crypto::sha256(&payload_ciphertext).to_vec(),
        };
        let envelope_body = ObjectEnvelopeBody {
            meta_nonce: meta_nonce.clone(),
            sha256_meta_ciphertext: crypto::sha256(&meta_ciphertext).to_vec(),
            payloads: vec![envelope_payload.clone()],
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
                created_at: item.created_at.clone(),
                source_device_id: item.source_device_id.clone(),
                envelope: ObjectEnvelope {
                    body: envelope_body,
                    signature: vec![0; OBJECT_ENVELOPE_SIGNATURE_BYTES],
                },
            },
            payload_ciphertext,
        }
    }

    fn file_item(id: &str, created_at: &str) -> DecryptedFileItem {
        DecryptedFileItem {
            id: id.into(),
            filename: "notes.txt".into(),
            mime_type: "text/plain".into(),
            blob_size: 5,
            created_at: created_at.into(),
            source_device_id: TEST_DEVICE_ID.into(),
        }
    }

    /// A file object at one revision. Files keep no cached payload, so the
    /// encrypted meta and the envelope are the whole record.
    fn encrypted_file_at(
        item: &DecryptedFileItem,
        revision: u64,
        parent_hash: Option<[u8; crypto::SHA256_BYTES]>,
        operation: ObjectEnvelopeOperation,
    ) -> EncryptedObject {
        let object_id = item.id.parse().expect("object id");
        let payload_id = uuid::Uuid::now_v7().into();
        let source_device_id = item.source_device_id.parse().expect("device id");
        let aad_body = ObjectEnvelopeBody {
            object_id,
            object_type: ObjectKind::File,
            envelope_version: crypto::OBJECT_ENVELOPE_VERSION,
            revision,
            parent_hash,
            source_device_id,
            created_at: item.created_at.clone(),
            operation,
            meta_nonce: Vec::new(),
            sha256_meta_ciphertext: Vec::new(),
            payloads: vec![ObjectEnvelopePayload {
                id: payload_id,
                nonce: Vec::new(),
                ciphertext_size: item.blob_size,
                sha256_ciphertext: Vec::new(),
            }],
        };
        let meta = FileMeta {
            filename: item.filename.clone(),
            mime_type: item.mime_type.clone(),
            size: Some(item.blob_size),
        };
        let (meta_nonce, meta_ciphertext) =
            encrypt_file_meta_bytes(&meta, &TEST_KEY, &aad_body).expect("meta encrypt");
        let envelope_body = ObjectEnvelopeBody {
            meta_nonce: meta_nonce.clone(),
            sha256_meta_ciphertext: crypto::sha256(&meta_ciphertext).to_vec(),
            ..aad_body
        };
        EncryptedObject {
            meta_nonce,
            meta_ciphertext,
            payloads: vec![ObjectPayloadDescriptor {
                id: payload_id,
                nonce: vec![0; crypto::XCHACHA20_NONCE_BYTES],
                ciphertext_size: item.blob_size,
                sha256_ciphertext: crypto::sha256(&[]).to_vec(),
            }],
            created_at: item.created_at.clone(),
            source_device_id: item.source_device_id.clone(),
            envelope: ObjectEnvelope {
                body: envelope_body,
                signature: vec![0; OBJECT_ENVELOPE_SIGNATURE_BYTES],
            },
        }
    }

    fn collab_item(id: &str, created_at: &str) -> CollabItem {
        CollabItem {
            id: id.into(),
            title: "shared doc".into(),
            share_token: "share-token".into(),
            share_url: None,
            created_at: created_at.into(),
            updated_at: created_at.into(),
        }
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn encrypts_device_identity_at_rest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        let profile = "profile-a";
        let wrapping_key = [3_u8; 32];
        let identity = DeviceSigningIdentity {
            device_id: Some(TEST_DEVICE_ID.into()),
            signing_secret_key: Zeroizing::new([9_u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]),
        };

        store
            .persist_device_signing_identity(profile, &identity, &wrapping_key)
            .await
            .expect("persist identity");

        let bytes = tokio::fs::read(store.device_identity_path(profile))
            .await
            .expect("identity bytes");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("identity json");
        assert_eq!(
            json.get("version").and_then(serde_json::Value::as_u64),
            Some(DEVICE_IDENTITY_RECORD_VERSION_V3)
        );
        assert!(json.get("wrapped_signing_secret_key").is_some());
        assert!(json.get("signing_secret_key").is_none());

        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("\"signing_secret_key\""));

        let loaded = store
            .load_or_create_device_signing_identity(profile, &wrapping_key)
            .await
            .expect("load identity");
        assert_eq!(loaded.device_id.as_deref(), Some(TEST_DEVICE_ID));
        assert_eq!(
            *loaded.signing_secret_key,
            [9_u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]
        );

        let wrong_key = [4_u8; 32];
        let err = store
            .load_or_create_device_signing_identity(profile, &wrong_key)
            .await
            .expect_err("wrong wrapping key should fail");
        assert!(matches!(err, LocalStoreError::DeviceIdentityDecrypt(_)));
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn rejects_plaintext_device_identity_record() {
        // A record without the AEAD-wrapped key (e.g. a forged plaintext record
        // written by an attacker who cannot derive the wrapping key) must be
        // rejected fail-closed, never adopted as a valid signing identity.
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        let profile = "profile-a";
        let wrapping_key = [5_u8; 32];
        let forged = serde_json::json!({
            "device_id": TEST_DEVICE_ID,
            "signing_secret_key": vec![6_u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES],
        });
        let forged_bytes = serde_json::to_vec_pretty(&forged).expect("forged json");

        ensure_private_dir(tmp.path()).await.expect("private dir");
        write_private_file_atomic(&store.device_identity_path(profile), &forged_bytes)
            .await
            .expect("write forged identity");

        let result = store
            .load_or_create_device_signing_identity(profile, &wrapping_key)
            .await;
        assert!(
            result.is_err(),
            "plaintext device-identity record must not be accepted"
        );

        // The forged record is not silently re-wrapped into a valid identity.
        let bytes = tokio::fs::read(store.device_identity_path(profile))
            .await
            .expect("identity bytes");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("identity json");
        assert!(
            json.get("wrapped_signing_secret_key").is_none(),
            "forged plaintext record must not be promoted to a wrapped record"
        );
    }

    /// Persist an identity, then rewrite one field of the stored JSON record.
    #[cfg(not(target_family = "wasm"))]
    async fn tampered_device_identity_record(
        store: &LocalStore,
        profile: &str,
        wrapping_key: &[u8; 32],
        device_id: serde_json::Value,
    ) {
        let identity = DeviceSigningIdentity {
            device_id: Some(TEST_DEVICE_ID.into()),
            signing_secret_key: Zeroizing::new([9_u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]),
        };
        store
            .persist_device_signing_identity(profile, &identity, wrapping_key)
            .await
            .expect("persist identity");

        let path = store.device_identity_path(profile);
        let bytes = tokio::fs::read(&path).await.expect("identity bytes");
        let mut json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("identity json");
        json["device_id"] = device_id;
        let bytes = serde_json::to_vec_pretty(&json).expect("tampered json");
        write_private_file_atomic(&path, &bytes)
            .await
            .expect("write tampered identity");
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn rejects_device_identity_record_with_a_substituted_device_id() {
        // `device_id` is cleartext beside the wrapped secret, and it is bound
        // into the wrap AAD. Swapping it for another valid UUID must fail the
        // tag rather than silently migrate the device.
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        let profile = "profile-a";
        let wrapping_key = [3_u8; 32];
        let other_device_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        tampered_device_identity_record(
            &store,
            profile,
            &wrapping_key,
            serde_json::json!(other_device_id),
        )
        .await;

        let err = store
            .load_or_create_device_signing_identity(profile, &wrapping_key)
            .await
            .expect_err("a substituted device id must not unwrap");
        assert!(matches!(err, LocalStoreError::DeviceIdentityDecrypt(_)));
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn rejects_device_identity_record_with_a_malformed_device_id() {
        // A malformed id is an error, not a corrupt record to replace.
        // Re-minting one would lose the registered device.
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        let profile = "profile-a";
        let wrapping_key = [3_u8; 32];
        tampered_device_identity_record(
            &store,
            profile,
            &wrapping_key,
            serde_json::json!("not-a-uuid"),
        )
        .await;

        let err = store
            .load_or_create_device_signing_identity(profile, &wrapping_key)
            .await
            .expect_err("a malformed device id must not mint a new identity");
        assert!(matches!(err, LocalStoreError::InvalidDeviceId(_)));

        let bytes = tokio::fs::read(store.device_identity_path(profile))
            .await
            .expect("identity bytes");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("identity json");
        assert_eq!(
            json.get("device_id").and_then(serde_json::Value::as_str),
            Some("not-a-uuid"),
            "the rejected record must not be replaced by a fresh identity",
        );
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn rejects_device_identity_record_copied_from_another_profile() {
        // The AAD binds the profile, so a record moved between profile slots
        // fails to unwrap even when both profiles share a wrapping key.
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        let wrapping_key = [3_u8; 32];
        let identity = DeviceSigningIdentity {
            device_id: Some(TEST_DEVICE_ID.into()),
            signing_secret_key: Zeroizing::new([9_u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]),
        };
        store
            .persist_device_signing_identity("profile-a", &identity, &wrapping_key)
            .await
            .expect("persist identity");

        let bytes = tokio::fs::read(store.device_identity_path("profile-a"))
            .await
            .expect("identity bytes");
        write_private_file_atomic(&store.device_identity_path("profile-b"), &bytes)
            .await
            .expect("copy identity");

        let err = store
            .load_or_create_device_signing_identity("profile-b", &wrapping_key)
            .await
            .expect_err("a record from another profile must not unwrap");
        assert!(matches!(err, LocalStoreError::DeviceIdentityDecrypt(_)));
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn two_profiles_share_base_dir_without_locking_each_other_out() {
        // Two users on the same OS account (same `base_dir`) each wrap their
        // device identity with their own per-user key. Keying the on-disk slot
        // per-profile must let the second user mint and load their own identity
        // instead of reading the first user's record and failing to unwrap it.
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());

        let key_a = [3_u8; 32];
        let key_b = [4_u8; 32];

        // User A registers first.
        let identity_a = store
            .load_or_create_device_signing_identity("profile-a", &key_a)
            .await
            .expect("mint identity for profile a");

        // User B logging in second must not be locked out by A's record.
        let identity_b = store
            .load_or_create_device_signing_identity("profile-b", &key_b)
            .await
            .expect("profile b must not be locked out by profile a");

        // The two profiles get distinct slots and distinct identities.
        assert_ne!(
            store.device_identity_path("profile-a"),
            store.device_identity_path("profile-b")
        );
        assert_ne!(
            *identity_a.signing_secret_key,
            *identity_b.signing_secret_key
        );

        // Each profile reloads its own identity stably with its own key.
        let reloaded_a = store
            .load_or_create_device_signing_identity("profile-a", &key_a)
            .await
            .expect("reload profile a");
        let reloaded_b = store
            .load_or_create_device_signing_identity("profile-b", &key_b)
            .await
            .expect("reload profile b");
        assert_eq!(
            *reloaded_a.signing_secret_key,
            *identity_a.signing_secret_key
        );
        assert_eq!(
            *reloaded_b.signing_secret_key,
            *identity_b.signing_secret_key
        );
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn hydrate_sweeps_orphaned_temp_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());

        // The device identity is the last thing written as a file, so its
        // atomic-write temp is the only one left that can be orphaned.
        let base_tmp = tmp
            .path()
            .join(format!("device_identity.json.{}.tmp", uuid::Uuid::now_v7()));
        tokio::fs::write(&base_tmp, b"orphaned ciphertext")
            .await
            .expect("write temp file");

        store
            .hydrate_ciphertext_cache(&TEST_KEY, 10)
            .await
            .expect("hydrate");

        assert!(
            !tokio::fs::try_exists(&base_tmp)
                .await
                .expect("exists check"),
            "stale temp file should have been swept",
        );
    }

    /// The cutover. Nothing reads the old directories any more, so leaving
    /// them would strand their ciphertext where no sweep can ever reach it.
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn opening_the_store_discards_the_file_based_one_it_replaces() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());

        let legacy_objects = store.legacy_object_dir();
        let legacy_clipboard = store.legacy_clipboard_dir();
        for directory in [&legacy_objects, &legacy_clipboard] {
            tokio::fs::create_dir_all(directory)
                .await
                .expect("legacy dir");
            tokio::fs::write(directory.join("leftover"), b"old ciphertext")
                .await
                .expect("legacy file");
        }

        let only = item(
            "33333333-3333-4333-8333-333333333333",
            "only",
            "2026-01-01T00:00:00+00:00",
        );
        let visible = store
            .persist_local_clipboard_present_encrypted(
                &only,
                only.text.as_bytes(),
                &encrypted_clipboard(&only, only.text.as_bytes()),
                1,
                1,
                10,
            )
            .await
            .expect("persist");
        assert_eq!(visible.clipboard_items.len(), 1);

        for directory in [&legacy_objects, &legacy_clipboard] {
            assert!(
                !tokio::fs::try_exists(directory)
                    .await
                    .expect("exists check"),
                "{} should have been discarded",
                directory.display(),
            );
        }
    }

    #[tokio::test]
    async fn persists_and_reads_recent_clipboard_items() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());

        let older = item(
            "11111111-1111-4111-8111-111111111111",
            "older",
            "2026-01-01T00:00:00+00:00",
        );
        let newer = item(
            "22222222-2222-4222-8222-222222222222",
            "newer",
            "2026-01-02T00:00:00+00:00",
        );

        store
            .persist_local_clipboard_present_encrypted(
                &older,
                older.text.as_bytes(),
                &encrypted_clipboard(&older, older.text.as_bytes()),
                1,
                1,
                10,
            )
            .await
            .expect("older");
        let visible = store
            .persist_local_clipboard_present_encrypted(
                &newer,
                newer.text.as_bytes(),
                &encrypted_clipboard(&newer, newer.text.as_bytes()),
                2,
                2,
                10,
            )
            .await
            .expect("newer");

        assert_eq!(visible.clipboard_items.len(), 2);
        assert_eq!(visible.clipboard_items[0].text, "newer");
        assert_eq!(visible.clipboard_items[1].text, "older");

        let payload = store
            .clipboard_payload("22222222-2222-4222-8222-222222222222", &TEST_KEY)
            .await
            .expect("payload")
            .expect("payload bytes");
        assert_eq!(payload, b"newer");

        let restored_store = LocalStore::new(tmp.path());
        restored_store.set_profile("profile-a".into());
        let restored = restored_store
            .hydrate_ciphertext_cache(&TEST_KEY, 10)
            .await
            .expect("hydrate");
        assert_eq!(restored.clipboard_items.len(), 2);
        assert_eq!(restored.clipboard_items[0].text, "newer");

        let payload = restored_store
            .clipboard_payload("22222222-2222-4222-8222-222222222222", &TEST_KEY)
            .await
            .expect("payload")
            .expect("payload bytes");
        assert_eq!(payload, b"newer");
    }

    #[tokio::test]
    async fn derives_bounded_preview_without_trusting_caller_text() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());

        let full_text = "x".repeat(CLIPBOARD_TEXT_PREVIEW_MAX_CHARS + 100);
        let item = DecryptedClipboardItem {
            id: "77777777-7777-4777-8777-777777777777".into(),
            text: "caller supplied preview".into(),
            mime_type: "text/plain".into(),
            payload_size: full_text.len() as i64,
            created_at: "2026-01-06T00:00:00+00:00".into(),
            source_device_id: TEST_DEVICE_ID.into(),
        };
        let expected_preview = format!("{}...", "x".repeat(CLIPBOARD_TEXT_PREVIEW_MAX_CHARS - 3));

        let visible = store
            .persist_local_clipboard_present_encrypted(
                &item,
                full_text.as_bytes(),
                &encrypted_clipboard(&item, full_text.as_bytes()),
                6,
                6,
                10,
            )
            .await
            .expect("persist");
        assert_eq!(visible.clipboard_items[0].text, expected_preview);

        let restored_store = LocalStore::new(tmp.path());
        restored_store.set_profile("profile-a".into());
        let restored = restored_store
            .hydrate_ciphertext_cache(&TEST_KEY, 10)
            .await
            .expect("hydrate");
        assert_eq!(restored.clipboard_items[0].text, expected_preview);

        let payload = restored_store
            .clipboard_payload("77777777-7777-4777-8777-777777777777", &TEST_KEY)
            .await
            .expect("payload")
            .expect("payload bytes");
        assert_eq!(payload, full_text.as_bytes());
    }

    #[tokio::test]
    async fn persists_image_payloads_and_uses_display_label() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());

        let image = DecryptedClipboardItem {
            id: "33333333-3333-4333-8333-333333333333".into(),
            text: "image/png clipboard payload (4 bytes)".into(),
            mime_type: "image/png".into(),
            payload_size: 4,
            created_at: "2026-01-03T00:00:00+00:00".into(),
            source_device_id: TEST_DEVICE_ID.into(),
        };

        let visible = store
            .persist_local_clipboard_present_encrypted(
                &image,
                &[0, 1, 2, 3],
                &encrypted_clipboard(&image, &[0, 1, 2, 3]),
                3,
                3,
                10,
            )
            .await
            .expect("image");

        assert_eq!(visible.clipboard_items.len(), 1);
        assert_eq!(
            visible.clipboard_items[0].text,
            "image/png clipboard payload (4 bytes)"
        );

        let payload = store
            .clipboard_payload("33333333-3333-4333-8333-333333333333", &TEST_KEY)
            .await
            .expect("payload")
            .expect("payload bytes");
        assert_eq!(payload, vec![0, 1, 2, 3]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restricts_cache_permissions_and_does_not_store_plaintext() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());

        let secret = item(
            "44444444-4444-4444-8444-444444444444",
            "super-secret",
            "2026-01-04T00:00:00+00:00",
        );
        store
            .persist_local_clipboard_present_encrypted(
                &secret,
                secret.text.as_bytes(),
                &encrypted_clipboard(&secret, secret.text.as_bytes()),
                4,
                4,
                10,
            )
            .await
            .expect("persist");

        let profile_mode = tokio::fs::metadata(store.profile_root())
            .await
            .expect("profile dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(profile_mode, 0o700, "profile dir should be 0700");

        // The write-ahead log holds the same rows as the database until a
        // checkpoint, so both files have to be private and both have to be
        // searched for the plaintext.
        let database = store.database_path();
        let write_ahead_log = database.with_extension("sqlite3-wal");
        assert!(
            tokio::fs::try_exists(&write_ahead_log)
                .await
                .expect("wal exists check"),
            "expected a write-ahead log beside the database",
        );

        let mut stored = Vec::new();
        for path in [&database, &write_ahead_log] {
            let metadata = tokio::fs::metadata(path).await.expect("metadata");
            assert_eq!(
                metadata.permissions().mode() & 0o777,
                0o600,
                "{} should be 0600",
                path.display(),
            );
            stored.extend(tokio::fs::read(path).await.expect("stored bytes"));
        }

        let secret_bytes = secret.text.as_bytes();
        assert!(
            !stored
                .windows(secret_bytes.len())
                .any(|window| window == secret_bytes),
            "the clipboard payload must never be stored in the clear",
        );
        // The preview is derived from the payload, so it must not leak either.
        assert!(!String::from_utf8_lossy(&stored).contains("super-secret"));
    }

    #[tokio::test]
    async fn rejects_path_like_ids() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        let bad = item("../escape", "bad", "2026-01-01T00:00:00+00:00");
        let good = item(
            "66666666-6666-4666-8666-666666666666",
            "bad",
            "2026-01-01T00:00:00+00:00",
        );
        let encrypted = encrypted_clipboard(&good, good.text.as_bytes());
        assert!(
            store
                .persist_local_clipboard_present_encrypted(
                    &bad,
                    bad.text.as_bytes(),
                    &encrypted,
                    1,
                    1,
                    10
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn ignores_stale_generation_snapshot_writes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());

        let stale_generation = store.start_generation().await;
        let current_generation = store.start_generation().await;
        assert_ne!(stale_generation, current_generation);

        let stale = item(
            "55555555-5555-4555-8555-555555555555",
            "stale",
            "2026-01-05T00:00:00+00:00",
        );
        let result = store
            .persist_snapshot_clipboard_present_encrypted(
                &stale,
                stale.text.as_bytes(),
                &encrypted_clipboard(&stale, stale.text.as_bytes()),
                5,
                stale_generation,
                10,
            )
            .await
            .expect("stale snapshot");
        assert!(result.is_none());

        let payload = store
            .clipboard_payload("55555555-5555-4555-8555-555555555555", &TEST_KEY)
            .await
            .expect("payload lookup");
        assert!(payload.is_none());
    }

    #[tokio::test]
    async fn delete_marker_keeps_revision_anchor_across_sweeps_and_restart() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let generation = store.start_generation().await;
        let original = item(
            "88888888-8888-4888-8888-888888888888",
            "original",
            "2026-01-08T00:00:00+00:00",
        );
        let encrypted = encrypted_clipboard(&original, original.text.as_bytes());
        store
            .persist_local_clipboard_present_encrypted(
                &original,
                original.text.as_bytes(),
                &encrypted,
                1,
                1,
                10,
            )
            .await
            .expect("persist original");
        store
            .apply_live_delete(ObjectKind::Clipboard, &original.id, 2, generation, 10)
            .await
            .expect("delete")
            .expect("current generation");

        let next_generation = store.start_generation().await;
        store
            .sweep_kind(ObjectKind::Clipboard, next_generation, 10, 10)
            .await
            .expect("sweep")
            .expect("current generation");

        let restarted = LocalStore::new(tmp.path());
        restarted.set_profile("profile-a".into());
        let error = restarted
            .persist_local_clipboard_present_encrypted(
                &original,
                original.text.as_bytes(),
                &encrypted,
                11,
                11,
                10,
            )
            .await
            .expect_err("a pre-delete revision must not be replayed after restart");
        assert!(
            error.to_string().contains("retained delete marker"),
            "unexpected error: {error}",
        );
    }

    #[tokio::test]
    async fn snapshot_absence_retains_head_without_forcing_a_new_revision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let item = item(
            "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
            "revision two",
            "2026-01-10T00:00:00+00:00",
        );
        let revision_one = encrypted_clipboard(&item, b"revision one");
        let parent_hash = crypto::object_envelope_parent_hash(&revision_one.object.envelope.body)
            .expect("parent hash");
        let revision_two = encrypted_clipboard_at(
            &item,
            item.text.as_bytes(),
            2,
            Some(parent_hash),
            ObjectEnvelopeOperation::Revise,
        );
        store
            .persist_local_clipboard_present_encrypted(
                &item,
                item.text.as_bytes(),
                &revision_two,
                2,
                2,
                10,
            )
            .await
            .expect("persist revision two");

        let generation = store.start_generation().await;
        store
            .sweep_kind(ObjectKind::Clipboard, generation, 10, 10)
            .await
            .expect("sweep")
            .expect("current generation");

        store
            .persist_local_clipboard_present_encrypted(
                &item,
                b"revision one",
                &revision_one,
                11,
                11,
                10,
            )
            .await
            .expect_err("sweep must not erase the accepted revision-two anchor");

        store
            .persist_local_clipboard_present_encrypted(
                &item,
                item.text.as_bytes(),
                &revision_two,
                12,
                12,
                10,
            )
            .await
            .expect("the same accepted head may reappear after mere absence");
    }

    #[tokio::test]
    async fn locally_signed_tombstone_is_a_durable_exact_head() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let original = item(
            "99999999-9999-4999-8999-999999999999",
            "original",
            "2026-01-09T00:00:00+00:00",
        );
        let encrypted = encrypted_clipboard(&original, original.text.as_bytes());
        store
            .persist_local_clipboard_present_encrypted(
                &original,
                original.text.as_bytes(),
                &encrypted,
                1,
                1,
                10,
            )
            .await
            .expect("persist original");
        let tombstone = LocalHead {
            revision: 2,
            parent_hash: [42; crypto::SHA256_BYTES],
        };
        store
            .apply_live_delete(ObjectKind::Clipboard, &original.id, 2, 0, 10)
            .await
            .expect("observe delete")
            .expect("current generation");
        store
            .apply_local_tombstone(ObjectKind::Clipboard, &original.id, 2, tombstone, 10)
            .await
            .expect("equal-seq local tombstone upgrades the observed-delete anchor");

        let generation = store.start_generation().await;
        store
            .sweep_kind(ObjectKind::Clipboard, generation, 10, 10)
            .await
            .expect("sweep")
            .expect("current generation");
        let restarted = LocalStore::new(tmp.path());
        restarted.set_profile("profile-a".into());
        assert_eq!(
            restarted.local_head(&original.id).await.expect("head"),
            Some(tombstone),
        );
    }

    /// The failure this closes: a clipboard payload file that goes missing made
    /// hydration delete the whole record, anchor included, and the object then
    /// had no memory of the revision it had accepted. Losing a cache entry must
    /// cost a refetch, never the rollback protection.
    #[tokio::test]
    async fn hydration_keeps_the_anchor_when_cached_content_cannot_be_read() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let item = item(
            "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff",
            "revision two",
            "2026-01-11T00:00:00+00:00",
        );
        let revision_one = encrypted_clipboard(&item, b"revision one");
        let parent_hash = crypto::object_envelope_parent_hash(&revision_one.object.envelope.body)
            .expect("parent hash");
        let revision_two = encrypted_clipboard_at(
            &item,
            item.text.as_bytes(),
            2,
            Some(parent_hash),
            ObjectEnvelopeOperation::Revise,
        );
        store
            .persist_local_clipboard_present_encrypted(
                &item,
                item.text.as_bytes(),
                &revision_two,
                2,
                2,
                10,
            )
            .await
            .expect("persist revision two");

        // A restored backup or a stray cleaner leaves this shape: the record
        // and its envelope intact, the cached payload gone.
        store
            .remove_payloads_for_object(&item.id)
            .await
            .expect("remove cached payload");

        let restarted = LocalStore::new(tmp.path());
        restarted.set_profile("profile-a".into());
        let visible = restarted
            .hydrate_ciphertext_cache(&TEST_KEY, 10)
            .await
            .expect("hydrate");
        assert!(
            visible.clipboard_items.is_empty(),
            "unreadable content must not be displayed",
        );

        restarted
            .persist_local_clipboard_present_encrypted(
                &item,
                b"revision one",
                &revision_one,
                11,
                11,
                10,
            )
            .await
            .expect_err("an unreadable cache entry must not forfeit the accepted revision");

        restarted
            .persist_local_clipboard_present_encrypted(
                &item,
                item.text.as_bytes(),
                &revision_two,
                12,
                12,
                10,
            )
            .await
            .expect("the refetched head is the one the anchor already accepted");
    }

    /// S2's claim, as a test: the anchors are not in the cache, so throwing
    /// the cache away cannot drop an anchor.
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn wiping_the_cache_leaves_every_anchor_standing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());

        // One object still held, at its second revision.
        let held = item(
            "cccccccc-1111-4111-8111-111111111111",
            "revision two",
            "2026-01-12T00:00:00+00:00",
        );
        let revision_one = encrypted_clipboard(&held, b"revision one");
        let parent_hash = crypto::object_envelope_parent_hash(&revision_one.object.envelope.body)
            .expect("parent hash");
        let revision_two = encrypted_clipboard_at(
            &held,
            held.text.as_bytes(),
            2,
            Some(parent_hash),
            ObjectEnvelopeOperation::Revise,
        );
        store
            .persist_local_clipboard_present_encrypted(
                &held,
                held.text.as_bytes(),
                &revision_two,
                2,
                2,
                10,
            )
            .await
            .expect("persist revision two");

        // One object deleted here, with a locally signed tombstone.
        let removed = item(
            "cccccccc-2222-4222-8222-222222222222",
            "removed",
            "2026-01-12T00:00:00+00:00",
        );
        store
            .persist_local_clipboard_present_encrypted(
                &removed,
                removed.text.as_bytes(),
                &encrypted_clipboard(&removed, removed.text.as_bytes()),
                3,
                3,
                10,
            )
            .await
            .expect("persist removed");
        let tombstone = LocalHead {
            revision: 2,
            parent_hash: [9; crypto::SHA256_BYTES],
        };
        store
            .apply_local_tombstone(ObjectKind::Clipboard, &removed.id, 4, tombstone, 10)
            .await
            .expect("tombstone");

        // Everything the cache holds, gone in one statement.
        store
            .with_database(|connection| {
                connection.execute("DELETE FROM objects", [])?;
                Ok(())
            })
            .await
            .expect("wipe the cache");

        assert_eq!(
            store.local_head(&removed.id).await.expect("tombstone head"),
            Some(tombstone),
            "a signed tombstone must survive a cache wipe",
        );
        store
            .persist_local_clipboard_present_encrypted(
                &held,
                b"revision one",
                &revision_one,
                11,
                11,
                10,
            )
            .await
            .expect_err("a wiped cache must not forfeit the revision it had accepted");
    }

    /// Hydration never opens a deleted object. If telling a deleted object
    /// from a live one took parsing it, every hydration would pay for every
    /// object ever deleted.
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn a_deleted_object_leaves_nothing_for_hydration_to_read() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let gone = item(
            "cccccccc-3333-4333-8333-333333333333",
            "gone",
            "2026-01-13T00:00:00+00:00",
        );
        store
            .persist_local_clipboard_present_encrypted(
                &gone,
                gone.text.as_bytes(),
                &encrypted_clipboard(&gone, gone.text.as_bytes()),
                1,
                1,
                10,
            )
            .await
            .expect("persist");
        store
            .apply_local_delete(ObjectKind::Clipboard, &gone.id, 2, 10)
            .await
            .expect("delete");

        assert!(
            store
                .live_stored_object_records()
                .await
                .expect("live records")
                .is_empty(),
            "a deleted object must not be enumerated as live",
        );
        // The memory of it is still there for anyone who asks by id.
        assert!(matches!(
            store.stored_object_record(&gone.id).await.expect("record"),
            Some(StoredObjectRecord::Deleted(_)),
        ));
    }

    /// The payload hangs off the object row and cascades with it, so no
    /// separate step has to remember to reclaim it for each kind.
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn dropping_an_object_reclaims_its_cached_payload() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let entry = item(
            "cccccccc-4444-4444-8444-444444444444",
            "payload",
            "2026-01-14T00:00:00+00:00",
        );
        store
            .persist_local_clipboard_present_encrypted(
                &entry,
                entry.text.as_bytes(),
                &encrypted_clipboard(&entry, entry.text.as_bytes()),
                1,
                1,
                10,
            )
            .await
            .expect("persist");
        assert!(
            store
                .stored_object_payload_ciphertext(&entry.id)
                .await
                .expect("payload lookup")
                .is_some()
        );

        store
            .apply_local_delete(ObjectKind::Clipboard, &entry.id, 2, 10)
            .await
            .expect("delete");

        assert!(
            store
                .stored_object_payload_ciphertext(&entry.id)
                .await
                .expect("payload lookup")
                .is_none(),
            "the cached ciphertext should go with the object row",
        );
    }

    /// A page built before the head advanced lists an older revision. That is
    /// an ordinary interleave, so the item is skipped and the pass carries on.
    /// The object stays accounted for, or the sweep that follows would drop
    /// something the server still holds.
    #[tokio::test]
    async fn a_stale_snapshot_page_skips_one_item_and_keeps_going() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let held = item(
            "ffffffff-1111-4111-8111-111111111111",
            "revision three",
            "2026-01-19T00:00:00+00:00",
        );
        let revision_two = encrypted_clipboard_at(
            &held,
            b"revision two",
            2,
            Some([2; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        let revision_three = encrypted_clipboard_at(
            &held,
            held.text.as_bytes(),
            3,
            Some([3; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        store
            .persist_local_clipboard_present_encrypted(
                &held,
                held.text.as_bytes(),
                &revision_three,
                3,
                3,
                10,
            )
            .await
            .expect("persist revision three");
        let head = store.local_head(&held.id).await.expect("head");

        let generation = store.start_generation().await;
        assert!(
            store
                .persist_snapshot_clipboard_present_encrypted(
                    &held,
                    b"revision two",
                    &revision_two,
                    2,
                    generation,
                    10,
                )
                .await
                .expect("a stale page item must not fail the pass")
                .is_none()
        );
        assert_eq!(store.local_head(&held.id).await.expect("head"), head);

        // The rest of the page still lands.
        let other = item(
            "ffffffff-2222-4222-8222-222222222222",
            "other",
            "2026-01-19T00:00:01+00:00",
        );
        store
            .persist_snapshot_clipboard_present_encrypted(
                &other,
                other.text.as_bytes(),
                &encrypted_clipboard(&other, other.text.as_bytes()),
                4,
                generation,
                10,
            )
            .await
            .expect("persist the next item")
            .expect("current generation");

        // And the sweep at the end of the pass keeps the skipped object.
        let visible = store
            .sweep_kind(ObjectKind::Clipboard, generation, 10, 10)
            .await
            .expect("sweep")
            .expect("current generation");
        assert_eq!(visible.clipboard_items.len(), 2);
        assert_eq!(store.local_head(&held.id).await.expect("head"), head);
    }

    /// The same revision with a different body is the server contradicting
    /// itself. The device keeps what it accepted.
    #[tokio::test]
    async fn an_equivocating_snapshot_body_is_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let held = item(
            "ffffffff-3333-4333-8333-333333333333",
            "revision three",
            "2026-01-20T00:00:00+00:00",
        );
        let revision_three = encrypted_clipboard_at(
            &held,
            held.text.as_bytes(),
            3,
            Some([3; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        let other_three = encrypted_clipboard_at(
            &held,
            b"another three",
            3,
            Some([9; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        store
            .persist_local_clipboard_present_encrypted(
                &held,
                held.text.as_bytes(),
                &revision_three,
                3,
                3,
                10,
            )
            .await
            .expect("persist revision three");
        let head = store.local_head(&held.id).await.expect("head");

        let generation = store.start_generation().await;
        assert!(
            store
                .persist_snapshot_clipboard_present_encrypted(
                    &held,
                    b"another three",
                    &other_three,
                    3,
                    generation,
                    10,
                )
                .await
                .expect("an equivocating body must not fail the pass")
                .is_none()
        );
        assert_eq!(store.local_head(&held.id).await.expect("head"), head);
        let hydrated = store
            .hydrate_ciphertext_cache(&TEST_KEY, 10)
            .await
            .expect("hydrate");
        assert_eq!(
            hydrated.clipboard_items.first().map(|item| item.text.clone()),
            Some("revision three".to_string()),
        );
    }

    /// A collab doc has no revision chain, so building the views must not ask
    /// for its head. Asking failed the whole view build, which meant one collab
    /// doc broke hydration and every later write.
    #[tokio::test]
    async fn a_held_collab_doc_does_not_break_the_view() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let visible = store
            .persist_local_collab_present(
                &collab_item(
                    "ffffffff-4444-4444-8444-444444444444",
                    "2026-01-21T00:00:00+00:00",
                ),
                TEST_DEVICE_ID,
                1,
                1,
                10,
            )
            .await
            .expect("persist a collab doc");
        assert_eq!(visible.collab_docs.len(), 1);
        assert!(visible.schedule_items.is_empty());

        let restarted = LocalStore::new(tmp.path());
        restarted.set_profile("profile-a".into());
        let hydrated = restarted
            .hydrate_ciphertext_cache(&TEST_KEY, 10)
            .await
            .expect("hydration must survive a collab record");
        assert_eq!(hydrated.collab_docs.len(), 1);
    }

    /// A collab listing is plaintext server metadata with no signed chain.
    /// Writing one under the id of an encrypted object must not replace that
    /// object's record: the next collab sweep would drop the record and its
    /// anchor, and revision 1 could then be replayed unchallenged.
    #[tokio::test]
    async fn a_collab_listing_cannot_erase_an_encrypted_objects_anchor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let file = file_item(
            "dddddddd-1111-4111-8111-111111111111",
            "2026-01-15T00:00:00+00:00",
        );
        let revision_one = encrypted_file_at(&file, 1, None, ObjectEnvelopeOperation::Create);
        let revision_five = encrypted_file_at(
            &file,
            5,
            Some([5; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        store
            .persist_local_file_present_encrypted(&file, &revision_five, 5, 5, 10)
            .await
            .expect("persist revision five");
        let head = store.local_head(&file.id).await.expect("head");
        assert_eq!(head.expect("held head").revision, 5);

        store
            .persist_local_collab_present(
                &collab_item(&file.id, &file.created_at),
                TEST_DEVICE_ID,
                6,
                6,
                10,
            )
            .await
            .expect("a collab listing for a held object is ignored, not an error");

        let generation = store.start_generation().await;
        store
            .sweep_kind(ObjectKind::Collab, generation, 10, 10)
            .await
            .expect("sweep")
            .expect("current generation");

        assert_eq!(
            store.local_head(&file.id).await.expect("head"),
            head,
            "a collab listing and sweep must not touch an encrypted object's head",
        );
        store
            .persist_local_file_present_encrypted(&file, &revision_one, 7, 7, 10)
            .await
            .expect_err("a collab listing must not open the door to an older revision");
    }

    /// The same hole, closed at the storage boundary: a record with no chain of
    /// its own must not take one away, and neither must forgetting an object.
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn a_chainless_record_cannot_take_an_anchor_away() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let entry = item(
            "dddddddd-2222-4222-8222-222222222222",
            "revision two",
            "2026-01-15T00:00:00+00:00",
        );
        let revision_two = encrypted_clipboard_at(
            &entry,
            entry.text.as_bytes(),
            2,
            Some([2; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        store
            .persist_local_clipboard_present_encrypted(
                &entry,
                entry.text.as_bytes(),
                &revision_two,
                2,
                2,
                10,
            )
            .await
            .expect("persist revision two");
        let held = store
            .stored_object_record(&entry.id)
            .await
            .expect("record")
            .expect("held record");

        let collab_record = StoredObjectRecord::Present(Box::new(StoredPresentObjectRecord {
            id: entry.id.clone(),
            kind: ObjectKind::Collab,
            seen_generation: None,
            event_seq: 3,
            created_seq: 3,
            content: StoredPresentContent::Collab(StoredCollabRecord {
                title: "shared doc".into(),
                share_token: "share-token".into(),
                share_url: None,
                created_at: entry.created_at.clone(),
                source_device_id: TEST_DEVICE_ID.into(),
                updated_at: entry.created_at.clone(),
            }),
        }));
        store
            .write_stored_object_record(&collab_record)
            .await
            .expect_err("a chainless record must not replace a held anchor");
        store
            .remove_stored_object_record_and_payloads(&held)
            .await
            .expect_err("an object with a chain position must not be forgotten");

        assert_eq!(
            store
                .local_head(&entry.id)
                .await
                .expect("head")
                .expect("held head")
                .revision,
            2,
        );
    }

    /// A locally signed delete whose response is delayed carries an older head
    /// than the revision another device published in the meantime. Taking it
    /// would lower the anchor and let the revisions in between be replayed.
    #[tokio::test]
    async fn a_late_local_tombstone_cannot_lower_the_anchor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let entry = item(
            "dddddddd-3333-4333-8333-333333333333",
            "revision four",
            "2026-01-16T00:00:00+00:00",
        );
        let revision_four = encrypted_clipboard_at(
            &entry,
            entry.text.as_bytes(),
            4,
            Some([4; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        store
            .persist_local_clipboard_present_encrypted(
                &entry,
                entry.text.as_bytes(),
                &revision_four,
                4,
                4,
                10,
            )
            .await
            .expect("persist revision four");
        let head = store.local_head(&entry.id).await.expect("head");

        let error = store
            .apply_local_tombstone(
                ObjectKind::Clipboard,
                &entry.id,
                99,
                LocalHead {
                    revision: 2,
                    parent_hash: [1; crypto::SHA256_BYTES],
                },
                10,
            )
            .await
            .expect_err("a late delete response must not lower the anchor");
        assert!(
            error
                .to_string()
                .contains("rolls back the retained revision anchor"),
            "unexpected error: {error}",
        );
        assert_eq!(store.local_head(&entry.id).await.expect("head"), head);
        assert!(matches!(
            store.stored_object_record(&entry.id).await.expect("record"),
            Some(StoredObjectRecord::Present(_)),
        ));
    }

    /// `local_head` reports nothing for an object that is locally gone, so a
    /// check driven by it alone ignores an absent object's anchor. This check
    /// reads the anchor itself.
    #[tokio::test]
    async fn validating_an_incoming_revision_honours_an_absent_anchor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let entry = item(
            "dddddddd-4444-4444-8444-444444444444",
            "revision three",
            "2026-01-17T00:00:00+00:00",
        );
        let revision_two = encrypted_clipboard_at(
            &entry,
            b"revision two",
            2,
            Some([2; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        let revision_three = encrypted_clipboard_at(
            &entry,
            entry.text.as_bytes(),
            3,
            Some([3; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        let other_three = encrypted_clipboard_at(
            &entry,
            b"another three",
            3,
            Some([9; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        let parent_hash = crypto::object_envelope_parent_hash(&revision_three.object.envelope.body)
            .expect("parent hash");
        let revision_four = encrypted_clipboard_at(
            &entry,
            b"revision four",
            4,
            Some(parent_hash),
            ObjectEnvelopeOperation::Revise,
        );
        let unchained_four = encrypted_clipboard_at(
            &entry,
            b"revision four",
            4,
            Some([0; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        store
            .persist_local_clipboard_present_encrypted(
                &entry,
                entry.text.as_bytes(),
                &revision_three,
                3,
                3,
                10,
            )
            .await
            .expect("persist revision three");
        let generation = store.start_generation().await;
        store
            .sweep_kind(ObjectKind::Clipboard, generation, 10, 10)
            .await
            .expect("sweep")
            .expect("current generation");
        assert!(
            store.local_head(&entry.id).await.expect("head").is_none(),
            "an absent object holds an anchor but reports no head",
        );

        store
            .validate_incoming_revision(&entry.id, &revision_two.object.envelope.body)
            .await
            .expect_err("an older revision must be refused");
        store
            .validate_incoming_revision(&entry.id, &other_three.object.envelope.body)
            .await
            .expect_err("a different body at the accepted revision must be refused");
        store
            .validate_incoming_revision(&entry.id, &unchained_four.object.envelope.body)
            .await
            .expect_err("a successor that does not chain must be refused");
        store
            .validate_incoming_revision(&entry.id, &revision_three.object.envelope.body)
            .await
            .expect("the accepted head may reappear after mere absence");
        store
            .validate_incoming_revision(&entry.id, &revision_four.object.envelope.body)
            .await
            .expect("the immediate successor is accepted");
    }

    /// A delete event with no tombstone body proves one revision followed the
    /// last visible head, so a restored object has to be two revisions on.
    #[tokio::test]
    async fn validating_an_incoming_revision_honours_an_observed_delete_anchor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        store.set_profile("profile-a".into());
        let entry = item(
            "dddddddd-5555-4555-8555-555555555555",
            "revision three",
            "2026-01-18T00:00:00+00:00",
        );
        let revision_three = encrypted_clipboard_at(
            &entry,
            entry.text.as_bytes(),
            3,
            Some([3; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        let revision_four = encrypted_clipboard_at(
            &entry,
            b"revision four",
            4,
            Some([4; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        let revision_five = encrypted_clipboard_at(
            &entry,
            b"revision five",
            5,
            Some([5; crypto::SHA256_BYTES]),
            ObjectEnvelopeOperation::Revise,
        );
        store
            .persist_local_clipboard_present_encrypted(
                &entry,
                entry.text.as_bytes(),
                &revision_three,
                3,
                3,
                10,
            )
            .await
            .expect("persist revision three");
        store
            .apply_live_delete(ObjectKind::Clipboard, &entry.id, 4, 0, 10)
            .await
            .expect("observe delete")
            .expect("current generation");
        assert!(
            store.local_head(&entry.id).await.expect("head").is_none(),
            "an observed delete leaves an anchor but no head",
        );

        store
            .validate_incoming_revision(&entry.id, &revision_four.object.envelope.body)
            .await
            .expect_err("the revision the tombstone replaced must be refused");
        store
            .validate_incoming_revision(&entry.id, &revision_five.object.envelope.body)
            .await
            .expect("a restore past the tombstone is accepted");
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod adversarial_tests;
