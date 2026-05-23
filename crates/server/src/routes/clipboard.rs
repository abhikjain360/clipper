use axum::{
    Json,
    extract::{Extension, Query, State},
    http::StatusCode,
};
use base64::Engine;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, Set};
use tokio::io::AsyncWriteExt;
use tracing::info;

use crate::auth::AuthInfo;
use crate::entity::{clipboard_item, event_log};
use crate::routes::{error_response, validate_client_id};
use crate::state::AppState;
use crate::ws::WsBroadcast;
use clipper_core::crypto::sha256;
use clipper_core::models::{
    ClipboardItem, ClipboardListResponse, ClipboardUploadRequest, ErrorResponse, OkResponse,
};

pub async fn upload(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Json(req): Json<ClipboardUploadRequest>,
) -> Result<Json<OkResponse>, (StatusCode, Json<ErrorResponse>)> {
    let b64 = &base64::engine::general_purpose::STANDARD;

    validate_client_id(&req.id)?;

    let ciphertext = b64
        .decode(&req.ciphertext_b64)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid ciphertext_b64"))?;

    let nonce = b64
        .decode(&req.nonce_b64)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid nonce_b64"))?;

    let provided_hash = b64
        .decode(&req.ciphertext_sha256_b64)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid ciphertext_sha256_b64"))?;

    // Verify hash
    let computed_hash = sha256(&ciphertext);
    if computed_hash.as_slice() != provided_hash.as_slice() {
        return Err(error_response(StatusCode::BAD_REQUEST, "SHA-256 mismatch"));
    }

    if clipboard_item::Entity::find_by_id(&req.id)
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .is_some()
    {
        return Err(error_response(
            StatusCode::CONFLICT,
            "Clipboard item already exists",
        ));
    }

    // Write ciphertext to disk
    let clip_dir = state.clipboard_dir();
    tokio::fs::create_dir_all(&clip_dir)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Storage error"))?;

    let filename = format!("{}.bin", req.id);
    let path = clip_dir.join(&filename);
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                error_response(StatusCode::CONFLICT, "Clipboard item already exists")
            } else {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Storage error")
            }
        })?;
    file.write_all(&ciphertext)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Storage error"))?;
    file.flush()
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Storage error"))?;
    drop(file);

    let now = Utc::now().to_rfc3339();
    let expires = (Utc::now() + Duration::days(7)).to_rfc3339();

    let item = clipboard_item::ActiveModel {
        id: Set(req.id.clone()),
        ciphertext_path: Set(filename),
        nonce: Set(nonce),
        ciphertext_size: Set(ciphertext.len() as i64),
        sha256_ciphertext: Set(computed_hash.to_vec()),
        created_at: Set(now.clone()),
        expires_at: Set(expires),
        source_device_id: Set(auth.device_id.clone()),
    };
    if item.insert(state.db()).await.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database error",
        ));
    }

    // Log event
    let event = event_log::ActiveModel {
        seq: Default::default(),
        event_type: Set("clipboard.created".into()),
        object_kind: Set("clipboard".into()),
        object_id: Set(req.id.clone()),
        created_at: Set(now.clone()),
    };
    let inserted = event
        .insert(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    // Broadcast to WebSocket clients
    let _ = state.ws_tx().send(WsBroadcast {
        seq: inserted.seq,
        event_type: "clipboard.created".into(),
        object_kind: "clipboard".into(),
        object_id: req.id,
        created_at: now,
    });

    info!(device_id = %auth.device_id, "Clipboard item uploaded");

    Ok(Json(OkResponse { ok: true }))
}

