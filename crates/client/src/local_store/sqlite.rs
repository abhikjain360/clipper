//! SQLite backing for the native local store.
//!
//! Two tables with different lifetimes (`docs/local-store-plan.md`, S2):
//!
//! - `objects`, with `object_payloads` hanging off it, is a cache. Every row
//!   can be dropped and downloaded again.
//! - `object_anchors` is the ledger of what this device has already accepted.
//!   Nothing the server holds can reconstruct it, so a lost row there reduces
//!   rollback protection.
//!
//! They are separate tables so that a cache wipe cannot reach an anchor:
//! `DELETE FROM objects` drops the whole cache and leaves every anchor.
//!
//! Calls here block. They are single-file, single-connection statements over
//! indexed rows, so the work is microseconds and a `spawn_blocking` hop per
//! call would cost more than it saves. The enclosing methods stay `async`
//! because the browser implementation of the same boundary is.

use std::{path::Path, str::FromStr};

use clipper_core::{crypto, models::ObjectKind};
use rusqlite::{Connection, OptionalExtension, params};

use super::{
    LocalHead, LocalStoreError, StoredObjectRecord, StoredPresentContent,
    StoredPresentObjectRecord, StoredRevisionAnchor, StoredRevisionAnchorKind,
    StoredSyncMarkerRecord, present_revision_anchor,
};

/// Bumped whenever the tables below change shape.
///
/// A mismatch recreates the database rather than migrating it, because the
/// cache is worth nothing and the project keeps no local compatibility. The
/// anchors go with it, and that is the real loss: a schema change that expects
/// to keep them has to carry the `object_anchors` rows across instead of
/// taking this path.
const SCHEMA_VERSION: i32 = 1;

const SCHEMA: &str = "
CREATE TABLE objects (
    id              TEXT    PRIMARY KEY NOT NULL,
    kind            TEXT    NOT NULL,
    -- 1 while a fetch is outstanding and `content` is still NULL.
    pending         INTEGER NOT NULL,
    seen_generation INTEGER,
    event_seq       INTEGER NOT NULL,
    created_seq     INTEGER NOT NULL,
    content         BLOB
) STRICT;

-- Reconciliation works one kind at a time and asks about creation order;
-- without this it degrades into a scan of every object being held.
CREATE INDEX objects_by_kind ON objects (kind, created_seq DESC);

CREATE TABLE object_payloads (
    object_id  TEXT PRIMARY KEY NOT NULL REFERENCES objects (id) ON DELETE CASCADE,
    ciphertext BLOB NOT NULL
) STRICT;

CREATE TABLE object_anchors (
    object_id       TEXT    PRIMARY KEY NOT NULL,
    kind            TEXT    NOT NULL,
    event_seq       INTEGER NOT NULL,
    created_seq     INTEGER NOT NULL,
    seen_generation INTEGER,
    -- NULL together: a delete for an object this device never held leaves an
    -- ordering guard with no chain position to retain.
    anchor_kind     TEXT,
    revision        INTEGER,
    parent_hash     BLOB
) STRICT;
";

/// Open (creating if needed) the store database at `path`.
///
/// The file is created restricted before SQLite ever sees it: SQLite copies
/// the main database's mode onto the `-wal` and `-shm` sidecars, which hold
/// the same ciphertext, so a permissive mode here would leak through them.
pub(super) fn open(path: &Path) -> Result<Connection, LocalStoreError> {
    create_private_file_if_missing(path)?;
    let connection = Connection::open(path)?;
    // WAL for the reader-during-write case; FULL because the anchors are the
    // point and the file store this replaces fsynced every record it wrote,
    // so anything looser would be a quiet durability regression.
    //
    // A filesystem that cannot do WAL leaves the journal mode as it was rather
    // than failing. That is still correct, only slower, so it is worth saying
    // out loud and not worth refusing to start over.
    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        tracing::warn!(
            journal_mode = %journal_mode,
            "The local store could not use a write-ahead log"
        );
    }
    connection.execute_batch("PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON;")?;

    let version: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        if version != 0 {
            tracing::info!(
                from = version,
                to = SCHEMA_VERSION,
                "Recreating the local store for a new schema"
            );
        }
        connection.execute_batch(
            "DROP TABLE IF EXISTS object_payloads;
             DROP TABLE IF EXISTS object_anchors;
             DROP TABLE IF EXISTS objects;",
        )?;
        connection.execute_batch(SCHEMA)?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(connection)
}

