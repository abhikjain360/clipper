use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
};
use base64::Engine;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, Set};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;
use tracing::info;

use crate::auth::AuthInfo;
use crate::entity::{event_log, file};
use crate::routes::{error_response, validate_client_id};
use crate::state::AppState;
use crate::ws::WsBroadcast;
use clipper_core::models::{
    ErrorResponse, FileCompleteRequest, FileInitRequest, FileInitResponse, FileListItem,
    FileListResponse, OkResponse,
};

const MAX_FILE_BLOB_BYTES: u64 = 512 * 1024 * 1024;

pub async fn init_upload(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Json(req): Json<FileInitRequest>,
) -> Result<Json<FileInitResponse>, (StatusCode, Json<ErrorResponse>)> {
    let b64 = &base64::engine::general_purpose::STANDARD;
    let now = Utc::now().to_rfc3339();

    validate_client_id(&req.id)?;
    let blob_size = validate_blob_size(req.blob_size)?;

    if file::Entity::find_by_id(&req.id)
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .is_some()
    {
        return Err(error_response(StatusCode::CONFLICT, "File already exists"));
    }

    let meta_ciphertext = b64
        .decode(&req.meta_ciphertext_b64)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid meta_ciphertext_b64"))?;
    let meta_nonce = b64
        .decode(&req.meta_nonce_b64)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid meta_nonce_b64"))?;
    let blob_nonce = b64
        .decode(&req.blob_nonce_b64)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid blob_nonce_b64"))?;

    let files_dir = state.files_dir();
    tokio::fs::create_dir_all(&files_dir)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Storage error"))?;

    let blob_filename = format!("{}.bin", req.id);

    let new_file = file::ActiveModel {
        id: Set(req.id.clone()),
        blob_path: Set(blob_filename),
        meta_ciphertext: Set(meta_ciphertext),
        meta_nonce: Set(meta_nonce),
        blob_nonce: Set(blob_nonce),
        blob_size: Set(blob_size as i64),
        sha256_ciphertext: Set(vec![]), // filled on complete
        created_at: Set(now.clone()),
        updated_at: Set(now),
        source_device_id: Set(auth.device_id.clone()),
        status: Set("pending".into()),
    };
    new_file
        .insert(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    info!(device_id = %auth.device_id, file_id = %req.id, "File upload initiated");

    Ok(Json(FileInitResponse {
        upload_url: format!("/api/files/{}/blob", req.id),
    }))
}

pub async fn upload_blob(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path(file_id): Path<String>,
    body: Body,
) -> Result<Json<OkResponse>, (StatusCode, Json<ErrorResponse>)> {
    validate_client_id(&file_id)?;

    // Verify file exists and is pending
    let existing = file::Entity::find_by_id(&file_id)
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "File not found"))?;

    if existing.status != "pending" {
        return Err(error_response(
            StatusCode::CONFLICT,
            "File already uploaded",
        ));
    }

    if existing.source_device_id != auth.device_id {
        return Err(error_response(StatusCode::FORBIDDEN, "Forbidden"));
    }

    let expected_size = validate_blob_size(existing.blob_size)?;

    // Stream body to disk
    let path = state.files_dir().join(&existing.blob_path);
    let mut out_file = tokio::fs::File::create(&path)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Storage error"))?;

    use futures_util::StreamExt;
    let mut stream = body.into_data_stream();
    let mut total_size = 0_u64;
    while let Some(chunk) = stream.next().await {
        let data = chunk.map_err(|_| error_response(StatusCode::BAD_REQUEST, "Stream error"))?;
        total_size += data.len() as u64;
        if total_size > expected_size {
            drop(out_file);
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Blob size does not match initialized size",
            ));
        }
        out_file
            .write_all(&data)
            .await
            .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Write error"))?;
    }
    out_file
        .flush()
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Flush error"))?;
    drop(out_file);

    if total_size != expected_size {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Blob size does not match initialized size",
        ));
    }

    info!(device_id = %auth.device_id, file_id = %file_id, "File blob uploaded");

    Ok(Json(OkResponse { ok: true }))
}

