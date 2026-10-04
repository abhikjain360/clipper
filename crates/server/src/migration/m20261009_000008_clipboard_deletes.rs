use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE event_log RENAME TO event_log_old;
             CREATE TABLE event_log (
                seq INTEGER PRIMARY KEY NOT NULL,
                user_id UUID NOT NULL,
                event_type TEXT NOT NULL,
                object_kind TEXT NOT NULL,
                object_id UUID NOT NULL,
                created_at TEXT NOT NULL,
                CHECK (event_type IN ('created', 'updated', 'deleted')),
                CHECK (object_kind IN ('clipboard', 'file', 'collab', 'schedule', 'app_document')),
                CHECK (object_kind != 'clipboard' OR event_type IN ('created', 'deleted')),
                CONSTRAINT fk_event_log_user_id FOREIGN KEY (user_id) REFERENCES users (id)
                    ON DELETE CASCADE ON UPDATE CASCADE
             );
             INSERT INTO event_log SELECT seq, user_id, event_type, object_kind, object_id, created_at FROM event_log_old;
             DROP TABLE event_log_old;
             CREATE INDEX idx_event_log_user_seq ON event_log (user_id, seq);
             CREATE INDEX idx_event_log_created_at ON event_log (created_at);",
        ).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
