//! Add the `collab_docs` table and make `objects` carry either an encrypted
//! payload or a collab-doc pointer (never both).
//!
//! Collab docs are the one server-visible object kind: the server applies and
//! relays Y-CRDT updates, so `collab_docs.yjs_state` is plaintext (null until
//! the first Y-sync edit in Phase 3). Clipboard and file objects keep their
//! end-to-end-encrypted `meta_ciphertext`/`meta_nonce`/`envelope`; a collab
//! object leaves all three null and points at a `collab_docs` row instead.
//!
//! SQLite cannot drop a NOT NULL constraint with `ALTER COLUMN`, so the three
//! object ciphertext columns are made nullable by the standard table-rebuild
//! (rename, recreate, copy, drop). `event_log` is rebuilt the same way to widen
//! its `object_kind` and `deleted` check constraints to admit `'collab'`. This
//! project has no deployment to migrate (see CLAUDE.md), so the rebuild simply
//! carries existing rows forward and `down()` only drops the new table.
//!
//! Important SQLite gotcha handled below: `ALTER TABLE objects RENAME TO ...`
//! rewrites any *child* table's foreign key to follow the new name (default
//! `legacy_alter_table = OFF`). `object_payloads.object_id` references
//! `objects`, so the rename repoints that FK at `objects_old`, which is then
//! dropped — leaving a dangling reference that fails every later payload insert.
//! `PRAGMA legacy_alter_table`/`foreign_keys` are unreliable here because
//! SeaORM runs all migrations inside one transaction (where `foreign_keys` is a
//! no-op) over a pooled SQLite connection. So instead `object_payloads` is
//! rebuilt right after `objects`, which re-emits its FK pointing at the new
//! `objects` table deterministically, with no reliance on pragmas.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS collab_docs (
                id UUID NOT NULL PRIMARY KEY,
                owner_user_id UUID NOT NULL,
                share_token TEXT NOT NULL UNIQUE,
                yjs_state BLOB,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                CONSTRAINT fk_collab_docs_owner_user_id
                    FOREIGN KEY (owner_user_id) REFERENCES users (id)
                    ON DELETE CASCADE ON UPDATE CASCADE
            )",
        )
        .await?;

        // Rebuild `objects` so the ciphertext columns become nullable and a
        // nullable `collab_doc_id` FK is added, gated by an XOR check: a row is
        // either an encrypted object (ciphertext set, collab_doc_id null) or a
        // collab object (ciphertext null, collab_doc_id set).
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
                CHECK (kind IN ('clipboard', 'file', 'collab')),
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

        // Carry every existing row forward. Pre-migration rows are all encrypted
        // objects (clipboard/file), so `collab_doc_id` is null for all of them.
        db.execute_unprepared(
            "INSERT INTO objects (
                id, user_id, kind, meta_ciphertext, meta_nonce, created_at,
                updated_at, expires_at, source_device_id, envelope, status,
                created_seq, collab_doc_id
            )
            SELECT
                id, user_id, kind, meta_ciphertext, meta_nonce, created_at,
                updated_at, expires_at, source_device_id, envelope, status,
                created_seq, NULL
            FROM objects_old",
        )
        .await?;

        db.execute_unprepared("DROP TABLE objects_old").await?;

        // The `objects` rename repointed `object_payloads`' FK at `objects_old`
        // (now dropped). Rebuild `object_payloads` identically so its FK is
        // re-emitted against the new `objects` table. Nothing references
        // `object_payloads`, so this rename is self-contained.
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

        // Recreate the indexes that existed on `objects` (dropped with the old
        // table) plus a lookup index for collab doc joins.
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

        // Rebuild `event_log` so `object_kind` and the cross-column `deleted`
        // check admit `'collab'`. Clipboard items still expire passively and
        // never emit `deleted`, so only file and collab deletes are allowed.
        // Nothing references `event_log`, so its rename needs no child fixup.
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
                CHECK (event_type IN ('created', 'deleted')),
                CHECK (object_kind IN ('clipboard', 'file', 'collab')),
                CHECK (
                    event_type = 'created'
                    OR (event_type = 'deleted' AND object_kind IN ('file', 'collab'))
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

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // No deployment to reverse (see CLAUDE.md): drop the new table and leave
        // the widened `objects`/`event_log` definitions in place. The XOR check
        // still admits every pre-collab row, so existing data stays valid.
        manager
            .drop_table(
                Table::drop()
                    .table(CollabDocs::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum CollabDocs {
    Table,
}
