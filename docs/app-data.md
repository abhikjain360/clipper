# App data

This document specifies how Clipper stores data for apps built on top of it,
such as the gym logger and later the kitchen. App data is end-to-end encrypted
like every other Clipper object. Each device holds a full local copy it can
query with SQL, writes reach that copy first and sync when the device is
online, and the server stores only encrypted rows it cannot read.

Clipboard items, files, the schedule and collab documents keep their own
object storage, described in [object-envelopes.md](object-envelopes.md) and
[ws-sync-flow.md](ws-sync-flow.md). App data sits beside them and does not
change them.

## Terms

- **Collection**: a named set of records of one shape, for example
  `gym.sets`. A collection's definition is compiled into Clipper: its name,
  the Rust type every record must decode into, its conflict rule and the
  fields it indexes.
- **Row**: one record in a collection, identified by a UUIDv7 row id chosen by
  the device that creates it. A row's value is a JSON document. It can be flat,
  like one logged set, or nested, like a recipe.
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
- the encrypted value, or a delete marker;
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

The value is encrypted with XChaCha20-Poly1305 under the value key with a
random 24-byte nonce. The plaintext is the JSON encoding of
`AppDataValueEnvelope` in `crates/core`. It holds the collection name, the row id, the
collection's schema version, the device's write time and the row's JSON value,
so a device that receives a row from the server learns which collection it
belongs to. Its fields are `collection`, `row_id`, `schema_version`,
`written_at` and `value`. The schema version is a u64 and the write time is
an RFC 3339 string. The associated data is the 32-byte row key followed by
the revision as eight big-endian bytes, so the
server cannot move a value to another row or another revision.

The writing device signs this message with its Ed25519 signing key:

```text
ASCII("clipper:app-data-change:v1")
‖ row_key (32 bytes)
‖ revision (u64 big-endian)
‖ deleted (one byte, 0 or 1)
‖ SHA256(nonce ‖ ciphertext) (32 bytes; all zero for a delete)
‖ device_id (16 bytes)
```

The ciphertext includes its 16-byte authentication tag. A delete has no nonce
or ciphertext. The signature is 64 bytes.
The server verifies the signature with the device's registered public key
before it accepts the change, and receiving devices verify it again.

## Writing a row

1. The app validates the value against the collection's Rust type. A value
   that does not decode is refused before anything is stored.
2. The device stores the encrypted change in its local copy and adds it to its
   pending changes, with the revision it replaces. The app and the device's
   queries see the new value at once.
3. When the device is online, it sends its pending changes in batches of up
   to 200. Each change in a batch is accepted or refused on its own; a batch
   is not all-or-nothing.
4. For each accepted change the server returns its change sequence, and the
   device removes it from its pending changes.

A device that is offline keeps its pending changes across restarts and sends
them when it reconnects.

## Conflicts

The server accepts a change only if it replaces the row's current revision.
The submitted revision must be one higher than the revision it replaces.
Revisions range from 1 to 9,223,372,036,854,775,807, the server database's
positive signed 64-bit range.
When two devices changed the same row while one of them was offline, the
second one to send is refused with the row's current revision. That device
then applies the collection's conflict rule:

- **Append-only** collections, such as logged sets and body weight entries,
  create rows and never edit them, so the only possible conflict is a delete
  racing an edit. The delete wins.
- **Last write wins** collections, such as exercises and workout templates,
  keep the value with the later device write time. If the local change is
  later, the device resends it on top of the current revision; otherwise it
  drops its pending change and keeps the server's value.

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
optional byte vectors and must both be absent for a delete. `device_id` must
equal the authenticated device. The server verifies the signature against
that device's registered public key, scoped to the authenticated user.

The response is `AppDataChangesResponse` with a `results` list in request
order. Each result is one externally tagged postcard enum variant:

- `Accepted { sequence }`.
- `Conflict { current }`, where `current` is the row's stored state or absent
  when no row exists. An absent row has current revision zero.
- `Refused { reason }`, where the reason is `BadSignature`, `TooLarge`,
  `OverQuota` or `Malformed`.

A malformed change includes a wrong device id, a wrong byte-field length,
inconsistent delete fields or a revision that is not one higher than the
revision it replaces. Refused and conflicting changes do not prevent other
valid changes in the batch from being accepted. More than 200 changes rejects
the request. An empty batch returns an empty results list.

Stored state is `AppDataRow`, with these fields in serialization order:

```text
row_key, revision, sequence, deleted, nonce, ciphertext, device_id, signature
```

`device_id` becomes absent if that device is removed from the account. The row
and signature remain. A receiving device can verify the signature only while
it has the signing device's id and public key.

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

A delete is a revision with a delete marker and no value. Devices remove the
row from their queryable tables and keep the marker, so a delayed older
change cannot bring the row back. The server keeps delete markers so a device
that was offline during the delete still learns about it.

## Sync between devices

A device asks the server for changes after its last applied change sequence,
up to 500 per page, and applies them in sequence order. A new device starts
from zero and receives every row. Over the WebSocket connection described in
[ws-sync-flow.md](ws-sync-flow.md), the server announces the newest change
sequence whenever any of the user's rows change; a device that sees a sequence
higher than its own fetches the missing pages. A device applies a received
change only if its revision is higher than the one it holds.

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

The browser client does not sync app data.

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
  goes through the same validation and pending-change path as the app.

The `clipper` command-line client exposes both as `clipper data query` and
`clipper data write`.

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

## Gym collections

The gym logger is the first app on app data. Its collections:

- `gym.exercises` (last write wins): name, the muscles it trains with a share
  for each, and whether it is archived.
- `gym.workouts` (last write wins): a named template, with an ordered list of
  exercises and the target sets, reps, reps in reserve and rest for each.
- `gym.sessions` (last write wins): one visit to the gym, with its start and
  end time, the template it started from if any, and notes.
- `gym.sets` (append-only): one set, with its session, exercise, order, kind
  (warm-up or working), weight, reps, reps in reserve and completion time.
- `gym.body_weight` (append-only): one weighing, with its time and weight in
  kilograms.
- `gym.recovery` (last write wins): the user's own recovery time for one
  muscle, replacing the default.

The list of muscles and their default recovery times is part of the app, not a
collection. Muscle fatigue is calculated on the device from recent working
sets and is never stored.

## Outside this document

Server triggers that wake a device, collections the server may read, the
hosted MCP endpoint and apps defined outside Clipper's own code are specified
separately when they are built.