pub async fn complete_upload(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path(file_id): Path<String>,
    Json(req): Json<FileCompleteRequest>,
) -> Result<Json<OkResponse>, (StatusCode, Json<ErrorResponse>)> {
    let b64 = &base64::engine::general_purpose::STANDARD;

    validate_client_id(&file_id)?;

    let existing = file::Entity::find_by_id(&file_id)
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "File not found"))?;

    if existing.status != "pending" {
        return Err(error_response(
            StatusCode::CONFLICT,
            "File already completed",
        ));
    }

    if existing.source_device_id != auth.device_id {
        return Err(error_response(StatusCode::FORBIDDEN, "Forbidden"));
    }

    let expected_size = validate_blob_size(existing.blob_size)?;

    // Verify the blob exists and compute hash
    let blob_path = state.files_dir().join(&existing.blob_path);
    let (computed_hash, actual_size) = sha256_file(&blob_path)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Blob not found"))?;

    let provided_hash = b64
        .decode(&req.sha256_ciphertext_b64)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid sha256_ciphertext_b64"))?;

    if computed_hash.as_slice() != provided_hash.as_slice() {
        // Delete the corrupt blob
        let _ = tokio::fs::remove_file(&blob_path).await;
        return Err(error_response(StatusCode::BAD_REQUEST, "SHA-256 mismatch"));
    }

    let client_size = validate_blob_size(req.blob_size)?;
    if actual_size != expected_size || client_size != expected_size {
        let _ = tokio::fs::remove_file(&blob_path).await;
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Blob size does not match initialized size",
        ));
    }

    // Update file record
    let now = Utc::now().to_rfc3339();
    let mut active: file::ActiveModel = existing.into();
    active.sha256_ciphertext = Set(computed_hash.to_vec());
    active.blob_size = Set(actual_size as i64);
    active.status = Set("complete".into());
    active.updated_at = Set(now.clone());
    active
        .update(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    // Log event
    let event = event_log::ActiveModel {
        seq: Default::default(),
        event_type: Set("file.created".into()),
        object_kind: Set("file".into()),
        object_id: Set(file_id.clone()),
        created_at: Set(now.clone()),
    };
    let inserted = event
        .insert(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    let _ = state.ws_tx().send(WsBroadcast {
        seq: inserted.seq,
        event_type: "file.created".into(),
        object_kind: "file".into(),
        object_id: file_id.clone(),
        created_at: now,
    });

    info!(device_id = %auth.device_id, file_id = %file_id, "File upload completed");

    Ok(Json(OkResponse { ok: true }))
}

#[derive(Debug, serde::Deserialize)]
pub struct FileListQuery {
    pub limit: Option<u64>,
    pub before: Option<String>,
}

pub async fn list_files(
    State(state): State<AppState>,
    Query(query): Query<FileListQuery>,
) -> Result<Json<FileListResponse>, StatusCode> {
    let b64 = &base64::engine::general_purpose::STANDARD;
    let limit = query.limit.unwrap_or(100).min(500);

    let mut q = file::Entity::find()
        .filter(file::Column::Status.eq("complete"))
        .order_by(file::Column::CreatedAt, Order::Desc);

    if let Some(before) = &query.before {
        q = q.filter(file::Column::CreatedAt.lt(before.clone()));
    }

    let items = q
        .all(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let has_more = items.len() as u64 > limit;
    let items: Vec<file::Model> = items.into_iter().take(limit as usize).collect();

    let result_items: Vec<FileListItem> = items
        .iter()
        .map(|f| FileListItem {
            id: f.id.clone(),
            meta_nonce_b64: b64.encode(&f.meta_nonce),
            meta_ciphertext_b64: b64.encode(&f.meta_ciphertext),
            blob_nonce_b64: b64.encode(&f.blob_nonce),
            blob_size: f.blob_size,
            created_at: f.created_at.clone(),
            source_device_id: f.source_device_id.clone(),
        })
        .collect();

    let next_before = if has_more {
        items.last().map(|i| i.created_at.clone())
    } else {
        None
    };

    Ok(Json(FileListResponse {
        items: result_items,
        next_before,
    }))
}

pub async fn download_blob(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> Result<Body, StatusCode> {
    validate_client_id(&file_id).map_err(|(status, _)| status)?;

    let existing = file::Entity::find_by_id(&file_id)
        .filter(file::Column::Status.eq("complete"))
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let path = state.files_dir().join(&existing.blob_path);
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let stream = ReaderStream::new(file);
    Ok(Body::from_stream(stream))
}

pub async fn delete_file(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path(file_id): Path<String>,
) -> Result<Json<OkResponse>, StatusCode> {
    validate_client_id(&file_id).map_err(|(status, _)| status)?;

    let existing = file::Entity::find_by_id(&file_id)
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Delete blob
    let path = state.files_dir().join(&existing.blob_path);
    let _ = tokio::fs::remove_file(&path).await;

    // Delete DB record
    file::Entity::delete_by_id(&file_id)
        .exec(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Log event
    let now = Utc::now().to_rfc3339();
    let event = event_log::ActiveModel {
        seq: Default::default(),
        event_type: Set("file.deleted".into()),
        object_kind: Set("file".into()),
        object_id: Set(file_id.clone()),
        created_at: Set(now.clone()),
    };
    let inserted = event
        .insert(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let _ = state.ws_tx().send(WsBroadcast {
        seq: inserted.seq,
        event_type: "file.deleted".into(),
        object_kind: "file".into(),
        object_id: file_id.clone(),
        created_at: now,
    });

    info!(device_id = %auth.device_id, file_id = %file_id, "File deleted");

    Ok(Json(OkResponse { ok: true }))
}

fn validate_blob_size(size: i64) -> Result<u64, (StatusCode, Json<ErrorResponse>)> {
    if size < 0 {
        return Err(error_response(StatusCode::BAD_REQUEST, "Invalid blob_size"));
    }

    let size = size as u64;
    if size > MAX_FILE_BLOB_BYTES {
        return Err(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "File exceeds maximum size",
        ));
    }

    Ok(size)
}

async fn sha256_file(path: &std::path::Path) -> std::io::Result<([u8; 32], u64)> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 16 * 1024];
    let mut size = 0_u64;

    loop {
        let read = file.read(&mut buf).await?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buf[..read]);
    }

    Ok((hasher.finalize().into(), size))
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

    fn init_request(id: String, blob_size: i64, source_device_id: &str) -> FileInitRequest {
        FileInitRequest {
            id,
            meta_nonce_b64: B64.encode([1_u8; 12]),
            meta_ciphertext_b64: B64.encode(b"encrypted metadata"),
            blob_nonce_b64: B64.encode([2_u8; 12]),
            blob_size,
            source_device_id: source_device_id.into(),
        }
    }

    #[tokio::test]
    async fn init_rejects_non_uuid_id() {
        let (state, data_dir) = test_state().await;

        let result = init_upload(
            State(state),
            Extension(auth("device-a")),
            Json(init_request("../escape".into(), 3, "device-a")),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert!(!data_dir.path().join("files").join("escape.bin").exists());
    }

    #[tokio::test]
    async fn init_rejects_files_over_size_limit() {
        let (state, _data_dir) = test_state().await;
        let id = Uuid::new_v4().to_string();

        let result = init_upload(
            State(state),
            Extension(auth("device-a")),
            Json(init_request(id, MAX_FILE_BLOB_BYTES as i64 + 1, "device-a")),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn init_uses_authenticated_device_as_source() {
        let (state, _data_dir) = test_state().await;
        let id = Uuid::new_v4().to_string();

        let _ = init_upload(
            State(state.clone()),
            Extension(auth("device-auth")),
            Json(init_request(id.clone(), 3, "device-spoof")),
        )
        .await
        .expect("init");

        let file = file::Entity::find_by_id(id)
            .one(state.db())
            .await
            .expect("query")
            .expect("file");
        assert_eq!(file.source_device_id, "device-auth");
    }

    #[tokio::test]
    async fn upload_blob_rejects_body_size_mismatch() {
        let (state, data_dir) = test_state().await;
        let id = Uuid::new_v4().to_string();

        let _ = init_upload(
            State(state.clone()),
            Extension(auth("device-a")),
            Json(init_request(id.clone(), 2, "device-a")),
        )
        .await
        .expect("init");

        let result = upload_blob(
            State(state),
            Extension(auth("device-a")),
            Path(id.clone()),
            Body::from(vec![1_u8, 2, 3]),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert!(
            !data_dir
                .path()
                .join("files")
                .join(format!("{id}.bin"))
                .exists()
        );
    }
}
