use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, JoinType, QueryFilter,
    QuerySelect, RelationTrait, sea_query::Expr,
};
use uuid::Uuid;

use crate::entity::{object_payloads, object_revisions, objects, users};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UserStorageUsage {
    pub user_id: Uuid,
    pub object_count: i64,
    pub storage_bytes: i64,
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

    object_revisions::Entity::find()
        .join(
            JoinType::InnerJoin,
            object_revisions::Relation::Objects.def(),
        )
        .join(
            JoinType::LeftJoin,
            object_revisions::Relation::ObjectPayloads.def(),
        )
        .filter(matches)
        .select_only()
        .column(objects::Column::UserId)
        .column_as(
            object_payloads::Column::CiphertextSize.sum(),
            "storage_bytes",
        )
        .group_by(objects::Column::UserId)
        .into_tuple::<(Uuid, Option<i64>)>()
        .all(db)
        .await?
        .into_iter()
        .map(|(user_id, storage_bytes)| {
            let storage_bytes = storage_bytes.unwrap_or(0);
            if storage_bytes < 0 {
                Err(DbErr::Custom(format!(
                    "negative storage quota aggregate for {user_id}",
                )))
            } else {
                Ok(UserStorageUsage {
                    user_id,
                    object_count: 0,
                    storage_bytes,
                })
            }
        })
        .collect()
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
    // stored bytes, so it has to be charged (D6).
    objects::Entity::find()
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
        .into_tuple::<(Uuid, i64, Option<i64>)>()
        .all(db)
        .await?
        .into_iter()
        .map(|(user_id, object_count, storage_bytes)| {
            let storage_bytes = storage_bytes.unwrap_or(0);
            if object_count < 0 || storage_bytes < 0 {
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
