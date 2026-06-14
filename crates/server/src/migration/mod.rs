use sea_orm_migration::prelude::*;

mod m20260312_000001_create_tables;
mod m20260615_000002_collab_docs;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260312_000001_create_tables::Migration),
            Box::new(m20260615_000002_collab_docs::Migration),
        ]
    }
}
