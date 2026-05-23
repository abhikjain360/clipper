use axum::{extract::{Extension, State}, http::StatusCode, Json};
use base64::Engine;
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};

use crate::auth::AuthInfo;
use crate::entity::{clipboard_item, device, event_log, file, server_config};
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
    let dev = device::Entity::find_by_id(&auth.device_id)
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Get server config
    let config = server_config::Entity::find_by_id(1)
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    // Recent clipboard items (last 100)
    let clips = clipboard_item::Entity::find()
        .order_by(clipboard_item::Column::CreatedAt, Order::Desc)
        .all(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let clips: Vec<clipboard_item::Model> = clips.into_iter().take(100).collect();

    let mut clipboard_items = Vec::new();
    for item in &clips {
        let path = state.clipboard_dir().join(&item.ciphertext_path);
        if let Ok(ct) = tokio::fs::read(&path).await {
            clipboard_items.push(ClipboardItem {
                id: item.id.clone(),
                nonce_b64: b64.encode(&item.nonce),
                ciphertext_b64: b64.encode(&ct),
                created_at: item.created_at.clone(),
                source_device_id: item.source_device_id.clone(),
            });
        }
    }

    // Recent files (last 100)
    let files = file::Entity::find()
        .filter(file::Column::Status.eq("complete"))
        .order_by(file::Column::CreatedAt, Order::Desc)
        .all(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let files: Vec<file::Model> = files.into_iter().take(100).collect();
    let file_items: Vec<FileListItem> = files
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

    // Latest event seq
    let latest_seq = event_log::Entity::find()
        .order_by(event_log::Column::Seq, Order::Desc)
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|e| e.seq)
        .unwrap_or(0);

    Ok(Json(BootstrapResponse {
        device: DeviceInfo {
            id: dev.id,
            name: dev.name,
            platform: dev.platform,
        },
        clipboard_items,
        files: file_items,
        latest_seq,
        server: ServerInfo {
            enc_salt_b64: b64.encode(&config.enc_salt),
            auth_params: Argon2Params::default(),
            enc_params: Argon2Params::default(),
        },
    }))
}
