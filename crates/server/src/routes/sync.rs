use axum::{
    Json,
    extract::{Extension, State},
    http::StatusCode,
};
use base64::Engine;
use clipper_core::models::{BootstrapResponse, DeviceInfo, ServerInfo};
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};

use crate::{
    auth::AuthInfo,
    entity::{devices, event_log, users},
    secret_storage,
    state::AppState,
};

pub async fn bootstrap(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
) -> Result<Json<BootstrapResponse>, StatusCode> {
    let b64 = &base64::engine::general_purpose::STANDARD;

    let dev = devices::Entity::find_by_id(auth.device_id)
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let user = users::Entity::find_by_id(auth.user_id)
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let latest_seq = event_log::Entity::find()
        .filter(event_log::Column::UserId.eq(auth.user_id))
        .order_by(event_log::Column::Seq, Order::Desc)
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|e| i64::from(e.seq))
        .unwrap_or(0);

    let encryption_salt = secret_storage::unwrap_encryption_salt(
        state.secrets(),
        &user.encryption_salt,
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(BootstrapResponse {
        device: DeviceInfo {
            id: dev.id.into(),
            name: dev.name,
            platform: dev.platform,
        },
        latest_seq,
        server: ServerInfo {
            encryption_salt_b64: b64.encode(&encryption_salt),
            encryption_params: state.config().crypto.encryption_params,
        },
    }))
}
