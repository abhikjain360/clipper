# App data

This document specifies how Clipper stores data for apps built on top of it,
such as the gym logger and later the kitchen. App data is end-to-end encrypted
like every other Clipper object. Each device holds a full local copy it can
query with SQL, and the server stores only encrypted data it cannot read.

Clipboard items, files, the schedule and collab documents keep their own
object storage, described in [object-envelopes.md](object-envelopes.md) and
[ws-sync-flow.md](ws-sync-flow.md). Row collections have their own storage
beside it; document collections add one object kind to it.

## Terms

- **Collection**: a named set of records of one shape, for example
  `gym.sets`. A collection's definition is compiled into Clipper: its name,
  the Rust type every record must decode into, its conflict rule and the
  fields it indexes.
- **Row**: one record in a row collection, identified by a UUIDv7 row id chosen
  by the device that creates it, or by an id the collection derives from the
  value, as `gym.recovery` and `gym.sets` do. A new row of such a collection
  must use the derived id; an edit or delete names the row's existing id. A
  row's value is JSON. It can be flat, like
  one logged set, or nested, like a workout template.
- **Row collection** and **document collection**: the two ways a collection is
  stored. Row collections hold many small records, accept writes offline and
  keep only each row's newest revision. Document collections hold larger
  records that are edited over time, such as recipes; each record is a Clipper
  object with its full revision history, and writes need the server. Most of
  this document describes row collections; "Document collections" below
  describes the other kind.
- **Row key**: the identifier the server stores for a row. It is a keyed hash
  of the collection name and row id, so the server cannot tell which
  collection a row belongs to or link it to its row id.
- **Revision**: a row's version number. A row is created at revision 1, and
  every change, including a delete, increases it by one.
- **Change sequence**: a number the server assigns to every accepted change,
  increasing across all of one user's rows. A device remembers the highest
  sequence it has applied and asks for everything after it.
- **Pending change**: a change a device has saved locally that the server has
  not accepted yet.

## What the server stores

For each row the server stores only its newest revision:

- the row key, the revision and the change sequence;
- whether the revision is a delete, and its encrypted envelope (a value, or a
  delete marker for a delete);
- the id of the device that wrote it, its signature and the server's
  receive time.

Rows live in their own table in the server's existing SQLite database, apart
from the object tables, and are reached through their own routes. A change
sequence is allocated from the same counter as `event_log.seq`, inside the
transaction that writes the row, so row changes and object changes share one
ordering and one backup.

Earlier revisions are dropped when a new one is accepted. The server sees how
many rows a user has, their sizes, when they change and which device changed
them. It does not see collection names, row ids, field names or values.

## Keys and encryption

Two keys are derived from the user's data key (see
[object-envelopes.md](object-envelopes.md)) with HKDF-SHA256, no salt and
32 bytes of output:

- the row-key key, label `clipper:app-data:row-key:v1`;
- the value key, label `clipper:app-data:value:v1`.

The row key is HMAC-SHA256 under the row-key key over the UTF-8 collection name, a
zero byte and the 16 bytes of the row id.

Every revision, a delete included, carries an envelope encrypted with
XChaCha20-Poly1305 under the value key with a random 24-byte nonce. The
plaintext is the JSON encoding of `AppDataValueEnvelope` in `crates/core`. It
holds the collection name, the row id, the collection's schema version, the
device's write time, whether the revision is a delete and the row's JSON
value, so a device that receives a row from the server learns which collection
it belongs to. Its fields are `collection`, `row_id`, `schema_version`,
`written_at`, `deleted` and `value`. The schema version is a u64, the write
time is an RFC 3339 string and `value` is `null` for a delete. The associated
data is the 32-byte row key followed by the revision as eight big-endian
bytes, so the server cannot move an envelope to another row or another
revision, and cannot make up a delete.

The writing device signs this message with its Ed25519 signing key:

```text
ASCII("clipper:app-data-change:v1")
‖ row_key (32 bytes)
‖ revision (u64 big-endian)
‖ deleted (one byte, 0 or 1)
‖ SHA256(nonce ‖ ciphertext) (32 bytes)
‖ device_id (16 bytes)
```

