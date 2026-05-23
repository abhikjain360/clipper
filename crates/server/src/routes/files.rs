use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use base64::Engine;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, Set};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use tracing::info;

use crate::auth::AuthInfo;
use crate::entity::{event_log, file};
use crate::state::AppState;
use crate::ws::WsBroadcast;
use clipper_core::crypto::sha256;
use clipper_core::models::{
    ErrorResponse, FileCompleteRequest, FileInitRequest, FileInitResponse, FileListItem,
    FileListResponse, OkResponse,
};

pub async fn init_upload(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Json(req): Json<FileInitRequest>,
) -> Result<Json<FileInitResponse>, (StatusCode, Json<ErrorResponse>)> {
    let b64 = &base64::engine::general_purpose::STANDARD;
    let now = Utc::now().to_rfc3339();

    let meta_ciphertext = b64.decode(&req.meta_ciphertext_b64).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid meta_ciphertext_b64".into(),
            }),
        )
    })?;
    let meta_nonce = b64.decode(&req.meta_nonce_b64).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid meta_nonce_b64".into(),
            }),
        )
    })?;
    let blob_nonce = b64.decode(&req.blob_nonce_b64).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid blob_nonce_b64".into(),
            }),
        )
    })?;

    let files_dir = state.files_dir();
    tokio::fs::create_dir_all(&files_dir).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Storage error".into(),
            }),
        )
    })?;

    let blob_filename = format!("{}.bin", req.id);

    let new_file = file::ActiveModel {
        id: Set(req.id.clone()),
        blob_path: Set(blob_filename),
        meta_ciphertext: Set(meta_ciphertext),
        meta_nonce: Set(meta_nonce),
        blob_nonce: Set(blob_nonce),
        blob_size: Set(req.blob_size),
        sha256_ciphertext: Set(vec![]), // filled on complete
        created_at: Set(now.clone()),
        updated_at: Set(now),
        source_device_id: Set(req.source_device_id),
        status: Set("pending".into()),
    };
    new_file.insert(state.db()).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Database error".into(),
            }),
        )
    })?;

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
    // Verify file exists and is pending
    let existing = file::Entity::find_by_id(&file_id)
        .one(state.db())
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Database error".into(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "File not found".into(),
                }),
            )
        })?;

    if existing.status != "pending" {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "File already uploaded".into(),
            }),
        ));
    }

    // Stream body to disk
    let path = state.files_dir().join(&existing.blob_path);
    let mut out_file = tokio::fs::File::create(&path).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Storage error".into(),
            }),
        )
    })?;

    use futures_util::StreamExt;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let data = chunk.map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Stream error".into(),
                }),
            )
        })?;
        out_file.write_all(&data).await.map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Write error".into(),
                }),
            )
        })?;
    }
    out_file.flush().await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Flush error".into(),
            }),
        )
    })?;

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

    let existing = file::Entity::find_by_id(&file_id)
        .one(state.db())
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Database error".into(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "File not found".into(),
                }),
            )
        })?;

    if existing.status != "pending" {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "File already completed".into(),
            }),
        ));
    }

    // Verify the blob exists and compute hash
    let blob_path = state.files_dir().join(&existing.blob_path);
    let blob_data = tokio::fs::read(&blob_path).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Blob not found".into(),
            }),
        )
    })?;

    let computed_hash = sha256(&blob_data);
    let provided_hash = b64.decode(&req.sha256_ciphertext_b64).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid sha256_ciphertext_b64".into(),
            }),
        )
    })?;

    if computed_hash.as_slice() != provided_hash.as_slice() {
        // Delete the corrupt blob
        let _ = tokio::fs::remove_file(&blob_path).await;
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "SHA-256 mismatch".into(),
            }),
        ));
    }

    // Update file record
    let now = Utc::now().to_rfc3339();
    let mut active: file::ActiveModel = existing.into();
    active.sha256_ciphertext = Set(computed_hash.to_vec());
    active.blob_size = Set(req.blob_size);
    active.status = Set("complete".into());
    active.updated_at = Set(now.clone());
    active.update(state.db()).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Database error".into(),
            }),
        )
    })?;

    // Log event
    let event = event_log::ActiveModel {
        seq: Default::default(),
        event_type: Set("file.created".into()),
        object_kind: Set("file".into()),
        object_id: Set(file_id.clone()),
        created_at: Set(now.clone()),
    };
    let inserted = event.insert(state.db()).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Database error".into(),
            }),
        )
    })?;

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
