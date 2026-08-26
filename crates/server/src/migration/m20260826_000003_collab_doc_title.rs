//! Give collab docs a renameable plaintext `title`, and admit `updated` events.
//!
//! The title is a column rather than a field inside the Y.Doc because the doc
//! list has to render titles without opening a Y-sync WebSocket per document.
//! That costs nothing in confidentiality: a collab doc's whole content is
//! already server-visible plaintext (see `m20260615_000002_collab_docs`), which
//! is what makes it the one object kind the server can relay CRDT updates for.
//!
//! A rename is the first mutation of a *live* object, so `event_log` gains an
//! `updated` event type — that is how other devices learn a doc was renamed,
//! since the encrypted-object snapshot never lists collab objects. SQLite cannot
//! widen a CHECK constraint in place, so `event_log` is rebuilt by the same
//! rename/recreate/copy/drop dance the previous migration used. Nothing
//! references `event_log`, so its rename needs no child-table FK fixup.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Existing rows become untitled; clients render their own placeholder for
        // an empty title, so no backfill value is needed.
        db.execute_unprepared("ALTER TABLE collab_docs ADD COLUMN title TEXT NOT NULL DEFAULT ''")
            .await?;

        db.execute_unprepared("ALTER TABLE event_log RENAME TO event_log_old")
            .await?;

        // Only collab docs can be updated in place: clipboard and file objects
        // are immutable ciphertext, so their lifecycle stays create/delete.
        db.execute_unprepared(
            "CREATE TABLE event_log (
                seq BIGINT NOT NULL PRIMARY KEY,
                user_id UUID NOT NULL,
                event_type TEXT NOT NULL,
                object_kind TEXT NOT NULL,
                object_id UUID NOT NULL,
                created_at TEXT NOT NULL,
                CHECK (event_type IN ('created', 'updated', 'deleted')),
                CHECK (object_kind IN ('clipboard', 'file', 'collab')),
                CHECK (
                    event_type = 'created'
                    OR (event_type = 'deleted' AND object_kind IN ('file', 'collab'))
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

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop the column; leave the widened `event_log` checks in place. They
        // still admit every row a pre-rename server could have written, so
        // existing data stays valid (see CLAUDE.md: no deployment to reverse).
        manager
            .alter_table(
                Table::alter()
                    .table(CollabDocs::Table)
                    .drop_column(CollabDocs::Title)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum CollabDocs {
    Table,
    Title,
}