The ciphertext includes its 16-byte authentication tag. The signature is 64
bytes. The server verifies the signature with the device's registered public
key before it accepts the change, and receiving devices verify it again with
the public key the server returns beside the row. When the writing device has
been removed from the account the server has no key to return; a receiving
device then skips the signature check, as it does for object envelopes, and
relies on the envelope's authenticated encryption.

## Writing a row

1. The app validates the value against the collection's Rust type. A value
   that does not decode is refused before anything is stored. A collection
   either stores the value as written or its Rust type's own encoding of it;
   the kitchen collections store the encoding (see [kitchen.md](kitchen.md)).
2. The device stores the encrypted change in its local copy and adds it to its
   pending changes, with the revision it replaces. The app and the device's
   queries see the new value at once.
3. When the device is online, it sends its pending changes in batches of up
   to 200. Each change in a batch is accepted or refused on its own; a batch
   is not all-or-nothing.
4. For each accepted change the server returns its change sequence, and the
   device removes it from its pending changes.

A device that is offline keeps its pending changes across restarts and sends
them when it reconnects. While online it sends them after each write, on
reconnect, and every 30 seconds while any are waiting. When the Android app
returns to the foreground, the device confirms its session over HTTP, sends
its pending changes at once and reconnects its WebSocket without waiting for
the reconnect backoff. One sending pass
sends each pending change once, and sends a change that won a conflict at
most once more; a change still pending after that waits for the next pass,
and the device fetches received changes between passes.

A device holds at most one pending change per row. Writing the row again
before the server accepts the change replaces the pending change, which keeps
the revision it replaces. A row that has been deleted cannot be written
again. When the server refuses a change, the device keeps it, stops sending
it and reports it in its sync status; writing the row again replaces it.

## Conflicts

The server accepts a change only if it replaces the row's current revision.
The submitted revision must be one higher than the revision it replaces.
Revisions range from 1 to 9,223,372,036,854,775,807, the server database's
positive signed 64-bit range.
When two devices changed the same row while one of them was offline, the
second one to send is refused with the row's current revision. That device
then applies the collection's conflict rule:

- **Append-only** collections, such as body weight entries, create rows and
  never edit them, so the only possible conflict is a delete racing an edit.
  The delete wins.
- **Last write wins** collections, such as exercises and workout templates,
  keep the value with the later device write time. If the local change is
  later, the device resends it on top of the current revision; otherwise it
  drops its pending change and keeps the server's value.

In both kinds a delete wins over an edit, whatever their write times, so a
deleted row stays deleted. Two edits of an append-only row keep the server's
value. A local change that wins is encrypted and signed again for the next
revision and keeps its original write time.

A collection can also merge values. `gym.sessions` keeps an end time once a
session has one. A local write without an end time keeps the stored one. In a
conflict the winning value takes the other value's end time when it has none,
and the device then sends the merged value on top of the current revision. A
device that missed the end therefore cannot reopen a finished session.

A device trusts a conflict only if it moves the row forward: the server's
current revision must be higher than the revision the change replaced. A
conflict that reports an older or equal revision, or reports no row while the
device holds the row past revision 1, comes from a server that lost or rolled
back data. The device treats the change as refused: it keeps it, stops sending
it and reports it in its sync status.

## Server routes

Both routes require a bearer session. Request and response bodies use
`application/vnd.clipper.postcard`, like the object routes. Byte fields are
binary byte vectors, not base64 strings. The shared types are in
`crates/api-types`.

`POST /api/app-data/changes` takes `AppDataChangesRequest` with a `changes`
list. Each change has these fields in serialization order:

```text
row_key, revision, replaces_revision, deleted, nonce, ciphertext, device_id, signature
```

`replaces_revision` is zero for a new row. `nonce` and `ciphertext` are
required for every change, a delete included. `device_id` must equal the
authenticated device. The server verifies the signature against
that device's registered public key, scoped to the authenticated user.

The response is `AppDataChangesResponse` with a `results` list in request
order. Each result is one externally tagged postcard enum variant:

