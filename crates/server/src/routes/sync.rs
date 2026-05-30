use axum::{
    Json,
    extract::{Extension, State},
    http::StatusCode,
};
use base64::Engine;
use clipper_core::models::{BootstrapResponse, DeviceInfo, ServerInfo};
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};
use tracing::error;

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
        .map_err(|e| {
            error!(
                device_id = %auth.device_id,
                error = %e,
                "Failed to look up device in bootstrap",
            );
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or_else(|| {
            error!(
                device_id = %auth.device_id,
                "Authenticated device row missing in bootstrap (data inconsistency)",
            );
            StatusCode::NOT_FOUND
        })?;

    let user = users::Entity::find_by_id(auth.user_id)
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                user_id = %auth.user_id,
                error = %e,
                "Failed to look up user in bootstrap",
            );
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or_else(|| {
            error!(
                user_id = %auth.user_id,
                "Authenticated user row missing in bootstrap (data inconsistency)",
            );
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let latest_seq = event_log::Entity::find()
        .filter(event_log::Column::UserId.eq(auth.user_id))
        .order_by(event_log::Column::Seq, Order::Desc)
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                user_id = %auth.user_id,
                error = %e,
                "Failed to load latest event_log seq in bootstrap",
            );
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .map(|e| i64::from(e.seq))
        .unwrap_or(0);

    let encryption_salt =
        secret_storage::unwrap_encryption_salt(state.secrets(), &user.encryption_salt).map_err(
            |e| {
                error!(
                    user_id = %auth.user_id,
                    error = %e,
                    "Failed to unwrap encryption_salt in bootstrap",
                );
                StatusCode::INTERNAL_SERVER_ERROR
            },
        )?;

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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use clipper_core::crypto;
    use sea_orm::{ActiveModelTrait, Database, Set};
    use uuid::Uuid;

    use super::*;
    use crate::{entity::access_keys, secret::ServerSecrets};

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    async fn empty_state() -> (AppState, tempfile::TempDir) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        let mut config = crate::config::ServerConfig::default();
        config.server.data_dir = data_dir.path().to_path_buf();
        let state = AppState::open_with_db_and_config(db, config, ServerSecrets::test_fixture())
            .await
            .expect("state");
        (state, data_dir)
    }

    #[tokio::test]
    async fn bootstrap_returns_plaintext_encryption_salt() {
        let (state, _data_dir) = empty_state().await;
        let now = Utc::now().to_rfc3339();
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let access_key_hash = "bootstrap-test-access-key".to_string();
        let plaintext_salt = crypto::generate_encryption_salt();
        let wrapped_salt = secret_storage::wrap_encryption_salt(state.secrets(), &plaintext_salt)
            .expect("wrap salt");

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
            encryption_salt: Set(wrapped_salt.clone()),
            access_key_hash: Set(access_key_hash),
            created_at: Set(now.clone()),
            updated_at: Set(now.clone()),
        }
        .insert(state.db())
        .await
        .expect("insert user");

        devices::ActiveModel {
            id: Set(device_id),
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

        let Json(response) = bootstrap(
            State(state),
            Extension(AuthInfo {
                session_id: Uuid::now_v7(),
                user_id,
                device_id,
            }),
        )
        .await
        .expect("bootstrap");

        let returned_salt = B64
            .decode(response.server.encryption_salt_b64)
            .expect("decode salt");
        assert_eq!(returned_salt, plaintext_salt);
        assert_ne!(returned_salt, wrapped_salt);
    }
}
