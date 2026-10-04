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

        db.execute_unprepared("ALTER TABLE object_payloads RENAME TO object_payloads_old")
            .await?;
        db.execute_unprepared("ALTER TABLE object_revisions RENAME TO object_revisions_old")
            .await?;
        db.execute_unprepared("ALTER TABLE objects RENAME TO objects_old")
            .await?;

        db.execute_unprepared(
            "CREATE TABLE objects (
                id UUID NOT NULL PRIMARY KEY,
                user_id UUID NOT NULL,
                kind TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                expires_at TEXT,
                head_revision BIGINT,
                published_seq BIGINT,
                deleted_at TEXT,
                collab_doc_id UUID,
                CHECK (kind IN ('clipboard', 'file', 'collab', 'schedule', 'app_document')),
                CHECK (head_revision IS NULL OR head_revision >= 1),
                CHECK (head_revision IS NULL OR published_seq IS NOT NULL),
                CHECK (deleted_at IS NULL OR published_seq IS NOT NULL),
                CHECK (collab_doc_id IS NULL OR head_revision IS NULL),
                CONSTRAINT fk_objects_user_id
                    FOREIGN KEY (user_id) REFERENCES users (id)
                    ON DELETE CASCADE ON UPDATE CASCADE,
                CONSTRAINT fk_objects_collab_doc_id
                    FOREIGN KEY (collab_doc_id) REFERENCES collab_docs (id)
                    ON DELETE CASCADE ON UPDATE CASCADE
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE object_revisions (
                object_id UUID NOT NULL,
                revision BIGINT NOT NULL,
                operation TEXT NOT NULL,
                parent_hash BLOB,
                meta_ciphertext BLOB NOT NULL,
                meta_nonce BLOB NOT NULL,
                envelope BLOB NOT NULL,
                source_device_id UUID,
                created_at TEXT NOT NULL,
                stored_at TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                created_seq BIGINT,
                CHECK (revision >= 1),
                CHECK (operation IN ('create', 'revise', 'delete')),
                CHECK ((revision = 1) = (parent_hash IS NULL)),
                CHECK ((revision = 1) = (operation = 'create')),
                CHECK (status IN ('pending', 'complete')),
                CHECK (status <> 'complete' OR created_seq IS NOT NULL),
                CONSTRAINT pk_object_revisions PRIMARY KEY (object_id, revision),
                CONSTRAINT fk_object_revisions_object_id
                    FOREIGN KEY (object_id) REFERENCES objects (id)
                    ON DELETE CASCADE ON UPDATE CASCADE,
                CONSTRAINT fk_object_revisions_source_device_id
                    FOREIGN KEY (source_device_id) REFERENCES devices (id)
                    ON DELETE SET NULL ON UPDATE CASCADE
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE object_payloads (
                object_id UUID NOT NULL,
                revision BIGINT NOT NULL,
                payload_id UUID NOT NULL,
                ciphertext_path TEXT NOT NULL UNIQUE,
                nonce BLOB NOT NULL,
                ciphertext_size BIGINT NOT NULL,
                sha256_ciphertext BLOB NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                CHECK (ciphertext_size >= 0),
                CHECK (status IN ('pending', 'uploading', 'uploaded', 'complete')),
                CONSTRAINT pk_object_payloads PRIMARY KEY (object_id, revision, payload_id),
                CONSTRAINT fk_object_payloads_revision
                    FOREIGN KEY (object_id, revision)
                    REFERENCES object_revisions (object_id, revision)
                    ON DELETE CASCADE ON UPDATE CASCADE
            )",
        )
        .await?;

        db.execute_unprepared(
            "INSERT INTO objects (
                id, user_id, kind, created_at, updated_at, expires_at,
                head_revision, published_seq, deleted_at, collab_doc_id
            )
            SELECT
                id, user_id, kind, created_at, updated_at, expires_at,
                head_revision, published_seq, deleted_at, collab_doc_id
            FROM objects_old",
        )
        .await?;
        db.execute_unprepared(
            "INSERT INTO object_revisions (
                object_id, revision, operation, parent_hash, meta_ciphertext,
                meta_nonce, envelope, source_device_id, created_at, stored_at,
                status, created_seq
            )
            SELECT
                object_id, revision, operation, parent_hash, meta_ciphertext,
                meta_nonce, envelope, source_device_id, created_at, stored_at,
                status, created_seq
            FROM object_revisions_old",
        )
        .await?;
        db.execute_unprepared(
            "INSERT INTO object_payloads (
                object_id, revision, payload_id, ciphertext_path, nonce,
                ciphertext_size, sha256_ciphertext, created_at, updated_at, status
            )
            SELECT
                object_id, revision, payload_id, ciphertext_path, nonce,
                ciphertext_size, sha256_ciphertext, created_at, updated_at, status
            FROM object_payloads_old",
        )
        .await?;

        db.execute_unprepared("DROP TABLE object_payloads_old")
            .await?;
        db.execute_unprepared("DROP TABLE object_revisions_old")
            .await?;
        db.execute_unprepared("DROP TABLE objects_old").await?;

        db.execute_unprepared(
            "CREATE INDEX idx_objects_user_kind_published_seq
                ON objects (user_id, kind, published_seq, id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_objects_kind_user_created_at
                ON objects (kind, user_id, created_at)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_objects_kind_expires_at
                ON objects (kind, expires_at)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_objects_collab_doc_id
                ON objects (collab_doc_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_object_revisions_status_stored_at
                ON object_revisions (status, stored_at)",
        )
        .await?;

        db.execute_unprepared("ALTER TABLE event_log RENAME TO event_log_old")
            .await?;
        db.execute_unprepared(
            "CREATE TABLE event_log (
                seq BIGINT NOT NULL PRIMARY KEY,
                user_id UUID NOT NULL,
                event_type TEXT NOT NULL,
                object_kind TEXT NOT NULL,
                object_id UUID NOT NULL,
                created_at TEXT NOT NULL,
                CHECK (event_type IN ('created', 'updated', 'deleted')),
                CHECK (
                    object_kind IN ('clipboard', 'file', 'collab', 'schedule', 'app_document')
                ),
                CHECK (
                    event_type = 'created'
                    OR object_kind IN ('file', 'collab', 'schedule', 'app_document')
                ),
                CONSTRAINT fk_event_log_user_id
                    FOREIGN KEY (user_id) REFERENCES users (id)
                    ON DELETE CASCADE ON UPDATE CASCADE
            )",
        )
        .await?;
        db.execute_unprepared(
            "INSERT INTO event_log (
                seq, user_id, event_type, object_kind, object_id, created_at
            )
            SELECT seq, user_id, event_type, object_kind, object_id, created_at
            FROM event_log_old",
        )
        .await?;
        db.execute_unprepared("DROP TABLE event_log_old").await?;
        db.execute_unprepared(
            "CREATE INDEX idx_event_log_user_seq
                ON event_log (user_id, seq)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_event_log_created_at
                ON event_log (created_at)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    use crate::migration::Migrator;

    async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
        db.query_one_raw(Statement::from_string(DbBackend::Sqlite, sql))
            .await
            .expect("count query")
            .expect("count row")
            .try_get_by_index(0)
            .expect("count value")
    }

    #[tokio::test]
    async fn stored_objects_survive_and_app_documents_are_accepted() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("database");
        Migrator::up(&db, Some(6)).await.expect("migrations 1 to 6");
        db.execute_unprepared(
            "INSERT INTO access_keys (key_hash, created_at) VALUES ('key', 'now');
             INSERT INTO users (id, username, opaque_password_file, encryption_salt,
                 access_key_hash, created_at, updated_at)
                 VALUES (x'01', 'owner', x'00', x'00', 'key', 'now', 'now');
             INSERT INTO devices (id, user_id, name, platform, signing_public_key,
                 created_at, updated_at, last_seen_at)
                 VALUES (x'02', x'01', 'mac', 'macos', x'00', 'now', 'now', 'now');
             INSERT INTO objects (id, user_id, kind, created_at, updated_at,
                 head_revision, published_seq)
                 VALUES (x'10', x'01', 'schedule', 'now', 'now', 2, 200),
                        (x'11', x'01', 'file', 'now', 'now', 1, 300);
             INSERT INTO object_revisions VALUES
                 (x'10', 1, 'create', NULL, x'aa', x'bb', x'cc', x'02', 'now', 'now', 'complete', 100),
                 (x'10', 2, 'revise', x'dd', x'aa', x'bb', x'cc', x'02', 'now', 'now', 'complete', 200),
                 (x'11', 1, 'create', NULL, x'aa', x'bb', x'cc', x'02', 'now', 'now', 'complete', 300);
             INSERT INTO object_payloads VALUES
                 (x'10', 1, x'20', 'first', x'00', 5, x'00', 'now', 'now', 'complete'),
                 (x'10', 2, x'21', 'second', x'00', 5, x'00', 'now', 'now', 'complete'),
                 (x'11', 1, x'22', 'third', x'00', 5, x'00', 'now', 'now', 'complete');
             INSERT INTO event_log VALUES
                 (100, x'01', 'created', 'schedule', x'10', 'now'),
                 (200, x'01', 'updated', 'schedule', x'10', 'now');",
        )
        .await
        .expect("seed rows");

        Migrator::up(&db, None).await.expect("migration 7");

        assert_eq!(count(&db, "SELECT count(*) FROM objects").await, 2);
        assert_eq!(count(&db, "SELECT count(*) FROM object_revisions").await, 3);
        assert_eq!(count(&db, "SELECT count(*) FROM object_payloads").await, 3);
        assert_eq!(count(&db, "SELECT count(*) FROM event_log").await, 2);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name IN (
                     'idx_objects_user_kind_published_seq', 'idx_objects_kind_user_created_at',
                     'idx_objects_kind_expires_at', 'idx_objects_collab_doc_id',
                     'idx_object_revisions_status_stored_at', 'idx_event_log_user_seq',
                     'idx_event_log_created_at')"
            )
            .await,
            7
        );
        assert_eq!(
            count(&db, "SELECT count(*) FROM pragma_foreign_key_check").await,
            0
        );

        db.execute_unprepared("DELETE FROM objects WHERE id = x'10'")
            .await
            .expect("delete an object");
        assert_eq!(count(&db, "SELECT count(*) FROM object_revisions").await, 1);
        assert_eq!(count(&db, "SELECT count(*) FROM object_payloads").await, 1);

        db.execute_unprepared(
            "INSERT INTO objects (id, user_id, kind, created_at, updated_at)
                 VALUES (x'12', x'01', 'app_document', 'now', 'now');
             INSERT INTO event_log VALUES (400, x'01', 'deleted', 'app_document', x'12', 'now');",
        )
        .await
        .expect("an app document and its delete event");
    }
}