#[cfg(unix)]
fn create_private_file_if_missing(path: &Path) -> Result<(), LocalStoreError> {
    use std::os::unix::fs::OpenOptionsExt;

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn create_private_file_if_missing(_path: &Path) -> Result<(), LocalStoreError> {
    Ok(())
}

/// The state this device holds for one object, whether that is cached content,
/// an outstanding fetch, or only the memory of what it accepted.
pub(super) fn read_record(
    connection: &Connection,
    object_id: &str,
) -> Result<Option<StoredObjectRecord>, LocalStoreError> {
    let live = connection
        .query_row(
            "SELECT kind, pending, seen_generation, event_seq, created_seq, content
             FROM objects WHERE id = ?1",
            params![object_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                ))
            },
        )
        .optional()?;

    let Some((kind, pending, seen_generation, event_seq, created_seq, content)) = live else {
        return read_anchor_row(connection, object_id)
            .map(|marker| marker.map(StoredObjectRecord::Deleted));
    };

    let kind = object_kind(&kind)?;
    let seen_generation = seen_generation.map(|generation| generation as u64);
    if pending {
        return Ok(Some(StoredObjectRecord::PendingCreate(
            StoredSyncMarkerRecord {
                id: object_id.to_string(),
                kind,
                seen_generation,
                event_seq,
                created_seq,
                revision_anchor: read_anchor_row(connection, object_id)?
                    .and_then(|marker| marker.revision_anchor),
            },
        )));
    }

    let Some(content) = content else {
        return unreadable_cache_row(connection, object_id, "the row holds no content");
    };
    match serde_json::from_slice::<StoredPresentContent>(&content) {
        Ok(content) => Ok(Some(StoredObjectRecord::Present(Box::new(
            StoredPresentObjectRecord {
                id: object_id.to_string(),
                kind,
                seen_generation,
                event_seq,
                created_seq,
                content,
            },
        )))),
        Err(error) => unreadable_cache_row(
            connection,
            object_id,
            &format!("the row's content will not parse: {error}"),
        ),
    }
}

/// A held row this device can no longer read.
///
/// The content is a cache and goes; the anchor is not and stays, so the object
/// reads back as an absent one: the same head may reappear, the sweep carries
/// on, and the next fetch replaces the row. Failing here instead would make
/// every path that touches the id fail for good: persist, sweep, head read and
/// revision check.
fn unreadable_cache_row(
    connection: &Connection,
    object_id: &str,
    reason: &str,
) -> Result<Option<StoredObjectRecord>, LocalStoreError> {
    tracing::warn!(object_id = %object_id, "Discarding an unreadable local cache row: {reason}");
    match read_anchor_row(connection, object_id)? {
        Some(marker) => Ok(Some(StoredObjectRecord::Deleted(marker))),
        None => {
            connection.execute("DELETE FROM objects WHERE id = ?1", params![object_id])?;
            Ok(None)
        }
    }
}

