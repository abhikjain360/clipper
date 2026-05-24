use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
};
use base64::Engine;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;
use tracing::info;

use crate::auth::AuthInfo;
use crate::entity::{event_log, files};
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

    let file_uuid = validate_client_id(&req.id)?;
    let blob_size = validate_blob_size(req.blob_size)?;

    if files::Entity::find_by_id(file_uuid)
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

    let new_file = files::ActiveModel {
        id: Set(file_uuid),
        user_id: Set(auth.user_id),
        blob_path: Set(blob_filename),
        meta_ciphertext: Set(meta_ciphertext),
        meta_nonce: Set(meta_nonce),
        blob_nonce: Set(blob_nonce),
        blob_size: Set(blob_size as i64),
        sha256_ciphertext: Set(vec![]), // filled on complete
        created_at: Set(now.clone()),
        updated_at: Set(now),
        source_device_id: Set(auth.device_id),
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
    let file_uuid = validate_client_id(&file_id)?;

    // Verify file exists and is pending
    let existing = files::Entity::find_by_id(file_uuid)
        .filter(files::Column::UserId.eq(auth.user_id))
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
    let now = Utc::now().to_rfc3339();
    let claimed = files::Entity::update_many()
        .col_expr(
            files::Column::Status,
            sea_orm::sea_query::Expr::value("uploading"),
        )
        .col_expr(
            files::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(files::Column::Id.eq(file_uuid))
        .filter(files::Column::UserId.eq(auth.user_id))
        .filter(files::Column::Status.eq("pending"))
        .filter(files::Column::SourceDeviceId.eq(auth.device_id))
        .exec(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    if claimed.rows_affected != 1 {
        return Err(error_response(
            StatusCode::CONFLICT,
            "File upload in progress",
        ));
    }

    // Stream body to disk
    let files_dir = state.files_dir();
    if tokio::fs::create_dir_all(&files_dir).await.is_err() {
        reset_file_status(&state, file_uuid, "uploading", "pending").await;
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Storage error",
        ));
    }
    let path = files_dir.join(&existing.blob_path);
    let tmp_path = files_dir.join(format!(
        "{}.{}.tmp",
        existing.blob_path,
        uuid::Uuid::new_v4()
    ));
    let mut out_file = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .await
    {
        Ok(file) => file,
        Err(_) => {
            reset_file_status(&state, file_uuid, "uploading", "pending").await;
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Storage error",
            ));
        }
    };

    use futures_util::StreamExt;
    let mut stream = body.into_data_stream();
    let mut total_size = 0_u64;
    while let Some(chunk) = stream.next().await {
        let data = match chunk {
            Ok(data) => data,
            Err(_) => {
                drop(out_file);
                let _ = tokio::fs::remove_file(&tmp_path).await;
                reset_file_status(&state, file_uuid, "uploading", "pending").await;
                return Err(error_response(StatusCode::BAD_REQUEST, "Stream error"));
            }
        };
        total_size += data.len() as u64;
        if total_size > expected_size {
            drop(out_file);
            let _ = tokio::fs::remove_file(&tmp_path).await;
            reset_file_status(&state, file_uuid, "uploading", "pending").await;
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Blob size does not match initialized size",
            ));
        }
        if out_file.write_all(&data).await.is_err() {
            drop(out_file);
            let _ = tokio::fs::remove_file(&tmp_path).await;
            reset_file_status(&state, file_uuid, "uploading", "pending").await;
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Write error",
            ));
        }
    }
    if out_file.flush().await.is_err() {
        drop(out_file);
        let _ = tokio::fs::remove_file(&tmp_path).await;
        reset_file_status(&state, file_uuid, "uploading", "pending").await;
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Flush error",
        ));
    }
    drop(out_file);

    if total_size != expected_size {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        reset_file_status(&state, file_uuid, "uploading", "pending").await;
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Blob size does not match initialized size",
        ));
    }

    let _ = tokio::fs::remove_file(&path).await;
    if tokio::fs::rename(&tmp_path, &path).await.is_err() {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        reset_file_status(&state, file_uuid, "uploading", "pending").await;
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Storage error",
        ));
    }

    let now = Utc::now().to_rfc3339();
    let uploaded = files::Entity::update_many()
        .col_expr(
            files::Column::Status,
            sea_orm::sea_query::Expr::value("uploaded"),
        )
        .col_expr(
            files::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(files::Column::Id.eq(file_uuid))
        .filter(files::Column::UserId.eq(auth.user_id))
        .filter(files::Column::Status.eq("uploading"))
        .exec(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    if uploaded.rows_affected != 1 {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error_response(
            StatusCode::CONFLICT,
            "File upload no longer in progress",
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

    let file_uuid = validate_client_id(&file_id)?;

    let existing = files::Entity::find_by_id(file_uuid)
        .filter(files::Column::UserId.eq(auth.user_id))
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "File not found"))?;

    if existing.status != "uploaded" {
        return Err(error_response(
            StatusCode::CONFLICT,
            "File blob has not been uploaded",
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
        reset_file_status(&state, file_uuid, "uploaded", "pending").await;
        return Err(error_response(StatusCode::BAD_REQUEST, "SHA-256 mismatch"));
    }

    let client_size = validate_blob_size(req.blob_size)?;
    if actual_size != expected_size || client_size != expected_size {
        let _ = tokio::fs::remove_file(&blob_path).await;
        reset_file_status(&state, file_uuid, "uploaded", "pending").await;
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Blob size does not match initialized size",
        ));
    }

    // Update file record and log event atomically.
    let now = Utc::now().to_rfc3339();
    let txn = state
        .db()
        .begin()
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;
    let updated = files::Entity::update_many()
        .col_expr(
            files::Column::Sha256Ciphertext,
            sea_orm::sea_query::Expr::value(computed_hash.to_vec()),
        )
        .col_expr(
            files::Column::BlobSize,
            sea_orm::sea_query::Expr::value(actual_size as i64),
        )
        .col_expr(
            files::Column::Status,
            sea_orm::sea_query::Expr::value("complete"),
        )
        .col_expr(
            files::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now.clone()),
        )
        .filter(files::Column::Id.eq(file_uuid))
        .filter(files::Column::UserId.eq(auth.user_id))
        .filter(files::Column::Status.eq("uploaded"))
        .filter(files::Column::SourceDeviceId.eq(auth.device_id))
        .exec(&txn)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    if updated.rows_affected != 1 {
        let _ = txn.rollback().await;
        return Err(error_response(
            StatusCode::CONFLICT,
            "File upload no longer ready to complete",
        ));
    }

    // Log event
    let event = event_log::ActiveModel {
        seq: Default::default(),
        user_id: Set(auth.user_id),
        event_type: Set("file.created".into()),
        object_kind: Set("file".into()),
        object_id: Set(file_uuid),
        created_at: Set(now.clone()),
    };
    let inserted = match event.insert(&txn).await {
        Ok(inserted) => inserted,
        Err(_) => {
            let _ = txn.rollback().await;
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database error",
            ));
        }
    };

    txn.commit()
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    let _ = state.ws_tx().send(WsBroadcast {
        user_id: auth.user_id,
        seq: i64::from(inserted.seq),
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
    Extension(auth): Extension<AuthInfo>,
    Query(query): Query<FileListQuery>,
) -> Result<Json<FileListResponse>, StatusCode> {
    let b64 = &base64::engine::general_purpose::STANDARD;
    let limit = query.limit.unwrap_or(100).min(500);

    let mut q = files::Entity::find()
        .filter(files::Column::UserId.eq(auth.user_id))
        .filter(files::Column::Status.eq("complete"))
        .order_by(files::Column::CreatedAt, Order::Desc);

    if let Some(before) = &query.before {
        q = q.filter(files::Column::CreatedAt.lt(before.clone()));
    }

    let items = q
        .all(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let has_more = items.len() as u64 > limit;
    let items: Vec<files::Model> = items.into_iter().take(limit as usize).collect();

    let result_items: Vec<FileListItem> = items
        .iter()
        .map(|f| FileListItem {
            id: f.id.to_string(),
            meta_nonce_b64: b64.encode(&f.meta_nonce),
            meta_ciphertext_b64: b64.encode(&f.meta_ciphertext),
            blob_nonce_b64: b64.encode(&f.blob_nonce),
            blob_size: f.blob_size,
            created_at: f.created_at.clone(),
            source_device_id: f.source_device_id.to_string(),
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
    Extension(auth): Extension<AuthInfo>,
    Path(file_id): Path<String>,
) -> Result<Body, StatusCode> {
    let file_uuid = validate_client_id(&file_id).map_err(|(status, _)| status)?;

    let existing = files::Entity::find_by_id(file_uuid)
        .filter(files::Column::UserId.eq(auth.user_id))
        .filter(files::Column::Status.eq("complete"))
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
    let file_uuid = validate_client_id(&file_id).map_err(|(status, _)| status)?;

    let existing = files::Entity::find_by_id(file_uuid)
        .filter(files::Column::UserId.eq(auth.user_id))
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let path = state.files_dir().join(&existing.blob_path);
    let txn = state
        .db()
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Delete DB record and log event atomically.
    files::Entity::delete_by_id(file_uuid)
        .exec(&txn)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let now = Utc::now().to_rfc3339();
    let event = event_log::ActiveModel {
        seq: Default::default(),
        user_id: Set(auth.user_id),
        event_type: Set("file.deleted".into()),
        object_kind: Set("file".into()),
        object_id: Set(file_uuid),
        created_at: Set(now.clone()),
    };
    let inserted = match event.insert(&txn).await {
        Ok(inserted) => inserted,
        Err(_) => {
            let _ = txn.rollback().await;
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    txn.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Delete blob after the durable delete event exists.
    let _ = tokio::fs::remove_file(&path).await;

    let _ = state.ws_tx().send(WsBroadcast {
        user_id: auth.user_id,
        seq: i64::from(inserted.seq),
        event_type: "file.deleted".into(),
        object_kind: "file".into(),
        object_id: file_id.clone(),
        created_at: now,
    });

    info!(device_id = %auth.device_id, file_id = %file_id, "File deleted");

    Ok(Json(OkResponse { ok: true }))
}

async fn reset_file_status(state: &AppState, file_id: uuid::Uuid, from: &str, to: &str) {
    let now = Utc::now().to_rfc3339();
    let _ = files::Entity::update_many()
        .col_expr(files::Column::Status, sea_orm::sea_query::Expr::value(to))
        .col_expr(
            files::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(files::Column::Id.eq(file_id))
        .filter(files::Column::Status.eq(from))
        .exec(state.db())
        .await;
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
    use sea_orm::{ActiveModelTrait, Database, EntityTrait, Set};
    use sea_orm_migration::MigratorTrait;
    use tempfile::TempDir;
    use uuid::Uuid;

    use crate::entity::{access_keys, devices, users};
    use crate::migration;

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    async fn test_state() -> (AppState, TempDir) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        migration::Migrator::up(&db, None).await.expect("migrate");
        (AppState::new(db, data_dir.path().to_path_buf()), data_dir)
    }

    fn auth(user_id: Uuid, device_id: Uuid) -> AuthInfo {
        AuthInfo {
            session_id: Uuid::new_v4(),
            user_id,
            device_id,
        }
    }

    async fn insert_user(state: &AppState) -> Uuid {
        let now = Utc::now().to_rfc3339();
        let user_id = Uuid::new_v4();
        let access_key_hash = Uuid::new_v4().to_string();
        access_keys::ActiveModel {
            key_hash: Set(access_key_hash.clone()),
            created_at: Set(now.clone()),
            expires_at: Set(None),
            used_at: Set(Some(now.clone())),
            used_by_user_id: Set(Some(user_id)),
        }
        .insert(state.db())
        .await
        .expect("insert access key");
        users::ActiveModel {
            id: Set(user_id),
            opaque_server_setup: Set(vec![1]),
            opaque_password_file: Set(vec![2]),
            encryption_salt: Set(vec![3]),
            access_key_hash: Set(access_key_hash),
            created_at: Set(now.clone()),
            updated_at: Set(now),
        }
        .insert(state.db())
        .await
        .expect("insert user");
        user_id
    }

    async fn insert_device(state: &AppState, user_id: Uuid, id: Uuid) {
        let now = Utc::now().to_rfc3339();
        devices::ActiveModel {
            id: Set(id),
            user_id: Set(user_id),
            name: Set("test-device".into()),
            platform: Set("test".into()),
            created_at: Set(now.clone()),
            updated_at: Set(now.clone()),
            last_seen_at: Set(now),
        }
        .insert(state.db())
        .await
        .expect("insert device");
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

    // This tries to create a pending file with a path-like ID. We test it
    // because file IDs become blob filenames, and one bad pending record would
    // put later upload/download/delete operations on an unsafe path.
    #[tokio::test]
    async fn init_rejects_non_uuid_id() {
        let (state, data_dir) = test_state().await;

        let result = init_upload(
            State(state),
            Extension(auth(Uuid::new_v4(), Uuid::new_v4())),
            Json(init_request("../escape".into(), 3, "device-a")),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert!(!data_dir.path().join("files").join("escape.bin").exists());
    }

    // This declares a blob larger than the server limit during init. We test it
    // here because rejecting before the body upload prevents resource-exhaustion
    // work from ever starting.
    #[tokio::test]
    async fn init_rejects_files_over_size_limit() {
        let (state, _data_dir) = test_state().await;
        let id = Uuid::new_v4().to_string();

        let result = init_upload(
            State(state),
            Extension(auth(Uuid::new_v4(), Uuid::new_v4())),
            Json(init_request(id, MAX_FILE_BLOB_BYTES as i64 + 1, "device-a")),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::PAYLOAD_TOO_LARGE);
    }

    // This sends a spoofed source_device_id in the file metadata request. We
    // test it because file provenance must be derived from the authenticated
    // session, not from a client-controlled field.
    #[tokio::test]
    async fn init_uses_authenticated_device_as_source() {
        let (state, _data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_auth = Uuid::new_v4();
        insert_device(&state, user_id, device_auth).await;
        let id = Uuid::new_v4();

        let _ = init_upload(
            State(state.clone()),
            Extension(auth(user_id, device_auth)),
            Json(init_request(id.to_string(), 3, "device-spoof")),
        )
        .await
        .expect("init");

        let file = files::Entity::find_by_id(id)
            .one(state.db())
            .await
            .expect("query")
            .expect("file");
        assert_eq!(file.source_device_id, device_auth);
    }

    // This uploads a body whose byte count does not match the init metadata. We
    // test it because mismatched partial files must be removed before they can
    // later be completed or served.
    #[tokio::test]
    async fn upload_blob_rejects_body_size_mismatch() {
        let (state, data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_a = Uuid::new_v4();
        insert_device(&state, user_id, device_a).await;
        let id = Uuid::new_v4().to_string();

        let _ = init_upload(
            State(state.clone()),
            Extension(auth(user_id, device_a)),
            Json(init_request(id.clone(), 2, "device-a")),
        )
        .await
        .expect("init");

        let result = upload_blob(
            State(state),
            Extension(auth(user_id, device_a)),
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

    #[tokio::test]
    async fn upload_blob_rejects_duplicate_without_overwriting_blob() {
        let (state, data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_a = Uuid::new_v4();
        insert_device(&state, user_id, device_a).await;
        let id = Uuid::new_v4().to_string();

        let _ = init_upload(
            State(state.clone()),
            Extension(auth(user_id, device_a)),
            Json(init_request(id.clone(), 5, "device-a")),
        )
        .await
        .expect("init");

        let _ = upload_blob(
            State(state.clone()),
            Extension(auth(user_id, device_a)),
            Path(id.clone()),
            Body::from("first"),
        )
        .await
        .expect("first upload");

        let result = upload_blob(
            State(state),
            Extension(auth(user_id, device_a)),
            Path(id.clone()),
            Body::from("xxxxx"),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
        let blob = tokio::fs::read(data_dir.path().join("files").join(format!("{id}.bin")))
            .await
            .expect("blob");
        assert_eq!(blob, b"first");
    }

    #[tokio::test]
    async fn complete_upload_marks_file_complete_and_logs_event() {
        let (state, _data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_a = Uuid::new_v4();
        insert_device(&state, user_id, device_a).await;
        let id = Uuid::new_v4();
        let id_string = id.to_string();
        let blob = b"encrypted blob";

        let _ = init_upload(
            State(state.clone()),
            Extension(auth(user_id, device_a)),
            Json(init_request(
                id_string.clone(),
                blob.len() as i64,
                "device-a",
            )),
        )
        .await
        .expect("init");
        let _ = upload_blob(
            State(state.clone()),
            Extension(auth(user_id, device_a)),
            Path(id_string.clone()),
            Body::from(blob.to_vec()),
        )
        .await
        .expect("upload");

        let _ = complete_upload(
            State(state.clone()),
            Extension(auth(user_id, device_a)),
            Path(id_string.clone()),
            Json(FileCompleteRequest {
                sha256_ciphertext_b64: B64.encode(clipper_core::crypto::sha256(blob)),
                blob_size: blob.len() as i64,
            }),
        )
        .await
        .expect("complete");

        let file = files::Entity::find_by_id(id)
            .one(state.db())
            .await
            .expect("query")
            .expect("file");
        assert_eq!(file.status, "complete");

        let events = event_log::Entity::find()
            .all(state.db())
            .await
            .expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "file.created");
        assert_eq!(events[0].object_id, id);
    }
}
