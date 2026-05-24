use axum::{
    Json,
    extract::{Extension, State},
    http::StatusCode,
};
use base64::Engine;
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};

use crate::auth::AuthInfo;
use crate::entity::{clipboard_items, devices, event_log, files, users};
use crate::state::AppState;
use clipper_core::crypto::Argon2Params;
use clipper_core::models::{
    BootstrapResponse, ClipboardItem, DeviceInfo, FileListItem, ServerInfo,
};

pub async fn bootstrap(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
) -> Result<Json<BootstrapResponse>, StatusCode> {
    let b64 = &base64::engine::general_purpose::STANDARD;

    // Get device info
    let dev = devices::Entity::find_by_id(auth.device_id)
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Get user crypto info
    let user = users::Entity::find_by_id(auth.user_id)
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    // Recent clipboard items (last 100)
    let clips = clipboard_items::Entity::find()
        .filter(clipboard_items::Column::UserId.eq(auth.user_id))
        .order_by(clipboard_items::Column::CreatedAt, Order::Desc)
        .all(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let clips: Vec<clipboard_items::Model> = clips.into_iter().take(100).collect();

    let mut clipboard_items = Vec::new();
    for item in &clips {
        let path = state.clipboard_dir().join(&item.ciphertext_path);
        if let Ok(ct) = tokio::fs::read(&path).await {
            clipboard_items.push(ClipboardItem {
                id: item.id.to_string(),
                nonce_b64: b64.encode(&item.nonce),
                ciphertext_b64: b64.encode(&ct),
                created_at: item.created_at.clone(),
                source_device_id: item.source_device_id.to_string(),
            });
        }
    }

    // Recent files (last 100)
    let files = files::Entity::find()
        .filter(files::Column::UserId.eq(auth.user_id))
        .filter(files::Column::Status.eq("complete"))
        .order_by(files::Column::CreatedAt, Order::Desc)
        .all(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let files: Vec<files::Model> = files.into_iter().take(100).collect();
    let file_items: Vec<FileListItem> = files
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

    // Latest event seq
    let latest_seq = event_log::Entity::find()
        .filter(event_log::Column::UserId.eq(auth.user_id))
        .order_by(event_log::Column::Seq, Order::Desc)
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|e| i64::from(e.seq))
        .unwrap_or(0);

    Ok(Json(BootstrapResponse {
        device: DeviceInfo {
            id: dev.id.to_string(),
            name: dev.name,
            platform: dev.platform,
        },
        clipboard_items,
        files: file_items,
        latest_seq,
        server: ServerInfo {
            encryption_salt_b64: b64.encode(&user.encryption_salt),
            encryption_params: Argon2Params::default(),
        },
    }))
}
