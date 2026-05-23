use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "clipboard_items")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub ciphertext_path: String,
    pub nonce: Vec<u8>,
    pub ciphertext_size: i64,
    pub sha256_ciphertext: Vec<u8>,
    pub created_at: String,
    pub expires_at: String,
    pub source_device_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
