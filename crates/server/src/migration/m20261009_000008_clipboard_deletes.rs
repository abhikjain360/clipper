use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if db
            .query_one_raw(sea_orm::Statement::from_string(
                db.get_database_backend(),
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'event_log_old'",
            ))
            .await?
            .is_some()
        {
            manager
                .get_connection()
                .execute_unprepared(
                    "DROP TABLE IF EXISTS event_log;
                 ALTER TABLE event_log_old RENAME TO event_log;",
                )
                .await?;
        }
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

#[cfg(test)]
mod tests {
    use sea_orm::{Database, DbBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    use super::*;
    use crate::migration::Migrator;

    async fn seed() -> sea_orm::DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migrator::up(&db, Some(7)).await.unwrap();
        db.execute_unprepared("INSERT INTO access_keys (key_hash, created_at) VALUES ('key', 'now');
            INSERT INTO users (id, username, opaque_password_file, encryption_salt, access_key_hash, created_at, updated_at)
            VALUES (x'01', 'owner', x'00', x'00', 'key', 'now', 'now');
            INSERT INTO event_log VALUES (10, x'01', 'created', 'clipboard', x'02', 'now');").await.unwrap();
        db
    }

    async fn count(db: &sea_orm::DatabaseConnection, sql: &str) -> i64 {
        db.query_one_raw(Statement::from_string(DbBackend::Sqlite, sql))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap()
    }

    #[tokio::test]
    async fn clipboard_delete_migration_recovers_each_partial_rebuild() {
        for stage in 0..3 {
            let db = seed().await;
            db.execute_unprepared("ALTER TABLE event_log RENAME TO event_log_old")
                .await
                .unwrap();
            if stage > 0 {
                db.execute_unprepared(
                    "CREATE TABLE event_log AS SELECT * FROM event_log_old WHERE 0",
                )
                .await
                .unwrap();
            }
            if stage > 1 {
                db.execute_unprepared("INSERT INTO event_log SELECT * FROM event_log_old")
                    .await
                    .unwrap();
            }
            Migrator::up(&db, None).await.unwrap();
            assert_eq!(
                count(&db, "SELECT count(*) FROM event_log WHERE seq = 10").await,
                1
            );
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) FROM sqlite_master WHERE name = 'event_log_old'"
                )
                .await,
                0
            );
            db.execute_unprepared(
                "INSERT INTO event_log VALUES (20, x'01', 'deleted', 'clipboard', x'02', 'now')",
            )
            .await
            .unwrap();
            Migrator::up(&db, None).await.unwrap();
            assert_eq!(count(&db, "SELECT count(*) FROM event_log").await, 2);
        }
    }

    #[tokio::test]
    async fn clipboard_delete_migration_rolls_back_a_failure_and_can_retry() {
        let db = seed().await;
        db.execute_unprepared(
            "DROP INDEX idx_event_log_created_at;
            CREATE TABLE conflict (value TEXT);
            CREATE INDEX idx_event_log_created_at ON conflict (value);",
        )
        .await
        .unwrap();
        assert!(Migrator::up(&db, None).await.is_err());
        assert_eq!(
            count(&db, "SELECT count(*) FROM event_log WHERE seq = 10").await,
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM sqlite_master WHERE name = 'event_log_old'"
            )
            .await,
            0
        );
        db.execute_unprepared("DROP INDEX idx_event_log_created_at")
            .await
            .unwrap();
        Migrator::up(&db, None).await.unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM event_log").await, 1);
    }
}
