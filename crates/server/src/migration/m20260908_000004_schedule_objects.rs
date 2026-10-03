//! Admit `schedule` as an object kind, and let schedule objects be deleted.
//!
//! Schedule records — a series definition, a single-occurrence override, or a
//! log of time actually spent — all share this one kind. Splitting them would
//! duplicate routing and storage plumbing and let the server distinguish a
//! plan from a record of what actually happened. The discriminant lives in the encrypted
//! meta instead, so the server sees only that a schedule object exists.
//!
//! At this migration stage, editing a plan creates a replacement object and
//! deletes the old one. Therefore `schedule`
//! joins `file` and `collab` as a kind that admits a `deleted` event. It does
//! not admit `updated`: that stays collab-only, because a schedule object is
//! immutable ciphertext like clipboard and file.
//!
//! SQLite cannot widen a CHECK constraint in place, so both tables go through
//! the same rename/recreate/copy/drop dance the previous two migrations used.
//! Renaming `objects` repoints `object_payloads`' foreign key at the renamed
//! table, so that one is rebuilt afterwards to re-emit its FK, the same way
//! `m20260615_000002_collab_docs` does.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE objects RENAME TO objects_old")
            .await?;

        db.execute_unprepared(
            "CREATE TABLE objects (
                id UUID NOT NULL PRIMARY KEY,
                user_id UUID NOT NULL,
                kind TEXT NOT NULL,
                meta_ciphertext BLOB,
                meta_nonce BLOB,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                expires_at TEXT,
                source_device_id UUID,
                envelope BLOB,
                status TEXT NOT NULL DEFAULT 'pending',
                created_seq BIGINT,
                collab_doc_id UUID,
                CHECK (kind IN ('clipboard', 'file', 'collab', 'schedule')),
                CHECK (status IN ('pending', 'complete')),
                CHECK (status <> 'complete' OR created_seq IS NOT NULL),
                CHECK (
                    (meta_ciphertext IS NOT NULL AND meta_nonce IS NOT NULL
                        AND envelope IS NOT NULL AND collab_doc_id IS NULL)
                    OR
                    (meta_ciphertext IS NULL AND meta_nonce IS NULL
                        AND envelope IS NULL AND collab_doc_id IS NOT NULL)
                ),
                CONSTRAINT fk_objects_source_device_id
                    FOREIGN KEY (source_device_id) REFERENCES devices (id)
                    ON DELETE SET NULL ON UPDATE CASCADE,
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
            "INSERT INTO objects (
                id, user_id, kind, meta_ciphertext, meta_nonce, created_at,
                updated_at, expires_at, source_device_id, envelope, status,
                created_seq, collab_doc_id
            )
            SELECT
                id, user_id, kind, meta_ciphertext, meta_nonce, created_at,
                updated_at, expires_at, source_device_id, envelope, status,
                created_seq, collab_doc_id
            FROM objects_old",
        )
        .await?;

        db.execute_unprepared("DROP TABLE objects_old").await?;

        // The rename above repointed `object_payloads`' FK at `objects_old`,
        // which no longer exists. Rebuild it identically so the FK is re-emitted
        // against the new table.
        db.execute_unprepared("ALTER TABLE object_payloads RENAME TO object_payloads_old")
            .await?;

        db.execute_unprepared(
            "CREATE TABLE object_payloads (
                object_id UUID NOT NULL,
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
                CONSTRAINT pk_object_payloads PRIMARY KEY (object_id, payload_id),
                CONSTRAINT fk_object_payloads_object_id
                    FOREIGN KEY (object_id) REFERENCES objects (id)
                    ON DELETE CASCADE ON UPDATE CASCADE
            )",
        )
        .await?;

        db.execute_unprepared(
            "INSERT INTO object_payloads (
                object_id, payload_id, ciphertext_path, nonce, ciphertext_size,
                sha256_ciphertext, created_at, updated_at, status
            )
            SELECT
                object_id, payload_id, ciphertext_path, nonce, ciphertext_size,
                sha256_ciphertext, created_at, updated_at, status
            FROM object_payloads_old",
        )
        .await?;

        db.execute_unprepared("DROP TABLE object_payloads_old")
            .await?;

        // Recreate every index the dropped tables carried.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_objects_user_kind_status_created_seq_id
                ON objects (user_id, kind, status, created_seq, id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_objects_kind_user_status_created_at
                ON objects (kind, user_id, status, created_at)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_objects_kind_user_created_at
                ON objects (kind, user_id, created_at)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_objects_kind_expires_at
                ON objects (kind, expires_at)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_objects_collab_doc_id
                ON objects (collab_doc_id)",
        )
        .await?;
        // `object_payloads` needs no index of its own: its primary key
        // (object_id, payload_id) already leads with the lookup column, and the
        // UNIQUE on ciphertext_path is indexed by the table definition.

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
                CHECK (object_kind IN ('clipboard', 'file', 'collab', 'schedule')),
                CHECK (
                    event_type = 'created'
                    OR (event_type = 'deleted'
                        AND object_kind IN ('file', 'collab', 'schedule'))
                    OR (event_type = 'updated' AND object_kind = 'collab')
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
            "CREATE INDEX IF NOT EXISTS idx_event_log_user_seq
                ON event_log (user_id, seq)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_event_log_created_at
                ON event_log (created_at)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Nothing to reverse. The widened checks still admit every row an older
        // server could have written, so narrowing them would only risk
        // rejecting valid data (see CLAUDE.md on schema compatibility).
        Ok(())
    }
}