#[derive(Debug, serde::Deserialize)]
pub struct ListQuery {
    pub limit: Option<u64>,
    pub before: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ClipboardListResponse>, StatusCode> {
    let b64 = &base64::engine::general_purpose::STANDARD;
    let limit = query.limit.unwrap_or(100).min(500);

    let mut q =
        clipboard_item::Entity::find().order_by(clipboard_item::Column::CreatedAt, Order::Desc);

    if let Some(before) = &query.before {
        q = q.filter(clipboard_item::Column::CreatedAt.lt(before.clone()));
    }

    let items: Vec<clipboard_item::Model> = q
        .all(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Take limit + 1 to determine if there are more
    let has_more = items.len() as u64 > limit;
    let items: Vec<clipboard_item::Model> = items.into_iter().take(limit as usize).collect();

    let mut result_items = Vec::new();
    for item in &items {
        // Read ciphertext from disk
        let path = state.clipboard_dir().join(&item.ciphertext_path);
        let ciphertext = tokio::fs::read(&path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        result_items.push(ClipboardItem {
            id: item.id.clone(),
            nonce_b64: b64.encode(&item.nonce),
            ciphertext_b64: b64.encode(&ciphertext),
            created_at: item.created_at.clone(),
            source_device_id: item.source_device_id.clone(),
        });
    }

    let next_before = if has_more {
        items.last().map(|i| i.created_at.clone())
    } else {
        None
    };

    Ok(Json(ClipboardListResponse {
        items: result_items,
        next_before,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use sea_orm::{Database, EntityTrait};
    use sea_orm_migration::MigratorTrait;
    use tempfile::TempDir;
    use uuid::Uuid;

    use crate::migration;

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    async fn test_state() -> (AppState, TempDir) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        migration::Migrator::up(&db, None).await.expect("migrate");
        (AppState::new(db, data_dir.path().to_path_buf()), data_dir)
    }

    fn auth(device_id: &str) -> AuthInfo {
        AuthInfo {
            session_id: "session-id".into(),
            device_id: device_id.into(),
        }
    }

    fn upload_request(id: String, source_device_id: &str) -> ClipboardUploadRequest {
        upload_request_with_ciphertext(id, source_device_id, b"encrypted clipboard")
    }

    fn upload_request_with_ciphertext(
        id: String,
        source_device_id: &str,
        ciphertext: &[u8],
    ) -> ClipboardUploadRequest {
        ClipboardUploadRequest {
            id,
            nonce_b64: B64.encode([1_u8; 12]),
            ciphertext_sha256_b64: B64.encode(sha256(ciphertext)),
            ciphertext_b64: B64.encode(ciphertext),
            source_device_id: source_device_id.into(),
            client_created_at: None,
        }
    }

    // This sends a path-like clipboard ID before any blob write happens. We
    // test it because object IDs become filenames, so accepting non-UUID IDs
    // would reopen path traversal bugs in the upload path.
    #[tokio::test]
    async fn upload_rejects_non_uuid_id_before_writing() {
        let (state, data_dir) = test_state().await;

        let result = upload(
            State(state),
            Extension(auth("device-a")),
            Json(upload_request("../escape".into(), "device-a")),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert!(!data_dir.path().join("escape.bin").exists());
    }

    // This sends a spoofed source_device_id in the body while authenticated as a
    // different device. We test it because provenance must come from the bearer
    // token, not from client-controlled JSON.
    #[tokio::test]
    async fn upload_uses_authenticated_device_as_source() {
        let (state, _data_dir) = test_state().await;
        let id = Uuid::new_v4().to_string();

        let _ = upload(
            State(state.clone()),
            Extension(auth("device-auth")),
            Json(upload_request(id.clone(), "device-spoof")),
        )
        .await
        .expect("upload");

        let item = clipboard_item::Entity::find_by_id(id)
            .one(state.db())
            .await
            .expect("query")
            .expect("item");
        assert_eq!(item.source_device_id, "device-auth");
    }

    // This uploads two clipboard blobs with the same client ID. We test it
    // because rejecting the duplicate in the database is not enough if the
    // second write already replaced the ciphertext on disk.
    #[tokio::test]
    async fn upload_rejects_duplicate_id_without_overwriting_blob() {
        let (state, data_dir) = test_state().await;
        let id = Uuid::new_v4().to_string();

        let _ = upload(
            State(state.clone()),
            Extension(auth("device-a")),
            Json(upload_request_with_ciphertext(
                id.clone(),
                "device-a",
                b"first encrypted clipboard",
            )),
        )
        .await
        .expect("first upload");

        let result = upload(
            State(state),
            Extension(auth("device-a")),
            Json(upload_request_with_ciphertext(
                id.clone(),
                "device-a",
                b"second encrypted clipboard",
            )),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
        let blob = tokio::fs::read(data_dir.path().join("clipboard").join(format!("{id}.bin")))
            .await
            .expect("blob");
        assert_eq!(blob, b"first encrypted clipboard");
    }
}
