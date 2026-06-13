//! Client-side local cache.
//!
//! Persisted clipboard/file object cache records contain only encrypted object
//! material. Decrypted display state keeps only bounded clipboard previews in
//! memory; full clipboard payload bytes are decrypted from the local ciphertext
//! record only for the operation that needs them.

#[cfg(not(target_family = "wasm"))]
use std::path::Path;
use std::{collections::HashMap, path::PathBuf, sync::RwLock};

use clipper_app_types::{DecryptedClipboardItem, DecryptedFileItem};
use clipper_core::{
    crypto,
    models::{ObjectEnvelopeV1, ObjectKind, ObjectPayloadDescriptor},
};
use serde::{Deserialize, Serialize};
#[cfg(not(target_family = "wasm"))]
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use crate::api_client::{
    decrypt_clipboard_meta, decrypt_clipboard_payload, decrypt_file_meta_bytes,
};

const DEFAULT_PROFILE: &str = "default";
const CLIPBOARD_TEXT_PREVIEW_MAX_CHARS: usize = 512;
#[cfg(not(target_family = "wasm"))]
const DEVICE_IDENTITY_FILE: &str = "device-identity-v1.json";
const DEVICE_IDENTITY_RECORD_VERSION_V2: u64 = 2;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedObject {
    pub meta_nonce: Vec<u8>,
    pub meta_ciphertext: Vec<u8>,
    pub payloads: Vec<ObjectPayloadDescriptor>,
    pub created_at: String,
    pub source_device_id: String,
    pub envelope: ObjectEnvelopeV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedClipboardObject {
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
    encrypted: EncryptedObject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSyncMarkerRecord {
    id: String,
    kind: ObjectKind,
    seen_generation: Option<u64>,
    event_seq: i64,
    created_seq: i64,
}

impl StoredObjectRecord {
    fn id(&self) -> &str {
        match self {
            Self::Present(record) => &record.id,
            Self::PendingCreate(record) | Self::Deleted(record) => &record.id,
        }
    }

    fn kind(&self) -> ObjectKind {
        match self {
            Self::Present(record) => record.kind,
            Self::PendingCreate(record) | Self::Deleted(record) => record.kind,
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
    pub clipboard_items: Vec<DecryptedClipboardItem>,
    pub files: Vec<DecryptedFileItem>,
}

#[derive(Debug, Default)]
struct LocalSyncControl {
    generation: u64,
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
}

impl LocalStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            profile_id: RwLock::new(None),
            sync: Mutex::new(LocalSyncControl::default()),
            memory: Mutex::new(MemoryState::default()),
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

    pub async fn clear_memory(&self) {
        *self.memory.lock().await = MemoryState::default();
    }

    pub async fn persist_local_clipboard_present_encrypted(
        &self,
        item: &DecryptedClipboardItem,
        payload: &[u8],
        encrypted: &EncryptedClipboardObject,
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
        encrypted: &EncryptedClipboardObject,
        created_seq: i64,
        generation: u64,
        visible_clipboard_limit: usize,
    ) -> Result<Option<LocalVisibleState>, LocalStoreError> {
        let item_id = validate_item_id(&item.id)?;
        let sync = self.sync.lock().await;
        if sync.generation != generation {
            return Ok(None);
        }
        self.persist_clipboard_present_encrypted_inner(
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
        .await?;
        self.visible_state_inner(visible_clipboard_limit)
            .await
            .map(Some)
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
        self.persist_file_present_encrypted_inner(
            &item_id,
            item,
            encrypted,
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
        let mut memory = MemoryState::default();
        for record in self.all_stored_object_records().await? {
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
                    self.remove_stored_object_record_and_payloads(&record)
                        .await?;
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

    pub async fn apply_local_delete(
        &self,
        kind: ObjectKind,
        object_id: &str,
        event_seq: i64,
        visible_clipboard_limit: usize,
    ) -> Result<LocalVisibleState, LocalStoreError> {
        let object_id = validate_item_id(object_id)?;
        let sync = self.sync.lock().await;
        self.apply_delete_inner(kind, &object_id, event_seq, sync.generation)
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
        self.apply_delete_inner(kind, &object_id, event_seq, generation)
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
        self.remove_object_inner(&object_id).await?;
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
        wrapping_key: &[u8; 32],
    ) -> Result<DeviceSigningIdentity, LocalStoreError> {
        self.load_or_create_device_signing_identity_inner(wrapping_key)
            .await
    }

    pub async fn persist_device_signing_identity(
        &self,
        identity: &DeviceSigningIdentity,
        wrapping_key: &[u8; 32],
    ) -> Result<(), LocalStoreError> {
        if let Some(device_id) = identity.device_id.as_deref() {
            validate_device_id(device_id)?;
        }
        self.persist_device_signing_identity_inner(identity, wrapping_key)
            .await
    }

    async fn persist_clipboard_present_encrypted_inner(
        &self,
        item_id: &str,
        item: &DecryptedClipboardItem,
        payload: &[u8],
        encrypted: &EncryptedClipboardObject,
        sync_meta: StoredObjectSyncMeta,
    ) -> Result<(), LocalStoreError> {
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
            encrypted: encrypted.object.clone(),
        }));
        self.write_stored_clipboard_payload(item_id, &encrypted.payload_ciphertext)
            .await?;
        self.write_stored_object_record(&stored_record).await?;
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
            encrypted: encrypted.clone(),
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
            Some(StoredObjectRecord::PendingCreate(mut record)) => {
                if created_seq >= record.event_seq {
                    record.event_seq = created_seq;
                    record.created_seq = created_seq;
                    record.seen_generation = Some(generation);
                    self.write_stored_object_record(&StoredObjectRecord::PendingCreate(record))
                        .await?;
                }
                Ok(true)
            }
            _ => {
                let record = StoredObjectRecord::PendingCreate(StoredSyncMarkerRecord {
                    id: object_id.to_string(),
                    kind,
                    seen_generation: Some(generation),
                    event_seq: created_seq,
                    created_seq,
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
    ) -> Result<(), LocalStoreError> {
        if let Some(record) = self.stored_object_record(object_id).await?
            && record.event_seq() >= event_seq
        {
            return Ok(());
        }
        self.remove_payloads_for_object(kind, object_id).await?;
        self.remove_memory_record(object_id).await;
        let record = StoredObjectRecord::Deleted(StoredSyncMarkerRecord {
            id: object_id.to_string(),
            kind,
            seen_generation: Some(generation),
            event_seq,
            created_seq: event_seq,
        });
        self.write_stored_object_record(&record).await
    }

    async fn sweep_kind_inner(
        &self,
        kind: ObjectKind,
        generation: u64,
        stream_start_seq: i64,
    ) -> Result<(), LocalStoreError> {
        for record in self.all_stored_object_records().await? {
            if record.kind() != kind {
                continue;
            }
            match &record {
                StoredObjectRecord::Present(stored)
                    if stored.created_seq <= stream_start_seq
                        && stored.seen_generation != Some(generation) =>
                {
                    self.remove_stored_object_record_and_payloads(&record)
                        .await?;
                }
                StoredObjectRecord::Deleted(stored)
                    if stored.seen_generation != Some(generation) =>
                {
                    self.remove_stored_object_record_and_payloads(&record)
                        .await?;
                }
                StoredObjectRecord::PendingCreate(stored)
                    if stored.created_seq <= stream_start_seq
                        && stored.seen_generation != Some(generation) =>
                {
                    self.remove_stored_object_record_and_payloads(&record)
                        .await?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn remove_object_inner(&self, object_id: &str) -> Result<(), LocalStoreError> {
        if let Some(record) = self.stored_object_record(object_id).await? {
            self.remove_stored_object_record_and_payloads(&record)
                .await?;
        } else {
            self.remove_memory_record(object_id).await;
        }
        Ok(())
    }

    async fn recent_clipboard_items_inner(
        &self,
        limit: usize,
    ) -> Result<Vec<DecryptedClipboardItem>, LocalStoreError> {
        let mut records = self.all_memory_records().await;
        sort_records_desc(&mut records);
        let mut items = records
            .iter()
            .filter_map(clipboard_item_from_record)
            .collect::<Vec<_>>();
        items.truncate(limit);
        Ok(items)
    }

    async fn file_items_inner(&self) -> Result<Vec<DecryptedFileItem>, LocalStoreError> {
        let mut records = self.all_memory_records().await;
        sort_records_desc(&mut records);
        Ok(records.iter().filter_map(file_item_from_record).collect())
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
        }
    }

    async fn decrypt_clipboard_record_preview(
        &self,
        record: &StoredPresentObjectRecord,
        encryption_key: &[u8; 32],
    ) -> Result<LocalObjectRecord, LocalStoreError> {
        let encrypted = &record.encrypted;
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
        let encrypted = &record.encrypted;
        let payload = single_payload(encrypted)?;
        let Some(ciphertext) = self.stored_clipboard_payload_ciphertext(&record.id).await? else {
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

    async fn visible_state_inner(
        &self,
        visible_clipboard_limit: usize,
    ) -> Result<LocalVisibleState, LocalStoreError> {
        Ok(LocalVisibleState {
            clipboard_items: self
                .recent_clipboard_items_inner(visible_clipboard_limit)
                .await?,
            files: self.file_items_inner().await?,
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
        wrapping_key: &[u8; 32],
    ) -> Result<DeviceSigningIdentity, LocalStoreError> {
        if let Some(record) = self.read_device_identity_record().await? {
            let migrate_plaintext = matches!(&record, StoredDeviceIdentityRecord::Plaintext(_));
            match device_identity_from_record(record, wrapping_key) {
                Ok(identity) => {
                    if migrate_plaintext {
                        self.write_device_identity(&identity, wrapping_key).await?;
                    }
                    return Ok(identity);
                }
                Err(
                    error @ (LocalStoreError::DeviceIdentityDecrypt(_)
                    | LocalStoreError::UnsupportedDeviceIdentityVersion(_)),
                ) => return Err(error),
                Err(error) => {
                    tracing::warn!("Replacing invalid local device identity: {}", error);
                }
            }
        }

        let identity = new_device_signing_identity(None);
        self.write_device_identity(&identity, wrapping_key).await?;
        Ok(identity)
    }

    async fn persist_device_signing_identity_inner(
        &self,
        identity: &DeviceSigningIdentity,
        wrapping_key: &[u8; 32],
    ) -> Result<(), LocalStoreError> {
        self.write_device_identity(identity, wrapping_key).await
    }

    async fn read_device_identity_record(
        &self,
    ) -> Result<Option<StoredDeviceIdentityRecord>, LocalStoreError> {
        let path = self.device_identity_path();
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn write_device_identity(
        &self,
        identity: &DeviceSigningIdentity,
        wrapping_key: &[u8; 32],
    ) -> Result<(), LocalStoreError> {
        ensure_private_dir(&self.base_dir).await?;
        let record = encrypted_device_identity_record(identity, wrapping_key)?;
        let bytes = serde_json::to_vec_pretty(&record)?;
        write_private_file_atomic(&self.device_identity_path(), &bytes).await
    }

    async fn stored_object_record(
        &self,
        object_id: &str,
    ) -> Result<Option<StoredObjectRecord>, LocalStoreError> {
        let path = self.object_record_path(object_id);
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn write_stored_object_record(
        &self,
        record: &StoredObjectRecord,
    ) -> Result<(), LocalStoreError> {
        ensure_private_dir(&self.object_dir()).await?;
        let bytes = serde_json::to_vec_pretty(record)?;
        write_private_file_atomic(&self.object_record_path(record.id()), &bytes).await
    }

    async fn write_stored_clipboard_payload(
        &self,
        object_id: &str,
        ciphertext: &[u8],
    ) -> Result<(), LocalStoreError> {
        ensure_private_dir(&self.clipboard_dir()).await?;
        write_private_file_atomic(
            &self.clipboard_payload_ciphertext_path(object_id),
            ciphertext,
        )
        .await
    }

    async fn stored_clipboard_payload_ciphertext(
        &self,
        object_id: &str,
    ) -> Result<Option<Vec<u8>>, LocalStoreError> {
        match tokio::fs::read(self.clipboard_payload_ciphertext_path(object_id)).await {
            Ok(ciphertext) => Ok(Some(ciphertext)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn all_stored_object_records(&self) -> Result<Vec<StoredObjectRecord>, LocalStoreError> {
        let dir = self.object_dir();
        let mut read_dir = match tokio::fs::read_dir(&dir).await {
            Ok(read_dir) => read_dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };

        let mut records = Vec::new();
        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match tokio::fs::read(&path)
                .await
                .map_err(LocalStoreError::from)
                .and_then(|bytes| {
                    serde_json::from_slice::<StoredObjectRecord>(&bytes).map_err(Into::into)
                }) {
                Ok(record) => records.push(record),
                Err(e) => {
                    tracing::warn!(path = %path.display(), "Failed to read local object record: {}", e)
                }
            }
        }
        Ok(records)
    }

    async fn remove_payloads_for_object(
        &self,
        kind: ObjectKind,
        object_id: &str,
    ) -> Result<(), LocalStoreError> {
        if kind == ObjectKind::Clipboard {
            _ = tokio::fs::remove_file(self.clipboard_payload_ciphertext_path(object_id)).await;
            _ = tokio::fs::remove_file(self.clipboard_dir().join(format!("{object_id}.payload")))
                .await;
            _ = tokio::fs::remove_file(self.clipboard_dir().join(format!("{object_id}.txt"))).await;
        }
        Ok(())
    }

    async fn remove_stored_object_record_and_payloads(
        &self,
        record: &StoredObjectRecord,
    ) -> Result<(), LocalStoreError> {
        self.remove_payloads_for_object(record.kind(), record.id())
            .await?;
        match tokio::fs::remove_file(self.object_record_path(record.id())).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.remove_memory_record(record.id()).await;
        Ok(())
    }

    fn profile_root(&self) -> PathBuf {
        self.base_dir.join(self.profile_id())
    }

    fn clipboard_dir(&self) -> PathBuf {
        self.profile_root().join("clipboard")
    }

    fn object_dir(&self) -> PathBuf {
        self.profile_root().join("objects")
    }

    fn object_record_path(&self, object_id: &str) -> PathBuf {
        self.object_dir().join(format!("{object_id}.json"))
    }

    fn clipboard_payload_ciphertext_path(&self, object_id: &str) -> PathBuf {
        self.clipboard_dir()
            .join(format!("{object_id}.payload.ciphertext"))
    }

    fn device_identity_path(&self) -> PathBuf {
        self.base_dir.join(DEVICE_IDENTITY_FILE)
    }
}

#[cfg(target_family = "wasm")]
impl LocalStore {
    async fn load_or_create_device_signing_identity_inner(
        &self,
        wrapping_key: &[u8; 32],
    ) -> Result<DeviceSigningIdentity, LocalStoreError> {
        let storage = browser_storage()?;
        if let Some(json) = storage
            .get_item(&self.device_identity_key())
            .map_err(storage_error)?
        {
            let record = serde_json::from_str::<StoredDeviceIdentityRecord>(&json)
                .map_err(LocalStoreError::from)?;
            let migrate_plaintext = matches!(&record, StoredDeviceIdentityRecord::Plaintext(_));
            match device_identity_from_record(record, wrapping_key) {
                Ok(identity) => {
                    if migrate_plaintext {
                        self.write_browser_device_identity(&storage, &identity, wrapping_key)?;
                    }
                    return Ok(identity);
                }
                Err(
                    error @ (LocalStoreError::DeviceIdentityDecrypt(_)
                    | LocalStoreError::UnsupportedDeviceIdentityVersion(_)),
                ) => return Err(error),
                Err(error) => {
                    tracing::warn!("Replacing invalid local device identity: {}", error);
                }
            }
        }

        let identity = new_device_signing_identity(None);
        self.write_browser_device_identity(&storage, &identity, wrapping_key)?;
        Ok(identity)
    }

    async fn persist_device_signing_identity_inner(
        &self,
        identity: &DeviceSigningIdentity,
        wrapping_key: &[u8; 32],
    ) -> Result<(), LocalStoreError> {
        let storage = browser_storage()?;
        self.write_browser_device_identity(&storage, identity, wrapping_key)
    }

    fn write_browser_device_identity(
        &self,
        storage: &web_sys::Storage,
        identity: &DeviceSigningIdentity,
        wrapping_key: &[u8; 32],
    ) -> Result<(), LocalStoreError> {
        let record = encrypted_device_identity_record(identity, wrapping_key)?;
        let json = serde_json::to_string(&record)?;
        storage
            .set_item(&self.device_identity_key(), &json)
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
        index.truncate(OBJECT_INDEX_LIMIT);
        let index_json = serde_json::to_string(&index)?;
        storage
            .set_item(&self.object_index_key(), &index_json)
            .map_err(storage_error)?;
        Ok(())
    }

    async fn write_stored_clipboard_payload(
        &self,
        object_id: &str,
        ciphertext: &[u8],
    ) -> Result<(), LocalStoreError> {
        let storage = browser_storage()?;
        let json = serde_json::to_string(ciphertext)?;
        storage
            .set_item(&self.clipboard_payload_ciphertext_key(object_id), &json)
            .map_err(storage_error)?;
        Ok(())
    }

    async fn stored_clipboard_payload_ciphertext(
        &self,
        object_id: &str,
    ) -> Result<Option<Vec<u8>>, LocalStoreError> {
        let storage = browser_storage()?;
        let json = storage
            .get_item(&self.clipboard_payload_ciphertext_key(object_id))
            .map_err(storage_error)?;
        json.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }

    async fn all_stored_object_records(&self) -> Result<Vec<StoredObjectRecord>, LocalStoreError> {
        let storage = browser_storage()?;
        let mut records = Vec::new();
        for object_id in self.read_object_index(&storage)? {
            match self.stored_object_record_from_storage(&storage, &object_id) {
                Ok(Some(record)) => records.push(record),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(object_id = %object_id, "Failed to read local object record: {}", e)
                }
            }
        }
        Ok(records)
    }

    async fn remove_payloads_for_object(
        &self,
        kind: ObjectKind,
        object_id: &str,
    ) -> Result<(), LocalStoreError> {
        if kind == ObjectKind::Clipboard {
            let storage = browser_storage()?;
            storage
                .remove_item(&self.clipboard_payload_ciphertext_key(object_id))
                .map_err(storage_error)?;
            storage
                .remove_item(&self.legacy_clipboard_payload_key(object_id))
                .map_err(storage_error)?;
        }
        Ok(())
    }

    async fn remove_stored_object_record_and_payloads(
        &self,
        record: &StoredObjectRecord,
    ) -> Result<(), LocalStoreError> {
        let storage = browser_storage()?;
        self.remove_payloads_for_object(record.kind(), record.id())
            .await?;
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

    fn clipboard_payload_ciphertext_key(&self, item_id: &str) -> String {
        format!(
            "{}.clipboard_payload_ciphertext.{item_id}",
            self.storage_prefix()
        )
    }

    fn object_index_key(&self) -> String {
        format!("{}.objects.index", self.storage_prefix())
    }

    fn object_record_key(&self, object_id: &str) -> String {
        format!("{}.objects.{object_id}", self.storage_prefix())
    }

    fn device_identity_key(&self) -> String {
        format!(
            "clipper.client.v1.{}.device_identity_v1",
            self.base_dir.display()
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredDeviceIdentityRecord {
    Encrypted(DeviceIdentityEncryptedRecord),
    Plaintext(DeviceIdentityPlaintextRecord),
}

#[derive(Debug, Serialize, Deserialize)]
struct DeviceIdentityEncryptedRecord {
    version: u64,
    #[serde(default)]
    device_id: Option<String>,
    wrapped_signing_secret_key: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeviceIdentityPlaintextRecord {
    #[serde(default)]
    device_id: Option<String>,
    signing_secret_key: Vec<u8>,
}

fn decrypt_file_record(
    record: &StoredPresentObjectRecord,
    encryption_key: &[u8; 32],
) -> Result<LocalObjectRecord, LocalStoreError> {
    let encrypted = &record.encrypted;
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
            .map(|payload| payload.ciphertext_size.max(0))
            .sum()
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

fn verify_payload_ciphertext(
    payload: &ObjectPayloadDescriptor,
    ciphertext: &[u8],
) -> Result<(), LocalStoreError> {
    if payload.ciphertext_size >= 0 && ciphertext.len() as i64 != payload.ciphertext_size {
        return Err(LocalStoreError::EncryptedCache(
            "cached payload size mismatch".into(),
        ));
    }
    if crypto::sha256(ciphertext).as_slice() != payload.sha256_ciphertext.as_slice() {
        return Err(LocalStoreError::EncryptedCache(
            "cached payload hash mismatch".into(),
        ));
    }
    Ok(())
}

fn clipboard_display_text(mime_type: &str, data: &[u8]) -> String {
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

fn is_text_mime_type(mime_type: &str) -> bool {
    mime_type
        .split(';')
        .next()
        .map(|base| {
            base.trim().eq_ignore_ascii_case("text/plain") || base.trim().starts_with("text/")
        })
        .unwrap_or(false)
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

fn encrypted_device_identity_record(
    identity: &DeviceSigningIdentity,
    wrapping_key: &[u8; 32],
) -> Result<DeviceIdentityEncryptedRecord, LocalStoreError> {
    Ok(DeviceIdentityEncryptedRecord {
        version: DEVICE_IDENTITY_RECORD_VERSION_V2,
        device_id: identity.device_id.clone(),
        wrapped_signing_secret_key: crypto::wrap_with_key(
            wrapping_key,
            identity.signing_secret_key.as_ref(),
            crypto::AAD_WRAP_DEVICE_SIGNING_SECRET_V1,
        )
        .map_err(|error| LocalStoreError::DeviceIdentityEncrypt(error.to_string()))?,
    })
}

fn device_identity_from_record(
    record: StoredDeviceIdentityRecord,
    wrapping_key: &[u8; 32],
) -> Result<DeviceSigningIdentity, LocalStoreError> {
    match record {
        StoredDeviceIdentityRecord::Encrypted(record) => {
            if record.version != DEVICE_IDENTITY_RECORD_VERSION_V2 {
                return Err(LocalStoreError::UnsupportedDeviceIdentityVersion(
                    record.version,
                ));
            }
            let device_id = record
                .device_id
                .map(|id| validate_device_id(&id))
                .transpose()?;
            let plaintext = crypto::unwrap_with_key(
                wrapping_key,
                &record.wrapped_signing_secret_key,
                crypto::AAD_WRAP_DEVICE_SIGNING_SECRET_V1,
            )
            .map_err(|error| LocalStoreError::DeviceIdentityDecrypt(error.to_string()))?;
            let signing_secret_key = device_signing_secret_key_from_vec(plaintext)?;
            Ok(DeviceSigningIdentity {
                device_id,
                signing_secret_key: Zeroizing::new(signing_secret_key),
            })
        }
        StoredDeviceIdentityRecord::Plaintext(record) => {
            device_identity_from_plaintext_record(record)
        }
    }
}

fn device_identity_from_plaintext_record(
    record: DeviceIdentityPlaintextRecord,
) -> Result<DeviceSigningIdentity, LocalStoreError> {
    let device_id = record
        .device_id
        .map(|id| validate_device_id(&id))
        .transpose()?;
    let signing_secret_key = device_signing_secret_key_from_vec(record.signing_secret_key)?;
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
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .await?;
    }
    file.write_all(bytes).await?;
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&tmp_path, path).await?;
    Ok(())
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
    #[error("browser local storage error: {0}")]
    BrowserStorage(String),
}

#[cfg(test)]
mod tests {
    use clipper_core::models::{
        ClipboardMeta, OBJECT_ENVELOPE_SIGNATURE_BYTES, ObjectEnvelopeBodyV1,
        ObjectEnvelopeOperation, ObjectEnvelopePayloadV1,
    };

    use super::*;
    use crate::api_client::{encrypt_clipboard_meta, encrypt_clipboard_payload};

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

    fn encrypted_clipboard(
        item: &DecryptedClipboardItem,
        payload: &[u8],
    ) -> EncryptedClipboardObject {
        let object_id = item.id.parse().expect("object id");
        let payload_id = uuid::Uuid::now_v7().into();
        let source_device_id = item.source_device_id.parse().expect("device id");
        let aad_body = ObjectEnvelopeBodyV1 {
            object_id,
            object_type: ObjectKind::Clipboard,
            object_version: 1,
            source_device_id,
            created_at: item.created_at.clone(),
            operation: ObjectEnvelopeOperation::Create,
            meta_nonce: Vec::new(),
            sha256_meta_ciphertext: Vec::new(),
            payloads: vec![ObjectEnvelopePayloadV1 {
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
        let envelope_payload = ObjectEnvelopePayloadV1 {
            id: payload_id,
            nonce: payload_nonce.clone(),
            ciphertext_size: payload_ciphertext.len() as i64,
            sha256_ciphertext: crypto::sha256(&payload_ciphertext).to_vec(),
        };
        let envelope_body = ObjectEnvelopeBodyV1 {
            meta_nonce: meta_nonce.clone(),
            sha256_meta_ciphertext: crypto::sha256(&meta_ciphertext).to_vec(),
            payloads: vec![envelope_payload.clone()],
            ..aad_body
        };
        EncryptedClipboardObject {
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
                envelope: ObjectEnvelopeV1 {
                    body: envelope_body,
                    signature: vec![0; OBJECT_ENVELOPE_SIGNATURE_BYTES],
                },
            },
            payload_ciphertext,
        }
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn encrypts_device_identity_at_rest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        let wrapping_key = [3_u8; 32];
        let identity = DeviceSigningIdentity {
            device_id: Some(TEST_DEVICE_ID.into()),
            signing_secret_key: Zeroizing::new([9_u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]),
        };

        store
            .persist_device_signing_identity(&identity, &wrapping_key)
            .await
            .expect("persist identity");

        let bytes = tokio::fs::read(store.device_identity_path())
            .await
            .expect("identity bytes");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("identity json");
        assert_eq!(
            json.get("version").and_then(serde_json::Value::as_u64),
            Some(DEVICE_IDENTITY_RECORD_VERSION_V2)
        );
        assert!(json.get("wrapped_signing_secret_key").is_some());
        assert!(json.get("signing_secret_key").is_none());

        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("\"signing_secret_key\""));

        let loaded = store
            .load_or_create_device_signing_identity(&wrapping_key)
            .await
            .expect("load identity");
        assert_eq!(loaded.device_id.as_deref(), Some(TEST_DEVICE_ID));
        assert_eq!(
            *loaded.signing_secret_key,
            [9_u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]
        );

        let wrong_key = [4_u8; 32];
        let err = store
            .load_or_create_device_signing_identity(&wrong_key)
            .await
            .expect_err("wrong wrapping key should fail");
        assert!(matches!(err, LocalStoreError::DeviceIdentityDecrypt(_)));
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn migrates_plaintext_device_identity_to_encrypted_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        let wrapping_key = [5_u8; 32];
        let legacy = DeviceIdentityPlaintextRecord {
            device_id: Some(TEST_DEVICE_ID.into()),
            signing_secret_key: vec![6_u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES],
        };
        let legacy_bytes = serde_json::to_vec_pretty(&legacy).expect("legacy json");

        ensure_private_dir(tmp.path()).await.expect("private dir");
        write_private_file_atomic(&store.device_identity_path(), &legacy_bytes)
            .await
            .expect("write legacy identity");

        let loaded = store
            .load_or_create_device_signing_identity(&wrapping_key)
            .await
            .expect("load legacy identity");
        assert_eq!(loaded.device_id.as_deref(), Some(TEST_DEVICE_ID));
        assert_eq!(
            *loaded.signing_secret_key,
            [6_u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]
        );

        let bytes = tokio::fs::read(store.device_identity_path())
            .await
            .expect("migrated identity bytes");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("migrated json");
        assert_eq!(
            json.get("version").and_then(serde_json::Value::as_u64),
            Some(DEVICE_IDENTITY_RECORD_VERSION_V2)
        );
        assert!(json.get("wrapped_signing_secret_key").is_some());
        assert!(json.get("signing_secret_key").is_none());
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

        let object_dir = store.object_dir();
        let object_dir_mode = tokio::fs::metadata(&object_dir)
            .await
            .expect("object dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(object_dir_mode, 0o700, "object dir should be 0700");

        let clipboard_dir = store.clipboard_dir();
        let clipboard_dir_mode = tokio::fs::metadata(&clipboard_dir)
            .await
            .expect("clipboard dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(clipboard_dir_mode, 0o700, "clipboard dir should be 0700");

        let record_path = object_dir.join("44444444-4444-4444-8444-444444444444.json");
        let record_metadata = tokio::fs::metadata(&record_path)
            .await
            .expect("record metadata");
        assert_eq!(
            record_metadata.permissions().mode() & 0o777,
            0o600,
            "object record should be 0600"
        );
        let record_bytes = tokio::fs::read(&record_path).await.expect("record bytes");
        let record_text = String::from_utf8_lossy(&record_bytes);
        assert!(!record_text.contains("super-secret"));
        assert!(!record_text.contains("\"text\""));
        assert!(!record_text.contains("payload_ciphertext"));

        let payload_path =
            store.clipboard_payload_ciphertext_path("44444444-4444-4444-8444-444444444444");
        let payload_metadata = tokio::fs::metadata(&payload_path)
            .await
            .expect("payload metadata");
        assert_eq!(
            payload_metadata.permissions().mode() & 0o777,
            0o600,
            "payload ciphertext file should be 0600"
        );
        let payload_bytes = tokio::fs::read(&payload_path)
            .await
            .expect("payload ciphertext");
        assert!(
            !payload_bytes
                .windows(secret.text.len())
                .any(|window| window == secret.text.as_bytes())
        );
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
}