- `Accepted { sequence }`.
- `Conflict { current }`, where `current` is the row's stored state or absent
  when no row exists. An absent row has current revision zero.
- `Refused { reason }`, where the reason is `BadSignature`, `TooLarge`,
  `OverQuota` or `Malformed`.

A malformed change includes a wrong device id, a wrong byte-field length or a
revision that is not one higher than the revision it replaces. Refused and conflicting changes do not prevent other
valid changes in the batch from being accepted. More than 200 changes rejects
the request. An empty batch returns an empty results list.

Stored state is `AppDataRow`, with these fields in serialization order:

```text
row_key, revision, sequence, deleted, nonce, ciphertext, device_id,
device_signing_public_key, signature
```

`device_signing_public_key` is the writing device's registered Ed25519 public
key, returned the same way object listings return
`source_device_signing_public_key`. `device_id` and the key become absent if
that device is removed from the account. The row and signature remain.

`GET /api/app-data/changes?after=<sequence>&limit=<count>` returns
`AppDataChangesPage`: `rows` followed by `newest_sequence`. `after` defaults
to zero and must be nonnegative. `limit` defaults to 500 and ranges from 1 to 500. Rows have sequence greater than `after` and are ordered by sequence.
The newest sequence is the largest stored row sequence for that user, or zero
for a user with no rows. The page and newest sequence come from one database
read snapshot. Earlier revisions are not returned.

A client continues from the last row sequence in a page. It does not skip to
`newest_sequence` while pages remain. The newest sequence is a server
watermark, not proof that the client has applied those changes.

## Deletes

A delete is a revision whose envelope has `deleted` set and no value. It is
encrypted and signed like any other revision, so a receiving device
authenticates it by decryption even when it cannot check the signature.
Devices remove the row from their queryable tables and keep the marker, so a
delayed older change cannot bring the row back. The server keeps delete
markers so a device that was offline during the delete still learns about it.

## Sync between devices

A device asks the server for changes after its last applied change sequence,
up to 500 per page, and applies them in sequence order. A new device starts
from zero and receives every row. Over the WebSocket connection described in
[ws-sync-flow.md](ws-sync-flow.md), the server announces the newest change
sequence whenever any of the user's rows change; a device that sees a sequence
higher than its own fetches the missing pages. A device applies a received
change only if its revision is higher than the one it holds and the row has
no pending local change; the conflict rule settles such a row when the pending
change is sent. A received change that fails its signature check, does not
decrypt, or names a different collection or row id than its row key is not
applied: the device reports it in its sync status and moves past it.

The WebSocket announcement is JSON:

```json
{ "type": "app_data_changed", "sequence": 123 }
```

The server announces after the write transaction commits, once per batch
with accepted changes. It sends only to that user's connections and excludes
the writing device. The `hello_ack` stream start includes app-data sequences.
On reconnect, a device fetches rows after its last applied row sequence.
Buffered announcements at or below the stream start are covered by this
fetch. Live announcements can arrive out of sequence; a client keeps the
largest announced sequence and fetches through it. A lagged connection gets
an `invalidate` for `all` and closes so the client reconnects and fetches.

## Document collections

Each record of a document collection is a Clipper object of kind
`AppDocument` (`app_document` on the server), stored, signed, synced and
deleted like the other encrypted object kinds in
[object-envelopes.md](object-envelopes.md) and
[ws-sync-flow.md](ws-sync-flow.md). The collection registry in `crates/client`
names each collection's storage: rows, with their conflict rule, or documents.

- The document id is the object id, a UUIDv7.
- The encrypted metadata is the JSON encoding of `AppDocumentMeta` in
  `crates/api-types`, with the fields `collection`, `document_id` and
  `schema_version`. A device refuses a document whose metadata names another
  object id.
- The first payload in the envelope's payload order is the JSON value, at most
  256 KiB once encrypted. Further payloads hold attachments, such as a photo of
  a dish.

Writing a document validates the value against the collection's Rust type and
then creates or revises the object on the server, so a device cannot save a
document while offline: the write fails with the offline error. A value that
does not decode is refused first, offline too. The stored value is chosen as
for rows, in "Writing a row".

