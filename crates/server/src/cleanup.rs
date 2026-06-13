use chrono::{Duration, Utc};
use futures_util::{StreamExt, stream};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QuerySelect, TransactionTrait};
use tracing::{debug, info};
use uuid::Uuid;

use crate::{
    entity::{event_log, object_payloads, objects, sessions},
    state::AppState,
    storage_quota,
};

type CleanupResult<T> = Result<T, CleanupError>;
const FILE_DELETE_CONCURRENCY: usize = 16;

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
        if let Err(e) = cleanup_expired_sessions(&state).await {
            tracing::error!(error = %e, "Expired session cleanup failed");
        }
    }
}

/// Delete sessions whose bearer token has expired. They are already rejected at
/// auth time (`expires_at < now`), so this is housekeeping to stop the table
/// from growing without bound.
async fn cleanup_expired_sessions(state: &AppState) -> CleanupResult<()> {
    let now = Utc::now().to_rfc3339();
    let result = sessions::Entity::delete_many()
        .filter(sessions::Column::ExpiresAt.lt(&now))
        .exec(state.db())
        .await?;

    if result.rows_affected > 0 {
        info!(count = result.rows_affected, "Cleaned up expired sessions");
    }

    Ok(())
}

async fn cleanup_expired_clipboard_objects(state: &AppState) -> CleanupResult<()> {
    let now = Utc::now().to_rfc3339();
    let expired_ids: Vec<Uuid> = objects::Entity::find()
        .filter(objects::Column::Kind.eq("clipboard"))
        .filter(objects::Column::ExpiresAt.is_not_null())
        .filter(objects::Column::ExpiresAt.lt(&now))
        .select_only()
        .column(objects::Column::Id)
        .into_tuple()
        .all(state.db())
        .await?;

    if !expired_ids.is_empty() {
        let count = delete_clipboard_objects(state, &expired_ids).await?;
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
    let now = Utc::now().to_rfc3339();
    let max_items = state.config().clipboard.max_items;
    // The set the read paths treat as live: non-expired clipboard objects, newest
    // `max_items` by created_seq. Anything else that is non-expired is genuine
    // excess and is trimmed. Ranking the keep-set over the SAME (non-expired)
    // predicate the reads use is what fixes the eviction bug: an expired-but-
    // unreaped item can no longer occupy a keep slot and push a still-valid item
    // out. Expired items are left to `cleanup_expired_clipboard_objects` so this
    // inline trim does not race the periodic expiry sweep over the same rows.
    let retained = crate::routes::objects::retained_clipboard_object_ids_raw(
        state.db(),
        user_id,
        max_items,
        &now,
    )
    .await?;

    let mut victim_query = objects::Entity::find()
        .filter(objects::Column::Kind.eq("clipboard"))
        .filter(objects::Column::UserId.eq(user_id))
        .filter(objects::Column::CreatedSeq.is_not_null())
        .filter(
            Condition::any()
                .add(objects::Column::ExpiresAt.is_null())
                .add(objects::Column::ExpiresAt.gt(&now)),
        );
    if !retained.is_empty() {
        victim_query = victim_query.filter(objects::Column::Id.is_not_in(retained));
    }
    let excess_ids: Vec<Uuid> = victim_query
        .select_only()
        .column(objects::Column::Id)
        .into_tuple()
        .all(state.db())
        .await?;
    if excess_ids.is_empty() {
        return Ok(0);
    }
    delete_clipboard_objects(state, &excess_ids).await
}

async fn delete_clipboard_objects(state: &AppState, ids: &[Uuid]) -> Result<usize, sea_orm::DbErr> {
    delete_objects_and_release_usage(state, ids).await
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

    let txn = state.db().begin().await?;

    // Select the orphan set inside the transaction with the SAME predicate the
    // delete uses. An object that races to `complete` between selection and the
    // delete is excluded by the status filter on the delete itself, so a
    // just-completed object (and its payload files) can never be destroyed here.
    let orphan_ids: Vec<Uuid> = objects::Entity::find()
        .filter(objects::Column::Status.ne("complete"))
        .filter(objects::Column::CreatedAt.lt(&cutoff))
        .select_only()
        .column(objects::Column::Id)
        .into_tuple()
        .all(&txn)
        .await?;
    if orphan_ids.is_empty() {
        _ = txn.rollback().await;
        return Ok(());
    }

    let payload_paths: Vec<String> = object_payloads::Entity::find()
        .filter(object_payloads::Column::ObjectId.is_in(orphan_ids.clone()))
        .select_only()
        .column(object_payloads::Column::CiphertextPath)
        .into_tuple()
        .all(&txn)
        .await?;
    let usage = storage_quota::object_usage_by_user(&txn, &orphan_ids).await?;
    let expected_objects = usage.iter().try_fold(0_i64, |total, usage| {
        total
            .checked_add(usage.object_count)
            .ok_or_else(|| sea_orm::DbErr::Custom("orphan cleanup count overflow".into()))
    })?;

    let res = objects::Entity::delete_many()
        .filter(objects::Column::Id.is_in(orphan_ids))
        .filter(objects::Column::Status.ne("complete"))
        .filter(objects::Column::CreatedAt.lt(&cutoff))
        .exec(&txn)
        .await?;
    if res.rows_affected as i64 != expected_objects {
        // A pending upload completed concurrently, so the usage we counted no
        // longer matches the rows the status-filtered delete actually removed.
        // Roll back and retry on the next sweep rather than mis-accounting a
        // user's storage quota (the just-completed object is preserved either way).
        _ = txn.rollback().await;
        debug!(
            deleted = res.rows_affected,
            expected = expected_objects,
            "Orphan cleanup raced a completion; retrying on the next sweep",
        );
        return Ok(());
    }

    for usage in usage {
        storage_quota::release_user_storage(&txn, usage).await?;
    }
    txn.commit().await?;

    remove_payload_files(state, payload_paths).await;
    if res.rows_affected > 0 {
        info!(
            count = res.rows_affected,
            "Cleaned up orphan object uploads"
        );
    }

    Ok(())
}

async fn delete_objects_and_release_usage(
    state: &AppState,
    ids: &[Uuid],
) -> Result<usize, sea_orm::DbErr> {
    if ids.is_empty() {
        return Ok(0);
    }

    let txn = state.db().begin().await?;
    let payload_paths: Vec<String> = object_payloads::Entity::find()
        .filter(object_payloads::Column::ObjectId.is_in(ids.to_vec()))
        .select_only()
        .column(object_payloads::Column::CiphertextPath)
        .into_tuple()
        .all(&txn)
        .await?;
    let usage = storage_quota::object_usage_by_user(&txn, ids).await?;
    let expected_objects = usage.iter().try_fold(0_i64, |total, usage| {
        total
            .checked_add(usage.object_count)
            .ok_or_else(|| sea_orm::DbErr::Custom("object cleanup count overflow".into()))
    })?;

    let res = objects::Entity::delete_many()
        .filter(objects::Column::Id.is_in(ids.to_vec()))
        .exec(&txn)
        .await?;
    if res.rows_affected as i64 != expected_objects {
        return Err(sea_orm::DbErr::Custom(format!(
            "object cleanup deleted {} rows but counted {}",
            res.rows_affected, expected_objects,
        )));
    }

    for usage in usage {
        storage_quota::release_user_storage(&txn, usage).await?;
    }
    txn.commit().await?;

    remove_payload_files(state, payload_paths).await;
    Ok(res.rows_affected as usize)
}

async fn remove_payload_files(state: &AppState, payload_paths: Vec<String>) {
    let objects_dir = state.objects_dir();
    stream::iter(
        payload_paths
            .into_iter()
            .map(|payload_path| objects_dir.join(payload_path)),
    )
    .for_each_concurrent(FILE_DELETE_CONCURRENCY, |path| async move {
        _ = tokio::fs::remove_file(path).await;
    })
    .await;
}
