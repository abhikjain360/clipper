use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "files")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub blob_path: String,
    pub meta_ciphertext: Vec<u8>,
    pub meta_nonce: Vec<u8>,
    pub blob_nonce: Vec<u8>,
    pub blob_size: i64,
    pub sha256_ciphertext: Vec<u8>,
    pub created_at: String,
    pub updated_at: String,
    pub source_device_id: String,
    /// "pending", "complete"
    pub status: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