- A write without an id creates a document with a new UUIDv7.
- A write that changes or deletes a document carries the revision the writer
  read. The device refuses it unless that is the revision it holds, so a
  change made from an older revision cannot replace a newer one that sync
  delivered in the meantime; the writer reads the document again and
  reapplies its change. The new revision names the held revision as its
  parent.
- A write with an id this device does not hold, and no revision, creates the
  document with that id.
- A revision whose parent is no longer the head fails with a revision conflict.
  The device then fetches the head, so a read after the failure shows the
  other write.
- A delete appends a tombstone. A deleted document cannot be written again.

Every revision is kept, as for other objects. A device fetches a document's
earlier revisions only when they are asked for.

On unlock, each device decrypts the newest revision of every document into the
same in-memory tables as rows, with the object revision in the `revision`
column and that revision's write time in `written_at`, so queries treat both
kinds alike. Snapshots and live events keep the tables in step with the held
documents. A row that names a document collection is never shown. History is
read from the object, not from the tables.

A document's history lists every revision from 1 to the revision this device
holds, each with its write time, the id of the device that wrote it and whether
it is a delete. The device fetches each revision, checks its envelope and that
it names the revision before it as its parent, and refuses a history that does
not end at the revision it holds. Reading one revision's value downloads and
decrypts that revision's first payload. Both need the server.

## Local storage and queries

Each native device keeps app data in its existing `store.sqlite3`, in the
same at-rest form as its other objects: rows are stored as the ciphertext the
server holds, together with the row key, revision, change sequence and
pending-change state. Decrypted values are never written to disk.

When the profile is unlocked, the device decrypts every row into an in-memory
SQLite database with one table per collection. Each table has the columns
`id`, `revision`, `written_at` and `value`, the JSON document, plus an index
for each field the collection declares. Apps and agents query these tables
with SQL, using SQLite's JSON functions for fields inside `value`. Local
writes and received changes update the in-memory tables and the stored
ciphertext together. A few thousand rows decrypt in well under a second.

A collection's table is named after it: `gym.sets` is the table `sets` in the
attached in-memory database `gym`, so `SELECT * FROM gym.sets` reads it. The
index for a field `f` is on `json_extract(value, '$.f')`, and a query uses it
when it filters on that same expression. `written_at` is the write time of the
row's newest revision.

A query is one statement. It runs under an SQLite authorizer that allows only
reading tables and calling functions, so a statement that writes, changes the
schema, attaches a database or sets a pragma is refused, as is a second
statement or a query that runs longer than 5 seconds. Rows come back as JSON
objects keyed by column name: text stays text, so `value` arrives as a JSON
string, integers and reals become numbers, and blobs become base64 strings.

The browser client does not sync app data, and its app-data calls return an
error.

## Opening the app offline

A phone must be able to open Clipper and log a workout without a network
connection. When resuming a saved session, the client first asks the server
to confirm it. If the server cannot be reached, the client unlocks its local
copy with the saved resume key and runs offline: it reads its local data and
keeps writes as pending changes. When the server becomes reachable, the client
confirms the session; if the server rejects it, the client signs out and
erases its local copy as it does today.

The client opens offline only within 3 days of its last confirmed session. It
stores the time of each confirmation, and treats a device clock earlier than
that stored time as past the limit, so moving the clock back does not extend
it. The limit is kept by the client itself. A modified client, or anyone who
can read the device's storage, is not bound by it. A lost phone is protected
by its screen lock and by the biometric gate on the stored resume key.

The 3 days are 72 hours measured from the device time at confirmation. The
phone stores the confirmation time in SecureStore beside the biometric-gated
credentials, bound to their token's hash. Updating this time needs no
fingerprint prompt. The client confirms on reconnect and once a minute while
running. A confirmation request times out after 10 seconds. HTTP errors do
not allow offline opening; only a network failure or timeout does.

