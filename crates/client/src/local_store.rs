//! Client-side local plaintext cache.
//!
//! The network boundary remains encrypted. This store is for local convenience
//! and durable UI state, so clipboard payloads are stored as plaintext.

#[cfg(not(target_family = "wasm"))]
use std::path::Path;
use std::{path::PathBuf, sync::RwLock};

#[cfg(target_family = "wasm")]
use base64::Engine;
use clipper_app_types::DecryptedClipboardItem;
use clipper_core::crypto;
use serde::{Deserialize, Serialize};
#[cfg(not(target_family = "wasm"))]
use tokio::io::AsyncWriteExt;
use zeroize::Zeroizing;

#[cfg(target_family = "wasm")]
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const DEFAULT_PROFILE: &str = "default";
const DEVICE_IDENTITY_FILE: &str = "device-identity-v1.json";
#[cfg(target_family = "wasm")]
const BROWSER_INDEX_LIMIT: usize = 1_000;

#[derive(Debug, Clone)]
pub struct DeviceSigningIdentity {
    pub device_id: Option<String>,
    pub signing_secret_key: Zeroizing<[u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]>,
}

#[derive(Debug)]
pub struct LocalStore {
    base_dir: PathBuf,
    profile_id: RwLock<Option<String>>,
}

impl LocalStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            profile_id: RwLock::new(None),
        }
    }

    pub fn set_profile(&self, profile_id: String) {
        let mut current = self
            .profile_id
            .write()
            .expect("local store profile lock poisoned");
        *current = Some(profile_id);
    }

    pub async fn persist_clipboard_payload_item(
        &self,
        item: &DecryptedClipboardItem,
        payload: &[u8],
    ) -> Result<(), LocalStoreError> {
        let item_id = validate_item_id(&item.id)?;
        self.persist_clipboard_payload_item_inner(&item_id, item, payload)
            .await
    }

    pub async fn recent_clipboard_items(
        &self,
        limit: usize,
    ) -> Result<Vec<DecryptedClipboardItem>, LocalStoreError> {
        self.recent_clipboard_items_inner(limit).await
    }

    pub async fn clipboard_text(&self, id: &str) -> Result<Option<String>, LocalStoreError> {
        let item_id = validate_item_id(id)?;
        self.clipboard_text_inner(&item_id).await
    }

    pub async fn clipboard_payload(&self, id: &str) -> Result<Option<Vec<u8>>, LocalStoreError> {
        let item_id = validate_item_id(id)?;
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
    async fn persist_clipboard_payload_item_inner(
        &self,
        item_id: &str,
        item: &DecryptedClipboardItem,
        payload: &[u8],
    ) -> Result<(), LocalStoreError> {
        let dir = self.clipboard_dir();
        tokio::fs::create_dir_all(&dir).await?;

        let payload_path = dir.join(format!("{item_id}.payload"));
        let meta_path = dir.join(format!("{item_id}.json"));

        write_file_atomic(&payload_path, payload).await?;

        let metadata = ClipboardMetadata {
            id: item.id.clone(),
            mime_type: item.mime_type.clone(),
            payload_size: item.payload_size,
            created_at: item.created_at.clone(),
            source_device_id: item.source_device_id.clone(),
        };
        let metadata_json = serde_json::to_vec_pretty(&metadata)?;
        write_file_atomic(&meta_path, &metadata_json).await?;

        Ok(())
    }

    async fn recent_clipboard_items_inner(
        &self,
        limit: usize,
    ) -> Result<Vec<DecryptedClipboardItem>, LocalStoreError> {
        let dir = self.clipboard_dir();
        let mut read_dir = match tokio::fs::read_dir(&dir).await {
            Ok(read_dir) => read_dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };

        let mut items = Vec::new();
        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            match self.read_clipboard_item_from_metadata_path(&path).await {
                Ok(Some(item)) => items.push(item),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(path = %path.display(), "Failed to read local clipboard item: {}", e)
                }
            }
        }

        sort_and_truncate(&mut items, limit);
        Ok(items)
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
        let payload_path = self.clipboard_dir().join(format!("{item_id}.payload"));
        match tokio::fs::read(&payload_path).await {
            Ok(payload) => Ok(Some(payload)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let legacy_text_path = self.clipboard_dir().join(format!("{item_id}.txt"));
                match tokio::fs::read(legacy_text_path).await {
                    Ok(payload) => Ok(Some(payload)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
            Err(e) => Err(e.into()),
        }
    }

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

    fn profile_root(&self) -> PathBuf {
        self.base_dir.join(self.profile_id())
    }

    fn clipboard_dir(&self) -> PathBuf {
        self.profile_root().join("clipboard")
    }

    fn device_identity_path(&self) -> PathBuf {
        self.base_dir.join(DEVICE_IDENTITY_FILE)
    }

    async fn read_clipboard_item_from_metadata_path(
        &self,
        metadata_path: &Path,
    ) -> Result<Option<DecryptedClipboardItem>, LocalStoreError> {
        let metadata_bytes = tokio::fs::read(metadata_path).await?;
        let metadata: ClipboardMetadata = serde_json::from_slice(&metadata_bytes)?;
        let item_id = validate_item_id(&metadata.id)?;
        let payload = match self.clipboard_payload_inner(&item_id).await? {
            Some(payload) => payload,
            None => return Ok(None),
        };

        Ok(Some(DecryptedClipboardItem {
            id: metadata.id,
            text: clipboard_display_text(&metadata.mime_type, &payload),
            mime_type: metadata.mime_type,
            payload_size: metadata.payload_size,
            created_at: metadata.created_at,
            source_device_id: metadata.source_device_id,
        }))
    }
}

#[cfg(target_family = "wasm")]
impl LocalStore {
    async fn persist_clipboard_payload_item_inner(
        &self,
        item_id: &str,
        item: &DecryptedClipboardItem,
        payload: &[u8],
    ) -> Result<(), LocalStoreError> {
        let storage = browser_storage()?;
        let record = BrowserClipboardRecord {
            item: item.clone(),
            payload_b64: B64.encode(payload),
        };
        let item_json = serde_json::to_string(&record)?;
        storage
            .set_item(&self.clipboard_item_key(item_id), &item_json)
            .map_err(storage_error)?;
        self.prepend_clipboard_index(&storage, item_id)?;
        Ok(())
    }

    async fn recent_clipboard_items_inner(
        &self,
        limit: usize,
    ) -> Result<Vec<DecryptedClipboardItem>, LocalStoreError> {
        let storage = browser_storage()?;
        let mut items = Vec::new();

        for item_id in self.read_clipboard_index(&storage)? {
            let item_json = storage
                .get_item(&self.clipboard_item_key(&item_id))
                .map_err(storage_error)?;
            let Some(item_json) = item_json else {
                continue;
            };

            match parse_browser_clipboard_record(&item_json) {
                Ok(item) => items.push(item),
                Err(e) => {
                    tracing::warn!(item_id = %item_id, "Failed to read local clipboard item: {}", e)
                }
            }
        }

        sort_and_truncate(&mut items, limit);
        Ok(items)
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
        let storage = browser_storage()?;
        let item_json = storage
            .get_item(&self.clipboard_item_key(item_id))
            .map_err(storage_error)?;
        let Some(item_json) = item_json else {
            return Ok(None);
        };

        parse_browser_clipboard_payload(&item_json).map(Some)
    }

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

    fn prepend_clipboard_index(
        &self,
        storage: &web_sys::Storage,
        item_id: &str,
    ) -> Result<(), LocalStoreError> {
        let mut index = self.read_clipboard_index(storage)?;
        index.retain(|id| id != item_id);
        index.insert(0, item_id.to_string());
        index.truncate(BROWSER_INDEX_LIMIT);

        let index_json = serde_json::to_string(&index)?;
        storage
            .set_item(&self.clipboard_index_key(), &index_json)
            .map_err(storage_error)?;
        Ok(())
    }

    fn read_clipboard_index(
        &self,
        storage: &web_sys::Storage,
    ) -> Result<Vec<String>, LocalStoreError> {
        let index_json = storage
            .get_item(&self.clipboard_index_key())
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

    fn clipboard_index_key(&self) -> String {
        format!("{}.clipboard.index", self.storage_prefix())
    }

    fn clipboard_item_key(&self, item_id: &str) -> String {
        format!("{}.clipboard.{item_id}", self.storage_prefix())
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

#[cfg(not(target_family = "wasm"))]
#[derive(Debug, Serialize, Deserialize)]
struct ClipboardMetadata {
    id: String,
    #[serde(default = "default_clipboard_mime_type")]
    mime_type: String,
    #[serde(default)]
    payload_size: i64,
    created_at: String,
    source_device_id: String,
}

#[cfg(target_family = "wasm")]
#[derive(Debug, Serialize, Deserialize)]
struct BrowserClipboardRecord {
    item: DecryptedClipboardItem,
    payload_b64: String,
}

#[cfg(not(target_family = "wasm"))]
fn default_clipboard_mime_type() -> String {
    "text/plain".into()
}

#[cfg(target_family = "wasm")]
fn parse_browser_clipboard_record(json: &str) -> Result<DecryptedClipboardItem, LocalStoreError> {
    match serde_json::from_str::<BrowserClipboardRecord>(json) {
        Ok(record) => Ok(record.item),
        Err(_) => Ok(serde_json::from_str::<DecryptedClipboardItem>(json)?),
    }
}

#[cfg(target_family = "wasm")]
fn parse_browser_clipboard_payload(json: &str) -> Result<Vec<u8>, LocalStoreError> {
    match serde_json::from_str::<BrowserClipboardRecord>(json) {
        Ok(record) => B64
            .decode(record.payload_b64)
            .map_err(|e| LocalStoreError::PayloadDecode(e.to_string())),
        Err(_) => {
            let item: DecryptedClipboardItem = serde_json::from_str(json)?;
            Ok(item.text.into_bytes())
        }
    }
}

fn sort_and_truncate(items: &mut Vec<DecryptedClipboardItem>, limit: usize) {
    items.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    items.truncate(limit);
}

#[cfg(not(target_family = "wasm"))]
fn clipboard_display_text(mime_type: &str, payload: &[u8]) -> String {
    if is_text_mime_type(mime_type) {
        String::from_utf8_lossy(payload).into_owned()
    } else {
        format!("{mime_type} clipboard payload ({} bytes)", payload.len())
    }
}

#[cfg(not(target_family = "wasm"))]
fn is_text_mime_type(mime_type: &str) -> bool {
    mime_type
        .split(';')
        .next()
        .unwrap_or(mime_type)
        .trim()
        .to_ascii_lowercase()
        .starts_with("text/")
}

#[cfg(not(target_family = "wasm"))]
async fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<(), LocalStoreError> {
    let tmp_path = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("file")
    ));
    tokio::fs::write(&tmp_path, bytes).await?;
    tokio::fs::rename(&tmp_path, path).await?;
    Ok(())
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
    #[error("browser local storage error: {0}")]
    BrowserStorage(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, text: &str, created_at: &str) -> DecryptedClipboardItem {
        DecryptedClipboardItem {
            id: id.into(),
            text: text.into(),
            mime_type: "text/plain".into(),
            payload_size: text.len() as i64,
            created_at: created_at.into(),
            source_device_id: "device-a".into(),
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
            .persist_clipboard_payload_item(&older, older.text.as_bytes())
            .await
            .expect("older");
        store
            .persist_clipboard_payload_item(&newer, newer.text.as_bytes())
            .await
            .expect("newer");

        let items = store.recent_clipboard_items(10).await.expect("recent");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "newer");
        assert_eq!(items[1].text, "older");

        let text = store
            .clipboard_text("22222222-2222-4222-8222-222222222222")
            .await
            .expect("text");
        assert_eq!(text.as_deref(), Some("newer"));

        let payload = store
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
            source_device_id: "device-a".into(),
        };

        store
            .persist_clipboard_payload_item(&image, &[0, 1, 2, 3])
            .await
            .expect("image");

        let items = store.recent_clipboard_items(10).await.expect("recent");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "image/png clipboard payload (4 bytes)");

        let payload = store
            .clipboard_payload("33333333-3333-4333-8333-333333333333")
            .await
            .expect("payload")
            .expect("payload bytes");
        assert_eq!(payload, vec![0, 1, 2, 3]);
    }

    #[tokio::test]
    async fn rejects_path_like_ids() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        let bad = item("../escape", "bad", "2026-01-01T00:00:00+00:00");
        assert!(
            store
                .persist_clipboard_payload_item(&bad, bad.text.as_bytes())
                .await
                .is_err()
        );
    }
}
