use chrono::{Duration, Utc};
use futures_util::{StreamExt, stream};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QuerySelect, TransactionTrait};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    entity::{event_log, object_payloads, object_revisions, objects, sessions},
    state::AppState,
    storage_quota::{self, UserStorageUsage},
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
        .filter(objects::Column::PublishedSeq.is_not_null())
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
    // One call drains every batch: a full batch may leave more orphans
    // behind, while a short batch ends the sweep.
    loop {
        if !cleanup_orphan_object_uploads_batch(state).await? {
            return Ok(());
        }
    }
}

/// Cap one orphan-sweep batch. The per-orphan OR conditions below bind two
/// SQLite parameters each, so an unbounded orphan set blows past the bind
/// limit and fails the sweep on every cycle; batching bounds every statement.
///
/// Returns whether a full batch was cleaned, meaning another batch may remain.
const ORPHAN_SWEEP_BATCH: u64 = 500;

async fn cleanup_orphan_object_uploads_batch(state: &AppState) -> CleanupResult<bool> {
    let cutoff = (Utc::now()
        - Duration::seconds(state.config().cleanup.orphan_upload_ttl_secs as i64))
    .to_rfc3339();

    let txn = state.db().begin().await?;

    // An abandoned upload is now a pending *revision*, not a pending object,
    // and the difference matters: a failed edit must cost the object nothing.
    // So this sweeps revisions, and only removes the object underneath when
    // that revision was the object's first and it never became visible.
    //
    // Eligibility keys on the server-assigned `stored_at`, never the client's
    // `created_at`: the latter rides in the client envelope and could be
    // backdated to make a fresh upload instantly orphan-eligible, or
    // future-dated to escape the sweep forever. `stored_at` is stamped by the
    // server at init and bumped on every payload upload, so this means "no
    // upload progress for orphan_upload_ttl_secs" rather than "created long ago".
    //
    // Select the orphan set inside the transaction with the SAME predicate the
    // delete uses. A revision that races to `complete` between selection and
    // the delete is excluded by the status filter on the delete itself, so a
    // just-completed revision (and its payload files) can never be destroyed
    // here.
    let orphans: Vec<(Uuid, i64)> = object_revisions::Entity::find()
        .filter(object_revisions::Column::Status.ne("complete"))
        .filter(object_revisions::Column::StoredAt.lt(&cutoff))
        .select_only()
        .column(object_revisions::Column::ObjectId)
        .column(object_revisions::Column::Revision)
        .limit(ORPHAN_SWEEP_BATCH)
        .into_tuple()
        .all(&txn)
        .await?;
    if orphans.is_empty() {
        _ = txn.rollback().await;
        return Ok(false);
    }

    let orphan_object_ids: Vec<Uuid> = {
        let mut ids: Vec<Uuid> = orphans.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };

    // Objects that never published anything. A chain only extends from a
    // complete head, so such an object has exactly this one pending revision
    // and nothing worth keeping the row for.
    let stillborn_ids: Vec<Uuid> = objects::Entity::find()
        .filter(objects::Column::Id.is_in(orphan_object_ids.clone()))
        .filter(objects::Column::PublishedSeq.is_null())
        .select_only()
        .column(objects::Column::Id)
        .into_tuple()
        .all(&txn)
        .await?;

    let payload_paths: Vec<String> = object_payloads::Entity::find()
        .filter(payload_paths_for_revisions(&orphans))
        .select_only()
        .column(object_payloads::Column::CiphertextPath)
        .into_tuple()
        .all(&txn)
        .await?;

    // Bytes come from the revisions; the object count only from the stillborn
    // objects. Counting both from `object_usage_by_user` would double-charge a
    // release on an object that also has live revisions.
    let mut usage = storage_quota::revision_usage_by_user(&txn, &orphans).await?;
    for stillborn in storage_quota::object_usage_by_user(&txn, &stillborn_ids).await? {
        match usage.iter_mut().find(|u| u.user_id == stillborn.user_id) {
            Some(existing) => existing.object_count += stillborn.object_count,
            None => usage.push(UserStorageUsage {
                storage_bytes: 0,
                ..stillborn
            }),
        }
    }

    let res = object_revisions::Entity::delete_many()
        .filter(revision_keys_condition(&orphans))
        .filter(object_revisions::Column::Status.ne("complete"))
        .filter(object_revisions::Column::StoredAt.lt(&cutoff))
        .exec(&txn)
        .await?;
    if res.rows_affected as usize != orphans.len() {
        // A pending upload completed concurrently, so the usage counted above
        // no longer matches the rows the status-filtered delete actually
        // removed. Roll back and retry on the next sweep rather than
        // mis-accounting a user's storage quota (the just-completed revision is
        // preserved either way).
        _ = txn.rollback().await;
        debug!(
            deleted = res.rows_affected,
            expected = orphans.len(),
            "Orphan cleanup raced a completion; retrying on the next sweep",
        );
        return Ok(false);
    }

    if !stillborn_ids.is_empty() {
        objects::Entity::delete_many()
            .filter(objects::Column::Id.is_in(stillborn_ids.clone()))
            .filter(objects::Column::PublishedSeq.is_null())
            .exec(&txn)
            .await?;
    }

    for usage in usage {
        // One user's release failing (its counters no longer cover the usage)
        // must not roll back the deletes for everyone else in the batch.
        if let Err(e) = storage_quota::release_user_storage(&txn, usage).await {
            warn!(user_id = %usage.user_id, error = %e, "Orphan cleanup could not release user storage quota");
        }
    }
    txn.commit().await?;

    remove_payload_files(state, payload_paths).await;
    if res.rows_affected > 0 {
        info!(
            revisions = res.rows_affected,
            objects = stillborn_ids.len(),
            "Cleaned up orphan object uploads"
        );
    }

    Ok(res.rows_affected >= ORPHAN_SWEEP_BATCH)
}

