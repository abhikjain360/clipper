use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    Json,
};
use base64::Engine;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, Set};
use tracing::info;

use crate::auth::AuthInfo;
use crate::entity::{clipboard_item, event_log};
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

    let ciphertext = b64.decode(&req.ciphertext_b64).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid ciphertext_b64".into(),
            }),
        )
    })?;

    let nonce = b64.decode(&req.nonce_b64).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid nonce_b64".into(),
            }),
        )
    })?;

    let provided_hash = b64.decode(&req.ciphertext_sha256_b64).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid ciphertext_sha256_b64".into(),
            }),
        )
    })?;

    // Verify hash
    let computed_hash = sha256(&ciphertext);
    if computed_hash.as_slice() != provided_hash.as_slice() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "SHA-256 mismatch".into(),
            }),
        ));
    }

    // Write ciphertext to disk
    let clip_dir = state.clipboard_dir();
    tokio::fs::create_dir_all(&clip_dir).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Storage error".into(),
            }),
        )
    })?;

    let filename = format!("{}.bin", req.id);
    let path = clip_dir.join(&filename);
    tokio::fs::write(&path, &ciphertext).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Storage error".into(),
            }),
        )
    })?;

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
        source_device_id: Set(req.source_device_id.clone()),
    };
    item.insert(state.db()).await.map_err(|_| {
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
        event_type: Set("clipboard.created".into()),
        object_kind: Set("clipboard".into()),
        object_id: Set(req.id.clone()),
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

    let mut q = clipboard_item::Entity::find()
        .order_by(clipboard_item::Column::CreatedAt, Order::Desc);

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
