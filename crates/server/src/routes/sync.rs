use axum::{
    Json,
    extract::{Extension, State},
};
use clipper_core::models::{ApiErrorCode, BootstrapResponse, DeviceInfo, ServerInfo};
use sea_orm::{
    ColumnTrait, DerivePartialModel, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect,
};
use tracing::error;
use uuid::Uuid;

use crate::{
    auth::AuthInfo,
    entity::{devices, event_log},
    routes::ApiError,
    state::AppState,
};

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "devices::Entity", from_query_result)]
struct BootstrapDeviceRow {
    id: Uuid,
    name: String,
    platform: String,
}

pub async fn bootstrap(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
) -> Result<Json<BootstrapResponse>, ApiError> {
    let dev = devices::Entity::find_by_id(auth.device_id)
        .into_partial_model::<BootstrapDeviceRow>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                device_id = %auth.device_id,
                error = %e,
                "Failed to look up device in bootstrap",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .ok_or_else(|| {
            error!(
                device_id = %auth.device_id,
                "Authenticated device row missing in bootstrap (data inconsistency)",
            );
            ApiError::from_code_with_message(ApiErrorCode::NotFound, "Device not found")
        })?;

    let latest_seq = event_log::Entity::find()
        .filter(event_log::Column::UserId.eq(auth.user_id))
        .order_by(event_log::Column::Seq, Order::Desc)
        .select_only()
        .column(event_log::Column::Seq)
        .into_tuple::<i64>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                user_id = %auth.user_id,
                error = %e,
                "Failed to load latest event_log seq in bootstrap",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .unwrap_or(0);

    Ok(Json(BootstrapResponse {
        device: DeviceInfo {
            id: dev.id.into(),
            name: dev.name,
            platform: dev.platform,
        },
        latest_seq,
        server: ServerInfo {},
    }))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, Database, Set};
    use uuid::Uuid;

    use super::*;
    use crate::{
        entity::{access_keys, users},
        secret::ServerSecrets,
    };

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
    async fn bootstrap_returns_device_and_latest_seq() {
        let (state, _data_dir) = empty_state().await;
        let now = Utc::now().to_rfc3339();
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let access_key_hash = "bootstrap-test-access-key".to_string();

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
            username: Set(user_id.as_simple().to_string()),
            opaque_password_file: Set(vec![2]),
            encryption_salt: Set(Vec::new()),
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

        assert_eq!(response.device.id, device_id.into());
        assert_eq!(response.latest_seq, 0);
    }
}
