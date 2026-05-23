use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "event_log")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub seq: i64,
    pub event_type: String,
    pub object_kind: String,
    pub object_id: String,
    pub created_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
