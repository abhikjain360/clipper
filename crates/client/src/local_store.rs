//! Client-side local plaintext cache.
//!
//! The network boundary remains encrypted. This store is for local convenience
//! and durable UI state, so clipboard text is stored as plaintext.

#[cfg(not(target_family = "wasm"))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::RwLock;

use clipper_app_types::DecryptedClipboardItem;
#[cfg(not(target_family = "wasm"))]
use serde::{Deserialize, Serialize};

const DEFAULT_PROFILE: &str = "default";
#[cfg(target_family = "wasm")]
const BROWSER_INDEX_LIMIT: usize = 1_000;

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

    pub async fn persist_clipboard_item(
        &self,
        item: &DecryptedClipboardItem,
    ) -> Result<(), LocalStoreError> {
        let item_id = validate_item_id(&item.id)?;
        self.persist_clipboard_item_inner(&item_id, item).await
    }

    pub async fn persist_clipboard_items(
        &self,
        items: &[DecryptedClipboardItem],
    ) -> Result<(), LocalStoreError> {
        for item in items {
            self.persist_clipboard_item(item).await?;
        }
        Ok(())
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
    async fn persist_clipboard_item_inner(
        &self,
        item_id: &str,
        item: &DecryptedClipboardItem,
    ) -> Result<(), LocalStoreError> {
        let dir = self.clipboard_dir();
        tokio::fs::create_dir_all(&dir).await?;

        let text_path = dir.join(format!("{item_id}.txt"));
        let meta_path = dir.join(format!("{item_id}.json"));

        write_file_atomic(&text_path, item.text.as_bytes()).await?;

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
        let path = self.clipboard_dir().join(format!("{item_id}.txt"));
        match tokio::fs::read_to_string(path).await {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn profile_root(&self) -> PathBuf {
        self.base_dir.join(self.profile_id())
    }

    fn clipboard_dir(&self) -> PathBuf {
        self.profile_root().join("clipboard")
    }

    async fn read_clipboard_item_from_metadata_path(
        &self,
        metadata_path: &Path,
    ) -> Result<Option<DecryptedClipboardItem>, LocalStoreError> {
        let metadata_bytes = tokio::fs::read(metadata_path).await?;
        let metadata: ClipboardMetadata = serde_json::from_slice(&metadata_bytes)?;
        let item_id = validate_item_id(&metadata.id)?;
        let text_path = metadata_path.with_file_name(format!("{item_id}.txt"));
        let text = match tokio::fs::read_to_string(text_path).await {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        Ok(Some(DecryptedClipboardItem {
            id: metadata.id,
            text,
            mime_type: metadata.mime_type,
            payload_size: metadata.payload_size,
            created_at: metadata.created_at,
            source_device_id: metadata.source_device_id,
        }))
    }
}

#[cfg(target_family = "wasm")]
impl LocalStore {
    async fn persist_clipboard_item_inner(
        &self,
        item_id: &str,
        item: &DecryptedClipboardItem,
    ) -> Result<(), LocalStoreError> {
        let storage = browser_storage()?;
        let item_json = serde_json::to_string(item)?;
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

            match serde_json::from_str::<DecryptedClipboardItem>(&item_json) {
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
        let storage = browser_storage()?;
        let item_json = storage
            .get_item(&self.clipboard_item_key(item_id))
            .map_err(storage_error)?;
        let Some(item_json) = item_json else {
            return Ok(None);
        };
        let item: DecryptedClipboardItem = serde_json::from_str(&item_json)?;
        Ok(Some(item.text))
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

#[cfg(not(target_family = "wasm"))]
fn default_clipboard_mime_type() -> String {
    "text/plain".into()
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
    #[error("invalid local clipboard item id: {0}")]
    InvalidId(String),
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

        store.persist_clipboard_item(&older).await.expect("older");
        store.persist_clipboard_item(&newer).await.expect("newer");

        let items = store.recent_clipboard_items(10).await.expect("recent");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "newer");
        assert_eq!(items[1].text, "older");

        let text = store
            .clipboard_text("22222222-2222-4222-8222-222222222222")
            .await
            .expect("text");
        assert_eq!(text.as_deref(), Some("newer"));
    }

    #[tokio::test]
    async fn rejects_path_like_ids() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalStore::new(tmp.path());
        let bad = item("../escape", "bad", "2026-01-01T00:00:00+00:00");
        assert!(store.persist_clipboard_item(&bad).await.is_err());
    }
}
