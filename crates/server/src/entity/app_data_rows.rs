use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "app_data_rows")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false, column_type = "Blob")]
    pub row_key: Vec<u8>,
    pub revision: i64,
    #[sea_orm(unique)]
    pub sequence: i64,
    pub deleted: bool,
    #[sea_orm(column_type = "Blob", nullable)]
    pub nonce: Option<Vec<u8>>,
    #[sea_orm(column_type = "Blob", nullable)]
    pub ciphertext: Option<Vec<u8>>,
    pub device_id: Option<Uuid>,
    #[sea_orm(column_type = "Blob")]
    pub signature: Vec<u8>,
    #[sea_orm(column_type = "Text")]
    pub received_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::devices::Entity",
        from = "Column::DeviceId",
        to = "super::devices::Column::Id",
        on_update = "Cascade",
        on_delete = "SetNull"
    )]
    Devices,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::UserId",
        to = "super::users::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Users,
}

impl Related<super::devices::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Devices.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Users.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
