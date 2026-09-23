use sea_orm_migration::prelude::*;

mod m20260312_000001_create_tables;
mod m20260615_000002_collab_docs;
mod m20260826_000003_collab_doc_title;
mod m20260908_000004_schedule_objects;
mod m20260908_000005_object_revisions;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260312_000001_create_tables::Migration),
            Box::new(m20260615_000002_collab_docs::Migration),
            Box::new(m20260826_000003_collab_doc_title::Migration),
            Box::new(m20260908_000004_schedule_objects::Migration),
            Box::new(m20260908_000005_object_revisions::Migration),
        ]
    }
}
