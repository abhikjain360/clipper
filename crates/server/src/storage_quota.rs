use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect, sea_query::Expr,
};
use uuid::Uuid;

use crate::entity::{object_payloads, objects, users};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UserStorageUsage {
    pub user_id: Uuid,
    pub object_count: i64,
    pub storage_bytes: i64,
}

pub(crate) async fn try_reserve_user_storage<C>(
    db: &C,
    user_id: Uuid,
    storage_bytes: i64,
    max_storage_bytes: i64,
    max_objects: i64,
) -> Result<bool, DbErr>
where
    C: ConnectionTrait,
{
    if storage_bytes < 0 || max_storage_bytes < 0 || max_objects < 1 {
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
            Expr::col(users::Column::ObjectCount).add(1_i64),
        )
        .filter(users::Column::Id.eq(user_id))
        .filter(users::Column::StorageBytes.lte(max_storage_bytes - storage_bytes))
        .filter(users::Column::ObjectCount.lte(max_objects - 1))
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

    objects::Entity::find()
        .left_join(object_payloads::Entity)
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