/// Match exactly the given `(object_id, revision)` pairs.
///
/// Two `IN` lists would match their cross product, which on a mixed orphan set
/// would delete live revisions of other objects.
fn revision_keys_condition(revisions: &[(Uuid, i64)]) -> Condition {
    revisions
        .iter()
        .fold(Condition::any(), |condition, (object_id, revision)| {
            condition.add(
                Condition::all()
                    .add(object_revisions::Column::ObjectId.eq(*object_id))
                    .add(object_revisions::Column::Revision.eq(*revision)),
            )
        })
}

/// The same pair-wise match, against `object_payloads`.
fn payload_paths_for_revisions(revisions: &[(Uuid, i64)]) -> Condition {
    revisions
        .iter()
        .fold(Condition::any(), |condition, (object_id, revision)| {
            condition.add(
                Condition::all()
                    .add(object_payloads::Column::ObjectId.eq(*object_id))
                    .add(object_payloads::Column::Revision.eq(*revision)),
            )
        })
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
        // One user's release failing must not roll back the deletes for the
        // other users in the set.
        if let Err(e) = storage_quota::release_user_storage(&txn, usage).await {
            warn!(user_id = %usage.user_id, error = %e, "Object cleanup could not release user storage quota");
        }
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

#[cfg(test)]
mod tests {
    use sea_orm::{ActiveModelTrait, Database, Set};

    use super::*;
    use crate::{
        entity::{access_keys, users},
        secret::ServerSecrets,
    };

    async fn test_state() -> (AppState, tempfile::TempDir) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        let mut config = crate::config::ServerConfig::default();
        config.server.data_dir = data_dir.path().to_path_buf();
        let state = AppState::open_with_db_and_config(db, config, ServerSecrets::test_fixture())
            .await
            .expect("state");
        (state, data_dir)
    }

    async fn insert_user(state: &AppState, storage_bytes: i64, object_count: i64) -> Uuid {
        let now = Utc::now().to_rfc3339();
        let user_id = Uuid::now_v7();
        let access_key_hash = Uuid::now_v7().to_string();
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
            encryption_salt: Set(vec![3]),
            access_key_hash: Set(access_key_hash),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            storage_bytes: Set(storage_bytes),
            object_count: Set(object_count),
        }
        .insert(state.db())
        .await
        .expect("insert user");
        user_id
    }

    /// A pending genesis revision old enough for the orphan sweep, with one
    /// payload row so the payload-path queries bind it too.
    async fn insert_orphan(state: &AppState, user_id: Uuid) -> Uuid {
        let object_id = Uuid::now_v7();
        let old = "2000-01-01T00:00:00+00:00".to_owned();
        objects::ActiveModel {
            id: Set(object_id),
            user_id: Set(user_id),
            kind: Set("file".to_string()),
            created_at: Set(old.clone()),
            updated_at: Set(old.clone()),
            expires_at: Set(None),
            head_revision: Set(None),
            published_seq: Set(None),
            deleted_at: Set(None),
            collab_doc_id: Set(None),
        }
        .insert(state.db())
        .await
        .expect("insert object");
        object_revisions::ActiveModel {
            object_id: Set(object_id),
            revision: Set(1),
            operation: Set("create".to_string()),
            parent_hash: Set(None),
            meta_ciphertext: Set(b"meta".to_vec()),
            meta_nonce: Set(vec![0_u8; 24]),
            envelope: Set(vec![0_u8; 8]),
            source_device_id: Set(None),
            created_at: Set(old.clone()),
            stored_at: Set(old.clone()),
            status: Set("pending".to_string()),
            created_seq: Set(None),
        }
        .insert(state.db())
        .await
        .expect("insert revision");
        object_payloads::ActiveModel {
            object_id: Set(object_id),
            revision: Set(1),
            payload_id: Set(Uuid::now_v7()),
            ciphertext_path: Set(format!("{object_id}.r1.orphan.bin")),
            nonce: Set(vec![0_u8; 24]),
            ciphertext_size: Set(10),
            sha256_ciphertext: Set(vec![0_u8; 32]),
            created_at: Set(old.clone()),
            updated_at: Set(old),
            status: Set("pending".to_string()),
        }
        .insert(state.db())
        .await
        .expect("insert payload");
        object_id
    }

    /// A complete object with one revision and one payload, as expiry cleanup
    /// consumes it.
    async fn insert_complete_object(state: &AppState, user_id: Uuid) -> Uuid {
        let object_id = insert_orphan(state, user_id).await;
        let now = Utc::now().to_rfc3339();
        objects::Entity::update_many()
            .col_expr(
                objects::Column::HeadRevision,
                sea_orm::sea_query::Expr::value(1_i64),
            )
            .col_expr(
                objects::Column::PublishedSeq,
                sea_orm::sea_query::Expr::value(1_i64),
            )
            .filter(objects::Column::Id.eq(object_id))
            .exec(state.db())
            .await
            .expect("publish object");
        object_revisions::Entity::update_many()
            .col_expr(
                object_revisions::Column::Status,
                sea_orm::sea_query::Expr::value("complete"),
            )
            .col_expr(
                object_revisions::Column::CreatedSeq,
                sea_orm::sea_query::Expr::value(1_i64),
            )
            .col_expr(
                object_revisions::Column::StoredAt,
                sea_orm::sea_query::Expr::value(now.clone()),
            )
            .filter(object_revisions::Column::ObjectId.eq(object_id))
            .exec(state.db())
            .await
            .expect("complete revision");
        object_payloads::Entity::update_many()
            .col_expr(
                object_payloads::Column::Status,
                sea_orm::sea_query::Expr::value("complete"),
            )
            .col_expr(
                object_payloads::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(object_payloads::Column::ObjectId.eq(object_id))
            .exec(state.db())
            .await
            .expect("complete payload");
        object_id
    }

    #[tokio::test]
    async fn orphan_sweep_cleans_more_than_one_batch_in_a_single_call() {
        let (state, _dir) = test_state().await;
        let user_id = insert_user(&state, 1_000_000_000, 100_000).await;
        for _ in 0..ORPHAN_SWEEP_BATCH as usize + 5 {
            insert_orphan(&state, user_id).await;
        }

        cleanup_orphan_object_uploads(&state).await.expect("sweep");

        assert!(
            object_revisions::Entity::find()
                .all(state.db())
                .await
                .expect("query revisions")
                .is_empty(),
            "every orphan batch must be cleaned by one call"
        );
        assert!(
            objects::Entity::find()
                .all(state.db())
                .await
                .expect("query objects")
                .is_empty(),
            "stillborn objects must go with their orphan revisions"
        );
    }

    #[tokio::test]
    async fn orphan_sweep_continues_past_one_users_release_failure() {
        let (state, _dir) = test_state().await;
        // Zeroed counters cover none of the orphan's bytes, so this user's
        // release fails.
        let broke = insert_user(&state, 0, 0).await;
        let broke_orphan = insert_orphan(&state, broke).await;
        let healthy = insert_user(&state, 1_000_000, 100_000).await;
        let healthy_orphan = insert_orphan(&state, healthy).await;

        cleanup_orphan_object_uploads(&state)
            .await
            .expect("one user's release failure must not fail the sweep");

        for orphan in [broke_orphan, healthy_orphan] {
            assert!(
                object_revisions::Entity::find_by_id((orphan, 1))
                    .one(state.db())
                    .await
                    .expect("query revision")
                    .is_none(),
                "orphan {orphan} must be cleaned",
            );
        }
    }

    #[tokio::test]
    async fn object_cleanup_continues_past_one_users_release_failure() {
        let (state, _dir) = test_state().await;
        let broke = insert_user(&state, 0, 0).await;
        let broke_id = insert_complete_object(&state, broke).await;
        let healthy = insert_user(&state, 1_000_000, 100_000).await;
        let healthy_id = insert_complete_object(&state, healthy).await;

        let cleaned = delete_objects_and_release_usage(&state, &[broke_id, healthy_id])
            .await
            .expect("one user's release failure must not fail the cleanup");
        assert_eq!(cleaned, 2);

        assert!(
            objects::Entity::find()
                .all(state.db())
                .await
                .expect("query objects")
                .is_empty(),
            "both users' objects must be deleted",
        );
    }
}
