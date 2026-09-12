//! Split an object's identity from its content, so content can have a history.
//!
//! Until now an object *was* a row: one id, one sealed blob, no way to change
//! it. Editing meant creating a new object and deleting the old one, which
//! works but loses the fact that the two are the same thing. This migration
//! makes an object a stable id with a chain of revisions behind it, each its
//! own immutable, individually-signed envelope.
//!
//! So `objects` keeps only what does not change — id, owner, kind — plus a
//! pointer at the head of the chain, and all content moves to
//! `object_revisions`. Payloads hang off a revision rather than an object,
//! because two revisions of the same file are two different ciphertexts.
//!
//! **This migration destroys every encrypted object.** Collab documents
//! survive — see the note beside their re-insert below.
//!
//! There is no copy step for encrypted objects: the envelope body gained
//! `revision` and `parent_hash`, and postcard encodes positionally, so no
//! existing row can be read back by the new client anyway. Rewriting them was
//! not possible either, since only a client holding the user's key can sign an
//! envelope. Collab documents, users, devices and access keys are preserved.
//!
//! Three columns on `objects` are denormalised from the chain —
//! `head_revision`, `published_seq` and `deleted_at`. They exist because every
//! listing query would otherwise need an aggregate over `object_revisions` to
//! find the newest row. They can only be written by `advance_object_head` in
//! `routes/objects.rs`, which is the one place that moves a chain forward.
//!
//! `objects.status` is gone, replaced by `published_seq IS NULL`. A pending
//! upload is now a pending *revision*, which is where the status belongs: a
//! failed edit leaves the object alone and only its half-written revision to
//! sweep.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Order matters: children first, since each holds an FK into the next.
        db.execute_unprepared("DROP TABLE IF EXISTS object_payloads")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS objects")
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
                CHECK (kind IN ('clipboard', 'file', 'collab', 'schedule')),
                CHECK (head_revision IS NULL OR head_revision >= 1),
                -- `published_seq` is the sync watermark at which this object's
                -- current state became visible, and NULL means not yet visible
                -- — which is what the old `status` column said. For a
                -- revisioned object it is the head revision's `created_seq`;
                -- for a collab object, whose content is a Y-doc with its own
                -- versioning and so has no revisions at all, it is stamped once
                -- at creation. Keeping one column for both is what lets the
                -- watermark query stay a single scan over `objects`.
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

        // The CHECKs here mirror `validate_revision_link` in `crates/api-types`
        // deliberately. That one refuses a malformed envelope at the HTTP
        // boundary; this one refuses a malformed row whatever the path in, so a
        // future route cannot write a chain the format forbids.
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

        // Collab documents are the one kind the envelope break does not touch.
        // Their content is a server-visible Y-doc, not ciphertext, so there is
        // no reason to throw them away with everything else. They keep their
        // rows and get fresh identity rows here.
        //
        // Fresh, because the original object ids only ever existed in the table
        // just dropped: `collab_docs` never stored one, and the `event_log`
        // rows that mention them cannot be matched back to a document. Clients
        // will see new ids, which costs nothing when every client is resyncing
        // from scratch anyway. `published_seq` is derived from the document's
        // creation time in the same microsecond space the allocator uses, plus
        // the rowid so two documents created in the same second cannot collide.
        db.execute_unprepared(
            "INSERT INTO objects (
                id, user_id, kind, created_at, updated_at, expires_at,
                head_revision, published_seq, deleted_at, collab_doc_id
            )
            SELECT
                lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4'
                    || substr(lower(hex(randomblob(2))), 2) || '-a'
                    || substr(lower(hex(randomblob(2))), 2) || '-'
                    || lower(hex(randomblob(6))),
                owner_user_id,
                'collab',
                created_at,
                updated_at,
                NULL,
                NULL,
                CAST(strftime('%s', created_at) AS INTEGER) * 1000000 + rowid,
                NULL,
                id
            FROM collab_docs",
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

        // Listing walks objects in published_seq order within a user and kind,
        // and skips tombstones, so the index leads with exactly that.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_objects_user_kind_published_seq
                ON objects (user_id, kind, published_seq, id)",
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
        // The orphan sweep looks for revisions left pending past a TTL, across
        // every object, which the (object_id, revision) primary key cannot
        // serve.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_object_revisions_status_stored_at
                ON object_revisions (status, stored_at)",
        )
        .await?;
        // `object_payloads` needs no index of its own: its primary key leads
        // with the lookup columns, and UNIQUE(ciphertext_path) is indexed by
        // the table definition.

        // `updated` stops being collab's private event. Publishing a revision
        // changes which ciphertext is current, and that is exactly what an
        // `updated` event is for; before this migration an encrypted object could
        // only be created or deleted. Clipboard still cannot be revised: it
        // expires passively and is replaced rather than revised.
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
                    OR object_kind IN ('file', 'collab', 'schedule')
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
        // Rebuilt with the table above: `cleanup_old_events` filters on
        // `created_at`, which would otherwise scan the whole log.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_event_log_created_at
                ON event_log (created_at)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        // Not implemented. Reversing this would mean folding a chain of
        // revisions back into a single row and re-signing every envelope as
        // v1. No server can do that: signing needs a key only the client
        // holds. Recreating the database is the supported path back.
        Err(DbErr::Migration(
            "m20260908_000005_object_revisions is irreversible: recreate the database instead"
                .to_string(),
        ))
    }
}