A connection failure or TLS certificate error, including a captive portal's
certificate, allows local opening within the same saved limit. It does not
extend the limit. A successful confirmation must identify the saved user and
device; an HTML page or a reply for another session is not a confirmation.
Both 401 and 403 sign the client out and erase its local data. A valid HTTP
confirmation ends offline mode and lets pending changes push even while the
WebSocket connection is still retrying.

Removing a device from the account deletes its sessions on the server, so the
server refuses every read and write from it from then on. The keys it holds
decrypt only the copy already on the device and cannot start a new session;
signing in again requires the passphrase.

## Agent access

The desktop daemon serves app data to local agents over its existing
authenticated socket (see [local-ipc-security.md](local-ipc-security.md)):

- `query_app_data` runs one read-only SQL statement against the in-memory
  tables and returns the rows as JSON. The connection it uses cannot write.
- `write_app_data` takes a collection, a row id and a value, or a delete, and
  goes through the same validation and pending-change path as the app. A
  value written without a row id gets a new UUIDv7, or the id its collection
  derives from it.
- `app_data_status` reports how many changes are waiting to be sent, how many
  the server refused, and the last sync error.
- `app_document_history` lists a document's revisions, and
  `app_document_revision` returns the value of one of them.

`write_app_data` also writes documents, with the rules in "Document
collections", and takes the revision the value was read at.

The `clipper` command-line client exposes these as `clipper data query`,
`clipper data write` and `clipper data status`, and lists a document's earlier
revisions with `clipper data history`.

## Limits

- A row's complete plaintext envelope is at most 64 KiB of JSON before
  encryption. The server caps ciphertext at 65,552 bytes, including the tag.
- A user has at most 200,000 rows, delete markers included.
- A write batch carries at most 200 changes and counts as one request against
  the existing rate limits in [server-resource-limits.md](server-resource-limits.md).

The server's `[limits]` settings `max_app_data_ciphertext_bytes` and
`max_user_app_data_rows` configure the ciphertext and row caps. Their CLI
flags are `--max-app-data-ciphertext-bytes` and `--max-user-app-data-rows`.
These row limits are separate from object counts and object storage-byte
quotas. Updating or deleting an existing row does not consume another row.
The POST body limit is 200 times the configured ciphertext cap plus 64 KiB
for the batch's remaining fields.

## Schedule done marks

`schedule.done` is a row collection with last-write-wins conflicts and schema
version 1. Each value has this shape:

```json
{
  "item_id": "019a6312-6680-7000-8000-000000000001",
  "occurrence_key": "date:2026-10-08",
  "done": true
}
```

`item_id` is the occurrence's stable schedule series id or imported event id.
`occurrence_key` is the canonical key returned by schedule expansion: the
original recurrence id, including when an override moves the occurrence. The
row id is UUIDv5 with `item_id` as its namespace and the UTF-8 occurrence key as
its name. Devices writing the same occurrence therefore write the same row.
Titles, source names, resolved start/end times, timezone, plan context and
object revisions are not part of its key. Other occurrences have other rows.

Marking Done writes `done: true`; Undo writes `done: false` to the same row.
Do not delete a row to undo it: app-data deletes are permanent. The collection
indexes `item_id`, `occurrence_key` and `done`.

The phone schedule list and desktop Next view hide an occurrence when its
mark is true or its end time is at or before the current clock time. Starting
does not hide it. Each day's Show done toggle includes both marked and ended
occurrences. Undo only removes the explicit mark; an ended occurrence remains
in Show done. The desktop calendar dims those same occurrences. These display
rules do not alter alarms, schedule definitions or recorded time.

Local agents can read marks with
`clipper data query 'SELECT * FROM schedule.done'` and set them with
`clipper data write schedule.done`, passing the JSON value above on stdin and
no row id. Set `done` to false to undo a mark. The collection derives the row id
for both writes. An explicit row id must match the value's occurrence, even
when editing an existing row. The browser's existing lack of app-data support
also applies to done marks.

## Gym collections

The gym logger is the first app on app data. Its collections:

- `gym.exercises` (last write wins): name, the muscles it trains with a share
  for each, and whether it is archived.
