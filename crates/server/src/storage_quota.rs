use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, JoinType, QueryFilter,
    QuerySelect, RelationTrait,
    sea_query::{Expr, Func, SimpleExpr},
};
use uuid::Uuid;

use crate::entity::{object_payloads, object_revisions, objects, users};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UserStorageUsage {
    pub user_id: Uuid,
    pub object_count: i64,
    pub storage_bytes: i64,
}

/// What one stored revision costs: payload ciphertext bytes plus metadata
/// ciphertext bytes. Both sides of the quota go through this definition. The
/// write paths call `revision_cost_bytes` with the request's sizes before
/// reserving. The release aggregations below add the payload sum to the
/// metadata sum with the same function, so the two cannot drift apart.
pub(crate) fn revision_cost_bytes(
    meta_ciphertext_len: i64,
    payload_bytes: i64,
) -> Option<i64> {
    if meta_ciphertext_len < 0 || payload_bytes < 0 {
        return None;
    }
    meta_ciphertext_len.checked_add(payload_bytes)
}

/// `SUM(LENGTH(object_revisions.meta_ciphertext))` as a select expression.
/// SQLite `LENGTH` on a blob counts bytes, which is what is stored.
///
/// This stays separate from the payload sum because the release queries join
/// revisions to payloads. Summing metadata over the joined rows would count
/// one revision's metadata once per payload instead of once.
pub(crate) fn meta_bytes_sum_expr() -> SimpleExpr {
    Func::sum(Func::cust("LENGTH").arg(Expr::col(object_revisions::Column::MetaCiphertext)))
        .into()
}

/// Reserve room for one write.
///
/// `objects_added` is 1 for a new object and 0 for a new revision of one that
/// already exists: a revision consumes bytes but does not add to the object
/// count, or editing a file repeatedly would exhaust the object quota without
/// creating anything new.
pub(crate) async fn try_reserve_user_storage<C>(
    db: &C,
    user_id: Uuid,
    storage_bytes: i64,
    objects_added: i64,
    max_storage_bytes: i64,
    max_objects: i64,
) -> Result<bool, DbErr>
where
    C: ConnectionTrait,
{
    if storage_bytes < 0
        || max_storage_bytes < 0
        || max_objects < 1
        || !(0..=1).contains(&objects_added)
    {
        return Err(DbErr::Custom("invalid storage quota reservation".into()));
    }
    // No bytes and no object: nothing to check, so succeed without touching
    // the row. A user over a lowered quota must still write payload-less
    // tombstones (to purge back under it), mirroring `release_user_storage`.
    if storage_bytes == 0 && objects_added == 0 {
        return Ok(true);
    }
    if storage_bytes > max_storage_bytes {
        return Ok(false);
    }

    let result = users::Entity::update_many()
        .col_expr(
            users::Column::StorageBytes,
            Expr::col(users::Column::StorageBytes).add(storage_bytes),
        )
        .col_expr(
            users::Column::ObjectCount,
            Expr::col(users::Column::ObjectCount).add(objects_added),
        )
        .filter(users::Column::Id.eq(user_id))
        .filter(users::Column::StorageBytes.lte(max_storage_bytes - storage_bytes))
        .filter(users::Column::ObjectCount.lte(max_objects - objects_added))
        .exec(db)
        .await?;

    Ok(result.rows_affected == 1)
}

pub(crate) async fn release_user_storage<C>(db: &C, usage: UserStorageUsage) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    if usage.object_count < 0 || usage.storage_bytes < 0 {
        return Err(DbErr::Custom("invalid storage quota release".into()));
    }
    if usage.object_count == 0 && usage.storage_bytes == 0 {
        return Ok(());
    }

    let result = users::Entity::update_many()
        .col_expr(
            users::Column::StorageBytes,
            Expr::col(users::Column::StorageBytes).sub(usage.storage_bytes),
        )
        .col_expr(
            users::Column::ObjectCount,
            Expr::col(users::Column::ObjectCount).sub(usage.object_count),
        )
        .filter(users::Column::Id.eq(usage.user_id))
        .filter(users::Column::StorageBytes.gte(usage.storage_bytes))
        .filter(users::Column::ObjectCount.gte(usage.object_count))
        .exec(db)
        .await?;

    if result.rows_affected == 1 {
        Ok(())
    } else {
        Err(DbErr::Custom(format!(
            "storage quota release affected {} user rows for {}",
            result.rows_affected, usage.user_id,
        )))
    }
}

/// Bytes held by specific revisions, charged to their owners.
///
/// The object count is always zero here — deleting a revision leaves the object
/// standing. Use `object_usage_by_user` when the whole chain goes.
pub(crate) async fn revision_usage_by_user<C>(
    db: &C,
    revisions: &[(Uuid, i64)],
) -> Result<Vec<UserStorageUsage>, DbErr>
where
    C: ConnectionTrait,
{
    if revisions.is_empty() {
        return Ok(Vec::new());
    }

    // A pair-wise OR rather than two `IN` lists, which would match the cross
    // product and free bytes belonging to revisions nobody asked about. Orphan
    // and prune sets are small, so the shape costs nothing.
    let mut matches = Condition::any();
    for (object_id, revision) in revisions {
        matches = matches.add(
            Condition::all()
                .add(object_revisions::Column::ObjectId.eq(*object_id))
                .add(object_revisions::Column::Revision.eq(*revision)),
        );
    }

    let payload_bytes_by_user: Vec<(Uuid, Option<i64>)> = object_revisions::Entity::find()
        .join(
            JoinType::InnerJoin,
            object_revisions::Relation::Objects.def(),
        )
        .join(
            JoinType::LeftJoin,
            object_revisions::Relation::ObjectPayloads.def(),
        )
        .filter(matches.clone())
        .select_only()
        .column(objects::Column::UserId)
        .column_as(
            object_payloads::Column::CiphertextSize.sum(),
            "storage_bytes",
        )
        .group_by(objects::Column::UserId)
        .into_tuple()
        .all(db)
        .await?;

    // Metadata is summed without the payload join, so a revision with several
    // payloads counts its metadata once. See `meta_bytes_sum_expr`.
    let meta_bytes_by_user: Vec<(Uuid, Option<i64>)> = object_revisions::Entity::find()
        .join(
            JoinType::InnerJoin,
            object_revisions::Relation::Objects.def(),
        )
        .filter(matches)
        .select_only()
        .column(objects::Column::UserId)
        .column_as(meta_bytes_sum_expr(), "meta_bytes")
        .group_by(objects::Column::UserId)
        .into_tuple()
        .all(db)
        .await?;

    merge_usage(payload_bytes_by_user, meta_bytes_by_user)
}