/// Every object this device currently holds or is still fetching.
///
/// Not every row. An object that is gone leaves an anchor and nothing else,
/// and neither hydration nor reconciliation has any use for one.
pub(super) fn live_records(
    connection: &Connection,
) -> Result<Vec<StoredObjectRecord>, LocalStoreError> {
    let mut statement = connection.prepare(
        "SELECT o.id, o.kind, o.pending, o.seen_generation, o.event_seq, o.created_seq, o.content,
                a.anchor_kind, a.revision, a.parent_hash
         FROM objects o LEFT JOIN object_anchors a ON a.object_id = o.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, bool>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, Option<Vec<u8>>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, Option<Vec<u8>>>(9)?,
        ))
    })?;

    let mut records = Vec::new();
    for row in rows {
        let (
            id,
            kind,
            pending,
            seen_generation,
            event_seq,
            created_seq,
            content,
            anchor_kind,
            revision,
            parent_hash,
        ) = row?;
        let seen_generation = seen_generation.map(|generation| generation as u64);
        // One damaged row must cost that object, not the hydration: a hard
        // error here leaves the client showing nothing at all.
        let kind = match object_kind(&kind) {
            Ok(kind) => kind,
            Err(error) => {
                tracing::warn!(object_id = %id, "Skipping a local cache row: {error}");
                continue;
            }
        };
        let anchor = damaged_anchor_dropped(
            &id,
            revision_anchor(anchor_kind.as_deref(), revision, parent_hash.as_deref()),
        );
        let marker = |id| StoredSyncMarkerRecord {
            id,
            kind,
            seen_generation,
            event_seq,
            created_seq,
            revision_anchor: anchor,
        };
        let record = match (pending, content) {
            (true, _) => StoredObjectRecord::PendingCreate(marker(id)),
            (false, Some(content)) => {
                match serde_json::from_slice::<StoredPresentContent>(&content) {
                    Ok(content) => {
                        StoredObjectRecord::Present(Box::new(StoredPresentObjectRecord {
                            id,
                            kind,
                            seen_generation,
                            event_seq,
                            created_seq,
                            content,
                        }))
                    }
                    // Unreadable content is a broken cache entry, and the answer to
                    // one is to fetch it again. Keep the chain position it proved
                    // while it was readable, the way an absent object does.
                    Err(error) => {
                        tracing::warn!(object_id = %id, "Local cache row will not parse: {error}");
                        if anchor.is_none() {
                            continue;
                        }
                        StoredObjectRecord::Deleted(marker(id))
                    }
                }
            }
            // A held object with no content is a row this code never writes.
            (false, None) => {
                tracing::warn!(object_id = %id, "Skipping a held object with no content");
                if anchor.is_none() {
                    continue;
                }
                StoredObjectRecord::Deleted(marker(id))
            }
        };
        records.push(record);
    }
    Ok(records)
}