- `gym.workouts` (last write wins): a named template, with an ordered list of
  exercises and, for each, the warm-up sets and their rest (60 seconds unless
  set), the target working sets, reps, reps in reserve and rest, and whether
  it forms a superset with the exercise before it. An exercise appears at
  most once. `archived` defaults to false when absent. The collection remains
  at schema version 1; older readers accept the additional field.
- `gym.sessions` (last write wins): one visit to the gym, with its start and
  end time, the template it started from if any, notes, its exercise plan
  and the exercise the user chose to do now, if any.
  The plan is copied from the template when the session starts and has the
  same fields per exercise, plus whether the exercise was skipped. Adding an
  exercise, adding a set, reordering and skipping during the session change
  the plan. Moving an exercise keeps superset blocks together: within its
  block it changes places with a neighbour, and across a block boundary its
  whole block moves past the neighbouring block. Once a session has an end
  time it keeps it, as described under Conflicts.
- `gym.sets` (last write wins): one set, with its session, exercise, order,
  kind (warm-up or working), weight, reps, reps in reserve and completion
  time. `order` is one more than the highest order among the session's sets
  when the set is logged, skipping an order whose row was deleted, so sets
  sort by order and then completion time. Its row id is a UUIDv5 of the
  session id and the order (`Set::row_id`), so two devices that log the same
  order of one session write the same row and settle it by last write wins.
  Editing a set in History is an edit of that row.
- `gym.body_weight` (append-only): one weighing, with its time and weight in
  kilograms.
- `gym.recovery` (last write wins): the user's own recovery time for one
  muscle, replacing the default. Its row id is a UUIDv5 of the muscle's name
  under a fixed namespace (`Recovery::row_id` in `crates/gym`), so each muscle
  has exactly one row and devices that set the same muscle settle it by last
  write wins.

The Library lists active workouts and exercises first, with archived items in
collapsed Archived sections. Archive and Unarchive change only the item's
flag. Archived workouts cannot start a new session, and archived exercises
are excluded from add-exercise and workout-editor choices. Open sessions,
History, Progress and Fatigue still use archived items and their names.

The list of muscles and their default recovery times is part of the app, not a
collection. Muscle fatigue is calculated on the device from recent working
sets and is never stored. Fatigue, progress and history ignore sets whose
session was deleted or is missing. Weekly body-weight averages group
weighings by the device's local calendar week, starting on Monday.

The state of a session in progress is calculated from its plan and its sets
and is never stored:

- The plan is split into blocks: an exercise that forms a superset with the
  one before it joins that exercise's block. An exercise is open while it is
  not skipped and has fewer warm-up or working sets than planned.
- When the user chose an exercise to do now and its block still has an open
  exercise, the current exercise is in that block, so a chosen superset stays
  current until every partner is done. Otherwise it is in the first block
  with an open exercise. Within the block it is the open exercise with
  the fewest working sets, the earlier one on a tie, so a superset alternates.
  Its next set is a warm-up while planned warm-ups remain, and a working set
  after that. Choosing an exercise does not change the plan, so an exercise
  left unfinished keeps its remaining sets and becomes current again once the
  chosen one is done.
- The session lists its exercises in the order they were trained: first the
  exercises with logged sets, by their first set, then the current exercise,
  then the rest in plan order.
- The rest timer starts at the completion time of the session's last set and
  lasts that exercise's rest, or its warm-up rest after a warm-up. Moving
  forward to the next exercise of the same superset has no rest, and a
  session with no current exercise has no rest timer.
- A set is logged only if the session's next order and current exercise are
  still the ones the screen showed, so a stale or repeated tap cannot log a
  set out of order.

On first use, after the device has finished downloading app data from the
server once since it unlocked, and when it has no exercises, workouts,
sessions or sets, the app writes a starter library of common barbell,
dumbbell and machine exercises and two workouts. Their row ids are fixed in
`crates/gym`, so two devices that both write it produce the same rows.

## Outside this document

Server triggers that wake a device, collections the server may read, the
hosted MCP endpoint and apps defined outside Clipper's own code are specified
separately when they are built.
