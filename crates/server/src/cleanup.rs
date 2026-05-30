use chrono::{Duration, Utc};
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect};
use tracing::info;
use uuid::Uuid;

use crate::{
    entity::{event_log, object_payloads, objects},
    state::AppState,
};

type CleanupResult<T> = Result<T, CleanupError>;

#[derive(Debug, thiserror::Error)]
enum CleanupError {
    #[error("cleanup database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

/// Run periodic cleanup tasks.
pub async fn run_cleanup_loop(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
        state.config().cleanup.interval_secs,
    ));

    loop {
        interval.tick().await;
        if let Err(e) = cleanup_expired_clipboard_objects(&state).await {
            tracing::error!(error = %e, "Expired clipboard cleanup failed");
        }
        if let Err(e) = cleanup_excess_clipboard_objects(&state).await {
            tracing::error!(error = %e, "Excess clipboard cleanup failed");
        }
        if let Err(e) = cleanup_old_events(&state).await {
            tracing::error!(error = %e, "Event log cleanup failed");
        }
        if let Err(e) = cleanup_orphan_object_uploads(&state).await {
            tracing::error!(error = %e, "Orphan upload cleanup failed");
        }
    }
}

async fn cleanup_expired_clipboard_objects(state: &AppState) -> CleanupResult<()> {
    let now = Utc::now().to_rfc3339();
    let expired = objects::Entity::find()
        .filter(objects::Column::Kind.eq("clipboard"))
        .filter(objects::Column::ExpiresAt.is_not_null())
        .filter(objects::Column::ExpiresAt.lt(&now))
        .all(state.db())
        .await?;

    if !expired.is_empty() {
        let count = delete_clipboard_objects(state, &expired).await?;
        info!(count, "Cleaned up expired clipboard objects");
    }

    Ok(())
}

async fn cleanup_excess_clipboard_objects(state: &AppState) -> CleanupResult<()> {
    let user_ids: Vec<Uuid> = objects::Entity::find()
        .filter(objects::Column::Kind.eq("clipboard"))
        .select_only()
        .column(objects::Column::UserId)
        .distinct()
        .into_tuple()
        .all(state.db())
        .await?;

    let mut total = 0_usize;
    for user_id in user_ids {
        total += trim_user_clipboard(state, user_id).await?;
    }
    if total > 0 {
        info!(count = total, "Trimmed excess clipboard objects");
    }
    Ok(())
}

/// Delete clipboard objects beyond the configured per-user `max_items`, keeping the most recent.
///
/// Why pub(crate): the object init/complete handlers spawn this after a successful clipboard
/// write so the cap is enforced without waiting for the periodic loop.
pub(crate) async fn trim_user_clipboard(
    state: &AppState,
    user_id: Uuid,
) -> Result<usize, sea_orm::DbErr> {
    let max_items = state.config().clipboard.max_items;
    let excess = objects::Entity::find()
        .filter(objects::Column::Kind.eq("clipboard"))
        .filter(objects::Column::UserId.eq(user_id))
        .order_by(objects::Column::CreatedAt, Order::Desc)
        .offset(max_items)
        // SQLite rejects OFFSET without LIMIT, so bound generously to "all remaining rows".
        .limit(i64::MAX as u64)
        .all(state.db())
        .await?;
    if excess.is_empty() {
        return Ok(0);
    }
    delete_clipboard_objects(state, &excess).await
}

async fn delete_clipboard_objects(
    state: &AppState,
    items: &[objects::Model],
) -> Result<usize, sea_orm::DbErr> {
    if items.is_empty() {
        return Ok(0);
    }
    let ids: Vec<Uuid> = items.iter().map(|o| o.id).collect();

    let payloads = object_payloads::Entity::find()
        .filter(object_payloads::Column::ObjectId.is_in(ids.clone()))
        .all(state.db())
        .await?;
    for payload in &payloads {
        let path = state.objects_dir().join(&payload.ciphertext_path);
        let _ = tokio::fs::remove_file(path).await;
    }

    let res = objects::Entity::delete_many()
        .filter(objects::Column::Id.is_in(ids))
        .exec(state.db())
        .await?;
    Ok(res.rows_affected as usize)
}

async fn cleanup_old_events(state: &AppState) -> CleanupResult<()> {
    let cutoff =
        (Utc::now() - Duration::days(state.config().cleanup.event_log_retention_days)).to_rfc3339();

    let result = event_log::Entity::delete_many()
        .filter(event_log::Column::CreatedAt.lt(&cutoff))
        .exec(state.db())
        .await?;

    if result.rows_affected > 0 {
        info!(
            count = result.rows_affected,
            "Cleaned up old event log entries"
        );
    }

    Ok(())
}

async fn cleanup_orphan_object_uploads(state: &AppState) -> CleanupResult<()> {
    let cutoff = (Utc::now()
        - Duration::seconds(state.config().cleanup.orphan_upload_ttl_secs as i64))
    .to_rfc3339();

    let orphans = objects::Entity::find()
        .filter(objects::Column::Status.ne("complete"))
        .filter(objects::Column::CreatedAt.lt(&cutoff))
        .all(state.db())
        .await?;

    let count = orphans.len();
    for object in &orphans {
        let payloads = object_payloads::Entity::find()
            .filter(object_payloads::Column::ObjectId.eq(object.id))
            .all(state.db())
            .await?;
        for payload in payloads {
            let path = state.objects_dir().join(&payload.ciphertext_path);
            let _ = tokio::fs::remove_file(path).await;
        }
    }

    if count > 0 {
        objects::Entity::delete_many()
            .filter(objects::Column::Status.ne("complete"))
            .filter(objects::Column::CreatedAt.lt(&cutoff))
            .exec(state.db())
            .await?;
        info!(count, "Cleaned up orphan object uploads");
    }

    Ok(())
}