pub(crate) async fn object_usage_by_user<C>(
    db: &C,
    object_ids: &[Uuid],
) -> Result<Vec<UserStorageUsage>, DbErr>
where
    C: ConnectionTrait,
{
    if object_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Two hops now, and the sum runs over every revision's payloads rather
    // than one set per object. That is the point: retained history is real
    // stored bytes, so it has to be charged.
    let payload_bytes_by_user: Vec<(Uuid, i64, Option<i64>)> = objects::Entity::find()
        .join(JoinType::LeftJoin, objects::Relation::ObjectRevisions.def())
        .join(
            JoinType::LeftJoin,
            object_revisions::Relation::ObjectPayloads.def(),
        )
        .filter(objects::Column::Id.is_in(object_ids.to_vec()))
        .select_only()
        .column(objects::Column::UserId)
        .column_as(
            objects::Column::Id.into_expr().count_distinct(),
            "object_count",
        )
        .column_as(
            object_payloads::Column::CiphertextSize.sum(),
            "storage_bytes",
        )
        .group_by(objects::Column::UserId)
        .into_tuple()
        .all(db)
        .await?;

    // Metadata is summed without the payload join, so a revision with several
    // payloads counts its metadata once. See `meta_bytes_sum_expr`.
    let meta_bytes_by_user: Vec<(Uuid, Option<i64>)> = object_revisions::Entity::find()
        .join(
            JoinType::InnerJoin,
            object_revisions::Relation::Objects.def(),
        )
        .filter(object_revisions::Column::ObjectId.is_in(object_ids.to_vec()))
        .select_only()
        .column(objects::Column::UserId)
        .column_as(meta_bytes_sum_expr(), "meta_bytes")
        .group_by(objects::Column::UserId)
        .into_tuple()
        .all(db)
        .await?;

    let mut meta_by_user = std::collections::HashMap::new();
    for (user_id, meta_bytes) in meta_bytes_by_user {
        let meta_bytes = meta_bytes.unwrap_or(0);
        if meta_bytes < 0 {
            return Err(DbErr::Custom(format!(
                "negative storage quota aggregate for {user_id}",
            )));
        }
        meta_by_user.insert(user_id, meta_bytes);
    }

    payload_bytes_by_user
        .into_iter()
        .map(|(user_id, object_count, payload_bytes)| {
            let payload_bytes = payload_bytes.unwrap_or(0);
            let meta_bytes = meta_by_user.remove(&user_id).unwrap_or(0);
            let storage_bytes =
                revision_cost_bytes(meta_bytes, payload_bytes).ok_or_else(|| {
                    DbErr::Custom(format!(
                        "invalid storage quota aggregate for {user_id}",
                    ))
                })?;
            if object_count < 0 {
                Err(DbErr::Custom(format!(
                    "negative storage quota aggregate for {user_id}",
                )))
            } else {
                Ok(UserStorageUsage {
                    user_id,
                    object_count,
                    storage_bytes,
                })
            }
        })
        .collect()
}

/// Combine per-user payload and metadata sums into revision costs.
///
/// Every user appears in at most one of the two inputs: a revision without
/// payloads has no payload row, and a user with no revisions is absent from
/// both. Either side alone still costs what it holds.
fn merge_usage(
    payload_bytes_by_user: Vec<(Uuid, Option<i64>)>,
    meta_bytes_by_user: Vec<(Uuid, Option<i64>)>,
) -> Result<Vec<UserStorageUsage>, DbErr> {
    let mut usage_by_user = std::collections::HashMap::new();
    for (user_id, payload_bytes) in payload_bytes_by_user {
        let payload_bytes = payload_bytes.unwrap_or(0);
        if payload_bytes < 0 {
            return Err(DbErr::Custom(format!(
                "negative storage quota aggregate for {user_id}",
            )));
        }
        usage_by_user.insert(user_id, (payload_bytes, 0_i64));
    }
    for (user_id, meta_bytes) in meta_bytes_by_user {
        let meta_bytes = meta_bytes.unwrap_or(0);
        if meta_bytes < 0 {
            return Err(DbErr::Custom(format!(
                "negative storage quota aggregate for {user_id}",
            )));
        }
        usage_by_user
            .entry(user_id)
            .and_modify(|(_, meta)| *meta = meta_bytes)
            .or_insert((0, meta_bytes));
    }
    usage_by_user
        .into_iter()
        .map(|(user_id, (payload_bytes, meta_bytes))| {
            let storage_bytes =
                revision_cost_bytes(meta_bytes, payload_bytes).ok_or_else(|| {
                    DbErr::Custom(format!(
                        "invalid storage quota aggregate for {user_id}",
                    ))
                })?;
            Ok(UserStorageUsage {
                user_id,
                object_count: 0,
                storage_bytes,
            })
        })
        .collect()
}
