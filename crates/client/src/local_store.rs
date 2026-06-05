//! Client-side local cache.
//!
//! Persisted clipboard/file object cache records contain only encrypted object
//! material. Decrypted display state and clipboard payload bytes live in memory
//! for the active authenticated session.

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
#[cfg(not(target_family = "wasm"))]
const DEVICE_IDENTITY_FILE: &str = "device-identity-v1.json";
#[cfg(target_family = "wasm")]
const OBJECT_INDEX_LIMIT: usize = 1_000;

#[derive(Debug, Clone)]
pub struct DeviceSigningIdentity {
    pub device_id: Option<String>,
    pub signing_secret_key: Zeroizing<[u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalSyncState {
    Present,
    PendingCreate,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalObjectRecord {
    pub id: String,
    pub kind: ObjectKind,
    pub sync_state: LocalSyncState,
    pub seen_generation: Option<u64>,
    pub event_seq: i64,
    pub created_seq: i64,
    pub created_at: Option<String>,
    pub source_device_id: Option<String>,
    pub clipboard: Option<LocalClipboardRecord>,
    pub file: Option<LocalFileRecord>,
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
struct StoredObjectRecord {
    id: String,
    kind: ObjectKind,
    sync_state: LocalSyncState,
    seen_generation: Option<u64>,
    event_seq: i64,
    created_seq: i64,
    #[serde(default)]
    encrypted: Option<EncryptedObject>,
    #[serde(default)]
    clipboard_payload_ciphertext: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
struct MemoryState {
    records: HashMap<String, LocalObjectRecord>,
    clipboard_payloads: HashMap<String, Vec<u8>>,
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
            created_seq,
            event_seq,
            Some(sync.generation),
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
            created_seq,
            created_seq,
            Some(generation),
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
            match decrypt_stored_object_record(&record, encryption_key) {
                Ok(Some((local_record, payload))) => {
                    if let Some(payload) = payload {
                        memory
                            .clipboard_payloads
                            .insert(local_record.id.clone(), payload);
                    }
                    memory.records.insert(local_record.id.clone(), local_record);
                }
                Ok(None) => {
                    if record.sync_state == LocalSyncState::Present && record.encrypted.is_none() {
                        self.remove_stored_object_record_and_payloads(&record)
                            .await?;
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        object_id = %record.id,
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

    pub async fn clipboard_text(&self, id: &str) -> Result<Option<String>, LocalStoreError> {
        let item_id = validate_item_id(id)?;
        let _sync = self.sync.lock().await;
        self.clipboard_text_inner(&item_id).await
    }

    pub async fn clipboard_payload(&self, id: &str) -> Result<Option<Vec<u8>>, LocalStoreError> {
        let item_id = validate_item_id(id)?;
        let _sync = self.sync.lock().await;
        self.clipboard_payload_inner(&item_id).await
    }

    pub async fn load_or_create_device_signing_identity(
        &self,
    ) -> Result<DeviceSigningIdentity, LocalStoreError> {
        self.load_or_create_device_signing_identity_inner().await
    }

    pub async fn persist_device_signing_identity(
        &self,
        identity: &DeviceSigningIdentity,
    ) -> Result<(), LocalStoreError> {
        if let Some(device_id) = identity.device_id.as_deref() {
            validate_device_id(device_id)?;
        }
        self.persist_device_signing_identity_inner(identity).await
    }

    async fn persist_clipboard_present_encrypted_inner(
        &self,
        item_id: &str,
        item: &DecryptedClipboardItem,
        payload: &[u8],
        encrypted: &EncryptedClipboardObject,
        created_seq: i64,
        event_seq: i64,
        seen_generation: Option<u64>,
    ) -> Result<(), LocalStoreError> {
        if let Some(record) = self.stored_object_record(item_id).await?
            && record.sync_state == LocalSyncState::Deleted
            && record.event_seq > event_seq
        {
            return Ok(());
        }

        let local_record = LocalObjectRecord {
            id: item.id.clone(),
            kind: ObjectKind::Clipboard,
            sync_state: LocalSyncState::Present,
            seen_generation,
            event_seq,
            created_seq,
            created_at: Some(item.created_at.clone()),
            source_device_id: Some(item.source_device_id.clone()),
            clipboard: Some(LocalClipboardRecord {
                text: item.text.clone(),
                mime_type: item.mime_type.clone(),
                payload_size: item.payload_size,
            }),
            file: None,
        };
        debug_assert_eq!(item_id, item.id);
        let stored_record = StoredObjectRecord {
            id: item.id.clone(),
            kind: ObjectKind::Clipboard,
            sync_state: LocalSyncState::Present,
            seen_generation,
            event_seq,
            created_seq,
            encrypted: Some(encrypted.object.clone()),
            clipboard_payload_ciphertext: Some(encrypted.payload_ciphertext.clone()),
        };
        self.write_stored_object_record(&stored_record).await?;
        self.write_memory_record(local_record, Some(payload.to_vec()))
            .await
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
        if let Some(record) = self.stored_object_record(item_id).await?
            && record.sync_state == LocalSyncState::Deleted
            && record.event_seq > event_seq
        {
            return Ok(());
        }

        let local_record = LocalObjectRecord {
            id: item.id.clone(),
            kind: ObjectKind::File,
            sync_state: LocalSyncState::Present,
            seen_generation,
            event_seq,
            created_seq,
            created_at: Some(item.created_at.clone()),
            source_device_id: Some(item.source_device_id.clone()),
            clipboard: None,
            file: Some(LocalFileRecord {
                filename: item.filename.clone(),
                mime_type: item.mime_type.clone(),
                blob_size: item.blob_size,
            }),
        };
        debug_assert_eq!(item_id, item.id);
        let stored_record = StoredObjectRecord {
            id: item.id.clone(),
            kind: ObjectKind::File,
            sync_state: LocalSyncState::Present,
            seen_generation,
            event_seq,
            created_seq,
            encrypted: Some(encrypted.clone()),
            clipboard_payload_ciphertext: None,
        };
        self.write_stored_object_record(&stored_record).await?;
        self.write_memory_record(local_record, None).await
    }

    async fn mark_pending_create_inner(
        &self,
        kind: ObjectKind,
        object_id: &str,
        created_seq: i64,
        generation: u64,
    ) -> Result<bool, LocalStoreError> {
        match self.stored_object_record(object_id).await? {
            Some(record)
                if record.sync_state == LocalSyncState::Deleted
                    && record.event_seq > created_seq =>
            {
                Ok(false)
            }
            Some(record)
                if record.sync_state == LocalSyncState::Present
                    && record.event_seq >= created_seq =>
            {
                Ok(false)
            }
            Some(mut record) if record.sync_state == LocalSyncState::Present => {
                record.event_seq = created_seq;
                record.created_seq = created_seq;
                record.seen_generation = Some(generation);
                self.write_stored_object_record(&record).await?;
                Ok(false)
            }
            Some(mut record) if record.sync_state == LocalSyncState::PendingCreate => {
                if created_seq >= record.event_seq {
                    record.event_seq = created_seq;
                    record.created_seq = created_seq;
                    record.seen_generation = Some(generation);
                    self.write_stored_object_record(&record).await?;
                }
                Ok(true)
            }
            _ => {
                let record = StoredObjectRecord {
                    id: object_id.to_string(),
                    kind,
                    sync_state: LocalSyncState::PendingCreate,
                    seen_generation: Some(generation),
                    event_seq: created_seq,
                    created_seq,
                    encrypted: None,
                    clipboard_payload_ciphertext: None,
                };
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
            && record.event_seq >= event_seq
        {
            return Ok(());
        }
        self.remove_payloads_for_object(kind, object_id).await?;
        self.remove_memory_record(object_id).await;
        let record = StoredObjectRecord {
            id: object_id.to_string(),
            kind,
            sync_state: LocalSyncState::Deleted,
            seen_generation: Some(generation),
            event_seq,
            created_seq: event_seq,
            encrypted: None,
            clipboard_payload_ciphertext: None,
        };
        self.write_stored_object_record(&record).await
    }

    async fn sweep_kind_inner(
        &self,
        kind: ObjectKind,
        generation: u64,
        stream_start_seq: i64,
    ) -> Result<(), LocalStoreError> {
        for record in self.all_stored_object_records().await? {
            if record.kind != kind {
                continue;
            }
            match record.sync_state {
                LocalSyncState::Present
                    if record.created_seq <= stream_start_seq
                        && record.seen_generation != Some(generation) =>
                {
                    self.remove_stored_object_record_and_payloads(&record)
                        .await?;
                }
                LocalSyncState::Deleted if record.seen_generation != Some(generation) => {
                    self.remove_stored_object_record_and_payloads(&record)
                        .await?;
                }
                LocalSyncState::PendingCreate
                    if record.created_seq <= stream_start_seq
                        && record.seen_generation != Some(generation) =>
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

    async fn clipboard_text_inner(&self, item_id: &str) -> Result<Option<String>, LocalStoreError> {
        match self.clipboard_payload_inner(item_id).await? {
            Some(payload) => Ok(Some(String::from_utf8(payload)?)),
            None => Ok(None),
        }
    }

    async fn clipboard_payload_inner(
        &self,
        item_id: &str,
    ) -> Result<Option<Vec<u8>>, LocalStoreError> {
        Ok(self
            .memory
            .lock()
            .await
            .clipboard_payloads
            .get(item_id)
            .cloned())
    }

    async fn write_memory_record(
        &self,
        record: LocalObjectRecord,
        clipboard_payload: Option<Vec<u8>>,
    ) -> Result<(), LocalStoreError> {
        let mut memory = self.memory.lock().await;
        if let Some(payload) = clipboard_payload {
            memory.clipboard_payloads.insert(record.id.clone(), payload);
        } else {
            memory.clipboard_payloads.remove(&record.id);
        }
        memory.records.insert(record.id.clone(), record);
        Ok(())
    }

    async fn remove_memory_record(&self, object_id: &str) {
        let mut memory = self.memory.lock().await;
        memory.records.remove(object_id);
        memory.clipboard_payloads.remove(object_id);
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
    ) -> Result<DeviceSigningIdentity, LocalStoreError> {
        if let Some(record) = self.read_device_identity_record().await? {
            match device_identity_from_record(record) {
                Ok(identity) => return Ok(identity),
                Err(error) => {
                    tracing::warn!("Replacing invalid local device identity: {}", error);
                }
            }
        }

        let identity = new_device_signing_identity(None);
        self.write_device_identity(&identity).await?;
        Ok(identity)
    }

    async fn persist_device_signing_identity_inner(
        &self,
        identity: &DeviceSigningIdentity,
    ) -> Result<(), LocalStoreError> {
        self.write_device_identity(identity).await
    }

    async fn read_device_identity_record(
        &self,
    ) -> Result<Option<DeviceIdentityRecord>, LocalStoreError> {
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
    ) -> Result<(), LocalStoreError> {
        ensure_private_dir(&self.base_dir).await?;
        let record = DeviceIdentityRecord {
            device_id: identity.device_id.clone(),
            signing_secret_key: identity.signing_secret_key.to_vec(),
        };
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
        write_private_file_atomic(&self.object_record_path(&record.id), &bytes).await
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
        self.remove_payloads_for_object(record.kind, &record.id)
            .await?;
        match tokio::fs::remove_file(self.object_record_path(&record.id)).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.remove_memory_record(&record.id).await;
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

    fn device_identity_path(&self) -> PathBuf {
        self.base_dir.join(DEVICE_IDENTITY_FILE)
    }
}

#[cfg(target_family = "wasm")]
impl LocalStore {
    async fn load_or_create_device_signing_identity_inner(
        &self,
    ) -> Result<DeviceSigningIdentity, LocalStoreError> {
        let storage = browser_storage()?;
        if let Some(json) = storage
            .get_item(&self.device_identity_key())
            .map_err(storage_error)?
        {
            match serde_json::from_str::<DeviceIdentityRecord>(&json)
                .map_err(LocalStoreError::from)
                .and_then(device_identity_from_record)
            {
                Ok(identity) => return Ok(identity),
                Err(error) => {
                    tracing::warn!("Replacing invalid local device identity: {}", error);
                }
            }
        }

        let identity = new_device_signing_identity(None);
        self.write_browser_device_identity(&storage, &identity)?;
        Ok(identity)
    }

    async fn persist_device_signing_identity_inner(
        &self,
        identity: &DeviceSigningIdentity,
    ) -> Result<(), LocalStoreError> {
        let storage = browser_storage()?;
        self.write_browser_device_identity(&storage, identity)
    }

    fn write_browser_device_identity(
        &self,
        storage: &web_sys::Storage,
        identity: &DeviceSigningIdentity,
    ) -> Result<(), LocalStoreError> {
        let record = DeviceIdentityRecord {
            device_id: identity.device_id.clone(),
            signing_secret_key: identity.signing_secret_key.to_vec(),
        };
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
            .set_item(&self.object_record_key(&record.id), &json)
            .map_err(storage_error)?;
        let mut index = self.read_object_index(&storage)?;
        index.retain(|id| id != &record.id);
        index.push(record.id.clone());
        index.truncate(OBJECT_INDEX_LIMIT);
        let index_json = serde_json::to_string(&index)?;
        storage
            .set_item(&self.object_index_key(), &index_json)
            .map_err(storage_error)?;
        Ok(())
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
        self.remove_payloads_for_object(record.kind, &record.id)
            .await?;
        storage
            .remove_item(&self.object_record_key(&record.id))
            .map_err(storage_error)?;
        let mut index = self.read_object_index(&storage)?;
        index.retain(|id| id != &record.id);
        let index_json = serde_json::to_string(&index)?;
        storage
            .set_item(&self.object_index_key(), &index_json)
            .map_err(storage_error)?;
        self.remove_memory_record(&record.id).await;
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
struct DeviceIdentityRecord {
    #[serde(default)]
    device_id: Option<String>,
    signing_secret_key: Vec<u8>,
}

fn decrypt_stored_object_record(
    record: &StoredObjectRecord,
    encryption_key: &[u8; 32],
) -> Result<Option<(LocalObjectRecord, Option<Vec<u8>>)>, LocalStoreError> {
    if record.sync_state != LocalSyncState::Present {
        return Ok(None);
    }
    let Some(encrypted) = record.encrypted.as_ref() else {
        return Ok(None);
    };

    match record.kind {
        ObjectKind::Clipboard => {
            decrypt_clipboard_record(record, encrypted, encryption_key).map(Some)
        }
        ObjectKind::File => decrypt_file_record(record, encrypted, encryption_key).map(Some),
    }
}

fn decrypt_clipboard_record(
    record: &StoredObjectRecord,
    encrypted: &EncryptedObject,
    encryption_key: &[u8; 32],
) -> Result<(LocalObjectRecord, Option<Vec<u8>>), LocalStoreError> {
    let payload = single_payload(encrypted)?;
    let ciphertext = record
        .clipboard_payload_ciphertext
        .as_deref()
        .ok_or_else(|| LocalStoreError::EncryptedCache("missing clipboard payload".into()))?;
    verify_payload_ciphertext(payload, ciphertext)?;
    let meta = decrypt_clipboard_meta(
        &encrypted.meta_nonce,
        &encrypted.meta_ciphertext,
        encryption_key,
        &encrypted.envelope.body,
    )
    .map_err(|error| LocalStoreError::EncryptedCache(error.to_string()))?;
    let plaintext = decrypt_clipboard_payload(
        &payload.nonce,
        ciphertext,
        encryption_key,
        &encrypted.envelope.body,
        payload.id,
    )
    .map_err(|error| LocalStoreError::EncryptedCache(error.to_string()))?;
    let payload_size = meta.size.unwrap_or(plaintext.len() as i64);
    let local_record = LocalObjectRecord {
        id: record.id.clone(),
        kind: ObjectKind::Clipboard,
        sync_state: LocalSyncState::Present,
        seen_generation: record.seen_generation,
        event_seq: record.event_seq,
        created_seq: record.created_seq,
        created_at: Some(encrypted.created_at.clone()),
        source_device_id: Some(encrypted.source_device_id.clone()),
        clipboard: Some(LocalClipboardRecord {
            text: clipboard_display_text(&meta.mime_type, &plaintext),
            mime_type: meta.mime_type,
            payload_size,
        }),
        file: None,
    };
    Ok((local_record, Some(plaintext)))
}

fn decrypt_file_record(
    record: &StoredObjectRecord,
    encrypted: &EncryptedObject,
    encryption_key: &[u8; 32],
) -> Result<(LocalObjectRecord, Option<Vec<u8>>), LocalStoreError> {
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
        kind: ObjectKind::File,
        sync_state: LocalSyncState::Present,
        seen_generation: record.seen_generation,
        event_seq: record.event_seq,
        created_seq: record.created_seq,
        created_at: Some(encrypted.created_at.clone()),
        source_device_id: Some(encrypted.source_device_id.clone()),
        clipboard: None,
        file: Some(LocalFileRecord {
            filename: meta.filename,
            mime_type: meta.mime_type,
            blob_size,
        }),
    };
    Ok((local_record, None))
}

fn sort_records_desc(records: &mut [LocalObjectRecord]) {
    records.sort_by(|a, b| {
        b.created_seq
            .cmp(&a.created_seq)
            .then_with(|| b.id.cmp(&a.id))
    });
}

fn clipboard_item_from_record(record: &LocalObjectRecord) -> Option<DecryptedClipboardItem> {
    if record.kind != ObjectKind::Clipboard || record.sync_state != LocalSyncState::Present {
        return None;
    }
    let clipboard = record.clipboard.as_ref()?;
    Some(DecryptedClipboardItem {
        id: record.id.clone(),
        text: clipboard.text.clone(),
        mime_type: clipboard.mime_type.clone(),
        payload_size: clipboard.payload_size,
        created_at: record.created_at.clone()?,
        source_device_id: record.source_device_id.clone()?,
    })
}

fn file_item_from_record(record: &LocalObjectRecord) -> Option<DecryptedFileItem> {
    if record.kind != ObjectKind::File || record.sync_state != LocalSyncState::Present {
        return None;
    }
    let file = record.file.as_ref()?;
    Some(DecryptedFileItem {
        id: record.id.clone(),
        filename: file.filename.clone(),
        mime_type: file.mime_type.clone(),
        blob_size: file.blob_size,
        created_at: record.created_at.clone()?,
        source_device_id: record.source_device_id.clone()?,
    })
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
        String::from_utf8_lossy(data).to_string()
    } else {
        format!("{mime_type} clipboard payload ({} bytes)", data.len())
    }
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

fn device_identity_from_record(
    record: DeviceIdentityRecord,
) -> Result<DeviceSigningIdentity, LocalStoreError> {
    let device_id = record
        .device_id
        .map(|id| validate_device_id(&id))
        .transpose()?;
    let signing_secret_key: [u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES] = record
        .signing_secret_key
        .try_into()
        .map_err(|_| LocalStoreError::InvalidDeviceSigningKey)?;
    Ok(DeviceSigningIdentity {
        device_id,
        signing_secret_key: Zeroizing::new(signing_secret_key),
    })
}

#[cfg(not(target_family = "wasm"))]
async fn ensure_private_dir(path: &Path) -> Result<(), LocalStoreError> {
    tokio::fs::create_dir_all(path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
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

        let text = store
            .clipboard_text("22222222-2222-4222-8222-222222222222")
            .await
            .expect("text");
        assert_eq!(text.as_deref(), Some("newer"));

        let restored_store = LocalStore::new(tmp.path());
        restored_store.set_profile("profile-a".into());
        let restored = restored_store
            .hydrate_ciphertext_cache(&TEST_KEY, 10)
            .await
            .expect("hydrate");
        assert_eq!(restored.clipboard_items.len(), 2);
        assert_eq!(restored.clipboard_items[0].text, "newer");

        let payload = restored_store
            .clipboard_payload("22222222-2222-4222-8222-222222222222")
            .await
            .expect("payload")
            .expect("payload bytes");
        assert_eq!(payload, b"newer");
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
            .clipboard_payload("33333333-3333-4333-8333-333333333333")
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
            .clipboard_payload("55555555-5555-4555-8555-555555555555")
            .await
            .expect("payload lookup");
        assert!(payload.is_none());
    }
}
