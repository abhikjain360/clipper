use axum::{
    Extension,
    extract::{Query, State},
};
use chrono::Utc;
use clipper_core::{
    crypto::app_data::verify_app_data_change_signature,
    models::{
        ApiErrorCode, AppDataChange, AppDataChangeRefusal, AppDataChangeResult, AppDataChangesPage,
        AppDataChangesQuery, AppDataChangesRequest, AppDataChangesResponse, AppDataRow,
        MAX_APP_DATA_PAGE_CHANGES,
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use tracing::error;
use uuid::Uuid;

use super::{ApiError, Postcard, RouteResult, with_txn};
use crate::{
    auth::AuthInfo,
    entity::{app_data_rows, devices},
    state::AppState,
};

pub fn change_routes(state: &AppState) -> axum::routing::MethodRouter<AppState> {
    axum::routing::get(get_changes)
        .post(post_changes)
        .layer(axum::extract::DefaultBodyLimit::max(
            state
                .config()
                .limits
                .max_app_data_ciphertext_bytes
                .saturating_mul(clipper_core::models::MAX_APP_DATA_BATCH_CHANGES)
                .saturating_add(64 * 1024),
        ))
}

pub async fn post_changes(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Postcard(request): Postcard<AppDataChangesRequest>,
) -> RouteResult<Postcard<AppDataChangesResponse>> {
    let results = with_txn(state.db(), "app data changes", async |txn| {
        let device = devices::Entity::find_by_id(auth.device_id)
            .filter(devices::Column::UserId.eq(auth.user_id))
            .one(txn)
            .await
            .map_err(database_error)?
            .ok_or_else(|| ApiError::from_code(ApiErrorCode::Unauthorized))?;
        let mut row_count = app_data_rows::Entity::find()
            .filter(app_data_rows::Column::UserId.eq(auth.user_id))
            .count(txn)
            .await
            .map_err(database_error)?;
        let mut results = Vec::with_capacity(request.changes.len());
        for change in request.changes {
            if let Some(reason) = refusal(&state, &auth, &device.signing_public_key, &change) {
                results.push(AppDataChangeResult::Refused { reason });
                continue;
            }
            let current = app_data_rows::Entity::find_by_id((auth.user_id, change.row_key.clone()))
                .one(txn)
                .await
                .map_err(database_error)?;
            let stored_revision = current.as_ref().map_or(0, |row| row.revision as u64);
            if stored_revision != change.replaces_revision {
                results.push(AppDataChangeResult::Conflict {
                    current: current.map(row_response),
                });
                continue;
            }
            if current.is_none() && row_count >= state.config().limits.max_user_app_data_rows {
                results.push(AppDataChangeResult::Refused {
                    reason: AppDataChangeRefusal::OverQuota,
                });
                continue;
            }
            let sequence = state.next_event_seq();
            let row = app_data_rows::ActiveModel {
                user_id: Set(auth.user_id),
                row_key: Set(change.row_key),
                revision: Set(change.revision as i64),
                sequence: Set(sequence),
                deleted: Set(change.deleted),
                nonce: Set(change.nonce),
                ciphertext: Set(change.ciphertext),
                device_id: Set(Some(auth.device_id)),
                signature: Set(change.signature),
                received_at: Set(Utc::now().to_rfc3339()),
            };
            if current.is_some() {
                row.update(txn).await.map_err(database_error)?;
            } else {
                row.insert(txn).await.map_err(database_error)?;
                row_count += 1;
            }
            results.push(AppDataChangeResult::Accepted { sequence });
        }
        Ok(results)
    })
    .await?;
    if let Some(sequence) = results
        .iter()
        .filter_map(|result| match result {
            AppDataChangeResult::Accepted { sequence } => Some(*sequence),
            _ => None,
        })
        .max()
    {
        state.broadcast_app_data_change(auth.user_id, auth.device_id, sequence);
    }
    Ok(Postcard(AppDataChangesResponse { results }))
}

fn refusal(
    state: &AppState,
    auth: &AuthInfo,
    public_key: &[u8],
    change: &AppDataChange,
) -> Option<AppDataChangeRefusal> {
    let value_valid = match (&change.nonce, &change.ciphertext, change.deleted) {
        (None, None, true) => true,
        (Some(nonce), Some(ciphertext), false) => nonce.len() == 24 && ciphertext.len() >= 16,
        _ => false,
    };
    if change.row_key.len() != 32
        || change.signature.len() != 64
        || !value_valid
        || change.device_id.into_uuid() != auth.device_id
        || change.revision == 0
        || change.revision > i64::MAX as u64
        || change.replaces_revision.checked_add(1) != Some(change.revision)
    {
        return Some(AppDataChangeRefusal::Malformed);
    }
    if change
        .ciphertext
        .as_ref()
        .is_some_and(|value| value.len() > state.config().limits.max_app_data_ciphertext_bytes)
    {
        return Some(AppDataChangeRefusal::TooLarge);
    }
    if verify_app_data_change_signature(public_key, change).is_err() {
        return Some(AppDataChangeRefusal::BadSignature);
    }
    None
}

pub async fn get_changes(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Query(query): Query<AppDataChangesQuery>,
) -> RouteResult<Postcard<AppDataChangesPage>> {
    if query.after < 0 || query.limit == 0 || query.limit > MAX_APP_DATA_PAGE_CHANGES {
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ValidationFailed,
            "Invalid changes cursor or limit",
        ));
    }
    let txn = state.db().begin().await.map_err(database_error)?;
    let rows = app_data_rows::Entity::find()
        .filter(app_data_rows::Column::UserId.eq(auth.user_id))
        .filter(app_data_rows::Column::Sequence.gt(query.after))
        .order_by_asc(app_data_rows::Column::Sequence)
        .limit(query.limit)
        .all(&txn)
        .await
        .map_err(database_error)?;
    let newest_sequence = newest_sequence(&txn, auth.user_id)
        .await
        .map_err(database_error)?;
    txn.commit().await.map_err(database_error)?;
    Ok(Postcard(AppDataChangesPage {
        rows: rows.into_iter().map(row_response).collect(),
        newest_sequence,
    }))
}

pub(crate) async fn newest_sequence(
    db: &impl sea_orm::ConnectionTrait,
    user_id: Uuid,
) -> Result<i64, sea_orm::DbErr> {
    Ok(app_data_rows::Entity::find()
        .filter(app_data_rows::Column::UserId.eq(user_id))
        .select_only()
        .column(app_data_rows::Column::Sequence)
        .order_by_desc(app_data_rows::Column::Sequence)
        .into_tuple::<i64>()
        .one(db)
        .await?
        .unwrap_or(0))
}

fn row_response(row: app_data_rows::Model) -> AppDataRow {
    AppDataRow {
        row_key: row.row_key,
        revision: row.revision as u64,
        sequence: row.sequence,
        deleted: row.deleted,
        nonce: row.nonce,
        ciphertext: row.ciphertext,
        device_id: row.device_id.map(Into::into),
        signature: row.signature,
    }
}

fn database_error(error: sea_orm::DbErr) -> ApiError {
    error!(%error, "App data database error");
    ApiError::from_code(ApiErrorCode::Database)
}

#[cfg(test)]
mod tests;