/// The objects of one kind that a reconciliation pass did not account for.
///
/// Just the ids. The sweep decides what to do from the row it fetches next,
/// and most passes account for everything, so this never reads or parses an
/// object's content.
pub(super) fn stale_object_ids(
    connection: &Connection,
    kind: ObjectKind,
    generation: u64,
    stream_start_seq: i64,
) -> Result<Vec<String>, LocalStoreError> {
    let mut statement = connection.prepare(
        "SELECT id FROM objects
         WHERE kind = ?1
           AND created_seq <= ?2
           AND (seen_generation IS NULL OR seen_generation <> ?3)",
    )?;
    let ids = statement.query_map(
        params![kind.to_string(), stream_start_seq, generation as i64],
        |row| row.get::<_, String>(0),
    )?;
    ids.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Persist a record, and the payload ciphertext that belongs with it, as one
/// transaction.
///
/// One transaction, so a crash cannot leave a record whose payload is
/// missing. Such a record is unreadable content the next hydration has to
/// clean up.
pub(super) fn write_record(
    connection: &mut Connection,
    record: &StoredObjectRecord,
    payload: Option<&[u8]>,
) -> Result<(), LocalStoreError> {
    // A payload row references its object row, so the only record that can
    // carry one is a held object. Saying so here turns a caller's mistake into
    // a clear error rather than a foreign-key failure from three frames down.
    if payload.is_some() && !matches!(record, StoredObjectRecord::Present(_)) {
        return Err(LocalStoreError::EncryptedCache(
            "a cached payload can only be written with a held object".into(),
        ));
    }

    let transaction = connection.transaction()?;
    match record {
        StoredObjectRecord::Present(present) => {
            upsert_object(
                &transaction,
                &present.id,
                present.kind,
                false,
                present.seen_generation,
                present.event_seq,
                present.created_seq,
                Some(&serde_json::to_vec(&present.content)?),
            )?;
            // Mirror the chain position out of the cache entry as it lands.
            // The envelope inside the content proves the same thing, but only
            // while the content is readable, and the content is the disposable
            // half. Collab docs have no chain and so get no row.
            match present_revision_anchor(present)? {
                Some(anchor) => upsert_anchor(
                    &transaction,
                    &present.id,
                    present.kind,
                    present.event_seq,
                    present.created_seq,
                    present.seen_generation,
                    Some(anchor),
                )?,
                // A record with no chain of its own must not take one away.
                // A collab listing served under the id of an encrypted object
                // would otherwise erase that object's accepted revision.
                None => {
                    if anchor_has_chain_position(&transaction, &present.id)? {
                        return Err(LocalStoreError::EncryptedCache(format!(
                            "object {} holds a revision anchor no chainless record may replace",
                            present.id
                        )));
                    }
                    transaction.execute(
                        "DELETE FROM object_anchors WHERE object_id = ?1",
                        params![present.id],
                    )?;
                }
            }
        }
        StoredObjectRecord::PendingCreate(marker) => {
            upsert_object(
                &transaction,
                &marker.id,
                marker.kind,
                true,
                marker.seen_generation,
                marker.event_seq,
                marker.created_seq,
                None,
            )?;
            // A pending fetch keeps whatever the object had already proved, so
            // the anchor survives the round trip that replaces the content.
            carry_anchor(&transaction, marker)?;
        }
        StoredObjectRecord::Deleted(marker) => {
            // The cached content goes (the payload with it, by cascade) and
            // the anchor is all that remains.
            transaction.execute("DELETE FROM objects WHERE id = ?1", params![marker.id])?;
            write_departed_anchor(&transaction, marker)?;
        }
    }
    if let Some(payload) = payload {
        transaction.execute(
            "INSERT INTO object_payloads (object_id, ciphertext) VALUES (?1, ?2)
             ON CONFLICT(object_id) DO UPDATE SET ciphertext = excluded.ciphertext",
            params![record.id(), payload],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

pub(super) fn read_payload(
    connection: &Connection,
    object_id: &str,
) -> Result<Option<Vec<u8>>, LocalStoreError> {
    connection
        .query_row(
            "SELECT ciphertext FROM object_payloads WHERE object_id = ?1",
            params![object_id],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(Into::into)
}

/// Only the browser store deletes a payload on its own; on native the row
/// cascades with its object. Kept for the test that simulates a lost payload.
#[cfg(test)]
pub(super) fn delete_payload(
    connection: &Connection,
    object_id: &str,
) -> Result<(), LocalStoreError> {
    connection.execute(
        "DELETE FROM object_payloads WHERE object_id = ?1",
        params![object_id],
    )?;
    Ok(())
}

/// Drop everything this device knows about an object, anchor included.
///
/// Only for objects with nothing to protect — a collab doc, which has no
/// chain, or a record too damaged to yield one. Anything else should be left
/// as an anchor instead; see `LocalStore::discard_unreadable_cache_entry`.
/// An object whose anchor records a chain position is refused here rather than
/// forgotten, because nothing the server holds can restore that memory.
pub(super) fn forget_object(
    connection: &mut Connection,
    object_id: &str,
) -> Result<(), LocalStoreError> {
    let transaction = connection.transaction()?;
    if anchor_has_chain_position(&transaction, object_id)? {
        return Err(LocalStoreError::EncryptedCache(format!(
            "object {object_id} holds a revision anchor and must not be forgotten"
        )));
    }
    transaction.execute("DELETE FROM objects WHERE id = ?1", params![object_id])?;
    transaction.execute(
        "DELETE FROM object_anchors WHERE object_id = ?1",
        params![object_id],
    )?;
    transaction.commit()?;
    Ok(())
}

/// Whether the object's anchor row records a chain position, rather than only
/// the ordering guard a deleted object leaves behind.
fn anchor_has_chain_position(
    connection: &Connection,
    object_id: &str,
) -> Result<bool, LocalStoreError> {
    let found: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM object_anchors WHERE object_id = ?1 AND anchor_kind IS NOT NULL",
            params![object_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

#[allow(clippy::too_many_arguments)]
fn upsert_object(
    connection: &Connection,
    id: &str,
    kind: ObjectKind,
    pending: bool,
    seen_generation: Option<u64>,
    event_seq: i64,
    created_seq: i64,
    content: Option<&[u8]>,
) -> Result<(), LocalStoreError> {
    connection.execute(
        "INSERT INTO objects (id, kind, pending, seen_generation, event_seq, created_seq, content)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
             kind            = excluded.kind,
             pending         = excluded.pending,
             seen_generation = excluded.seen_generation,
             event_seq       = excluded.event_seq,
             created_seq     = excluded.created_seq,
             content         = excluded.content",
        params![
            id,
            kind.to_string(),
            pending,
            seen_generation.map(|generation| generation as i64),
            event_seq,
            created_seq,
            content,
        ],
    )?;
    Ok(())
}

/// Write the durable row for an object.
///
/// The row exists even when there is no chain position to retain: for a
/// deleted object its `event_seq` is what stops a late create event bringing
/// the object back.
fn upsert_anchor(
    connection: &Connection,
    object_id: &str,
    kind: ObjectKind,
    event_seq: i64,
    created_seq: i64,
    seen_generation: Option<u64>,
    anchor: Option<StoredRevisionAnchor>,
) -> Result<(), LocalStoreError> {
    connection.execute(
        "INSERT INTO object_anchors
             (object_id, kind, event_seq, created_seq, seen_generation,
              anchor_kind, revision, parent_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(object_id) DO UPDATE SET
             kind            = excluded.kind,
             event_seq       = excluded.event_seq,
             created_seq     = excluded.created_seq,
             seen_generation = excluded.seen_generation,
             anchor_kind     = excluded.anchor_kind,
             revision        = excluded.revision,
             parent_hash     = excluded.parent_hash",
        params![
            object_id,
            kind.to_string(),
            event_seq,
            created_seq,
            seen_generation.map(|generation| generation as i64),
            anchor.map(|anchor| anchor_kind_text(anchor.kind)),
            anchor.map(|anchor| anchor.head.revision as i64),
            anchor.map(|anchor| anchor.head.parent_hash),
        ],
    )?;
    Ok(())
}

fn write_departed_anchor(
    connection: &Connection,
    marker: &StoredSyncMarkerRecord,
) -> Result<(), LocalStoreError> {
    upsert_anchor(
        connection,
        &marker.id,
        marker.kind,
        marker.event_seq,
        marker.created_seq,
        marker.seen_generation,
        marker.revision_anchor,
    )
}

/// Carry an anchor alongside an object whose content is being refetched.
///
/// The bookkeeping fields are kept in step with the live row so that dropping
/// that row later leaves a departed anchor that is still correct. With nothing
/// to retain there is nothing to keep: the live row already holds the ordering
/// guard, and a leftover row here would read back afterwards as a delete that
/// never happened.
fn carry_anchor(
    connection: &Connection,
    marker: &StoredSyncMarkerRecord,
) -> Result<(), LocalStoreError> {
    if marker.revision_anchor.is_none() {
        connection.execute(
            "DELETE FROM object_anchors WHERE object_id = ?1",
            params![marker.id],
        )?;
        return Ok(());
    }
    write_departed_anchor(connection, marker)
}

/// The `object_anchors` row for an object, read back as the marker it stands
/// for.
fn read_anchor_row(
    connection: &Connection,
    object_id: &str,
) -> Result<Option<StoredSyncMarkerRecord>, LocalStoreError> {
    let row = connection
        .query_row(
            "SELECT kind, event_seq, created_seq, seen_generation, anchor_kind, revision, parent_hash
             FROM object_anchors WHERE object_id = ?1",
            params![object_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<Vec<u8>>>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((kind, event_seq, created_seq, seen_generation, ak, revision, hash)) = row else {
        return Ok(None);
    };
    Ok(Some(StoredSyncMarkerRecord {
        id: object_id.to_string(),
        kind: object_kind(&kind)?,
        seen_generation: seen_generation.map(|generation| generation as u64),
        event_seq,
        created_seq,
        revision_anchor: damaged_anchor_dropped(
            object_id,
            revision_anchor(ak.as_deref(), revision, hash.as_deref()),
        ),
    }))
}

/// Keep the ordering guard when the chain position cannot be read.
///
/// A damaged anchor row would otherwise fail every later read of that object,
/// including the marker write behind a live create event, which would tear the
/// WebSocket down and reconnect into the same failure. Dropping the chain
/// position costs rollback protection for one object and says so.
fn damaged_anchor_dropped(
    object_id: &str,
    anchor: Result<Option<StoredRevisionAnchor>, LocalStoreError>,
) -> Option<StoredRevisionAnchor> {
    match anchor {
        Ok(anchor) => anchor,
        Err(error) => {
            tracing::warn!(
                object_id = %object_id,
                "Ignoring a damaged revision anchor; this object loses rollback protection: {error}",
            );
            None
        }
    }
}

fn revision_anchor(
    anchor_kind: Option<&str>,
    revision: Option<i64>,
    parent_hash: Option<&[u8]>,
) -> Result<Option<StoredRevisionAnchor>, LocalStoreError> {
    let (Some(anchor_kind), Some(revision), Some(parent_hash)) =
        (anchor_kind, revision, parent_hash)
    else {
        return Ok(None);
    };
    let parent_hash: [u8; crypto::SHA256_BYTES] = parent_hash.try_into().map_err(|_| {
        LocalStoreError::EncryptedCache("stored parent hash is not 32 bytes".into())
    })?;
    Ok(Some(StoredRevisionAnchor {
        head: LocalHead {
            revision: revision as u64,
            parent_hash,
        },
        kind: anchor_kind_from_text(anchor_kind)?,
    }))
}

fn object_kind(text: &str) -> Result<ObjectKind, LocalStoreError> {
    ObjectKind::from_str(text)
        .map_err(|_| LocalStoreError::EncryptedCache(format!("unknown stored object kind {text}")))
}

/// Spelled out rather than derived so that adding an anchor kind is a compile
/// error here, where the stored spelling has to be chosen deliberately.
fn anchor_kind_text(kind: StoredRevisionAnchorKind) -> &'static str {
    match kind {
        StoredRevisionAnchorKind::Absent => "absent",
        StoredRevisionAnchorKind::ObservedDelete => "observed_delete",
        StoredRevisionAnchorKind::Tombstone => "tombstone",
    }
}

fn anchor_kind_from_text(text: &str) -> Result<StoredRevisionAnchorKind, LocalStoreError> {
    match text {
        "absent" => Ok(StoredRevisionAnchorKind::Absent),
        "observed_delete" => Ok(StoredRevisionAnchorKind::ObservedDelete),
        "tombstone" => Ok(StoredRevisionAnchorKind::Tombstone),
        other => Err(LocalStoreError::EncryptedCache(format!(
            "unknown stored revision anchor kind {other}"
        ))),
    }
}
