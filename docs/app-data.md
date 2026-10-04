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
[object-envelopes.md](object-envelopes.md)) with HKDF-SHA256:

- the row-key key, label `clipper:app-data:row-key:v1`;
- the value key, label `clipper:app-data:value:v1`.

The row key is HMAC-SHA256 under the row-key key over the collection name, a
zero byte and the 16 bytes of the row id.

The value is encrypted with XChaCha20-Poly1305 under the value key with a
random nonce. The plaintext holds the collection name, the row id, the
collection's schema version, the device's write time and the row's JSON value,
so a device that receives a row from the server learns which collection it
belongs to. The associated data binds the row key and the revision, so the
server cannot move a value to another row or another revision.

The writing device signs, with its Ed25519 signing key, the row key, the
revision, the hash of the ciphertext (or the delete marker) and its device id.
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

- A row's value is at most 64 KiB of JSON before encryption.
- A user has at most 200,000 rows, delete markers included.
- A write batch carries at most 200 changes and counts as one request against
  the existing rate limits in [server-resource-limits.md](server-resource-limits.md).

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
