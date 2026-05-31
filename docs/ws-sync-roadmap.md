# WebSocket Sync Roadmap

This document records the target direction for proper WebSocket-based sync.
It is a future plan, not the current protocol.

The goal is to stop treating WebSocket messages as broad invalidation hints that
force a top-N refresh. WebSocket should define the live ordering of changes, and
HTTP should materialize specific objects or bounded snapshots when needed.

## Product Semantics

Clipboard and files should have different sync contracts.

Clipboard history is retention-bounded. For now it is acceptable for visible
clipboard history to be the minimum of:

- retained `event_log` rows;
- server clipboard TTL;
- server clipboard `max_items`.

If a clipboard create event or object has fallen out of that retained set, the
client may treat it as if it never existed. The client does not need offline
access to old clipboard items unless a later product requirement adds it.

Files are durable server objects. File blobs and encrypted metadata live beyond
the event-log retention window unless the user explicitly deletes the file.
Therefore a pruned `file.created` event must not imply that the file disappeared.
File sync needs a durable server-side snapshot source.

## Stream Watermark

On WebSocket connect, the server should establish a live stream before choosing
the catch-up watermark:

1. Client opens the WebSocket.
2. Server subscribes to the live broadcast channel.
3. Server reads the current high-water `event_log.seq` as `H`.
4. Server sends `HelloAck { stream_start_seq: H }`.
5. Client starts a new local reconciliation generation.

The server must not forward live events to that socket before sending
`HelloAck`. The client needs the watermark before it can decide whether an event
belongs to WebSocket live processing or HTTP snapshot responsibility.

`stream_start_seq` is not a client-applied cursor. It means: HTTP catch-up or
snapshot is responsible for state through `H`, and WebSocket is responsible for
events after `H`.

This naming matters. A value sent by the server is not the same thing as
`last_applied_seq`; the client has only applied a seq after the matching local
state change has succeeded.

Sync must not use an HTTP bootstrap watermark. Every sync generation, including
a manual refresh, starts from a WebSocket hello and its
`stream_start_seq`.

Remove `GET /api/sync/bootstrap` from the client auth/sync flow once
login/register responses return the needed `DeviceInfo` and `ServerInfo`. Auth
responses should carry that context directly. Do not keep a legacy client path
that treats any bootstrap value as an applied sync cursor; this roadmap assumes
the transport change lands as one coherent protocol change across clients.

## WebSocket Events

WebSocket events should carry enough information to mutate the local key-value
view without a broad refresh:

- `seq`
- `event_type`, such as `clipboard.created`, `file.created`, `file.deleted`
- `object_kind`
- `object_id`
- `created_at`

The event does not need to carry actual object metadata or payload bytes. For a
create event the client can insert a pending local record and fetch that
specific object over HTTP. This should use a targeted object endpoint, for
example `GET /api/objects/{object_id}`, returning the same encrypted metadata,
payload descriptors, envelope, source-device signing key, and `created_seq` as a
list snapshot item. The targeted endpoint must be scoped by authenticated
`user_id`; missing objects and cross-user object IDs should both return `404`,
not `403`. For delete events the client can remove the object or write a
tombstone immediately.

Mutation responses should return the committed event sequence for local writes,
for example `created_seq` on object creation/completion and `deleted_seq` on
file delete. Once local mutation responses carry that sequence, WebSocket fanout
should suppress self-originated events for the same `device_id`; other devices
for the same user still receive the event. The client must still keep idempotent
merge rules because retries, reconnects, future transports, or server bugs can
still produce duplicate or older events.

Do not rely on WebSocket delivery being sorted by `seq`. Concurrent server
writers can enqueue broadcasts out of order, and `tokio::sync::broadcast` should
not be treated as a global ordering guarantee across senders. The client merge
rule must accept newer seqs and reject older ones, and tests should cover
out-of-order delivery such as seq `103` arriving before seq `102`.

Do not create a materialized local `present` row without a committed
`created_seq`. On a successful local create, first learn the committed
`created_seq` from the mutation response, then write the local `present` row and
advance any applied cursor. If the HTTP response is lost after the server
committed, retry the idempotent mutation or run the same targeted materializer
with `GET /api/objects/{object_id}` to learn `created_seq`; a `404` means the
object is absent or already fell out of retention, so the client must remove any
hidden local pending/optimistic state for it. Self-originated WebSocket fanout is
not relied on for that device.

Create mutations must be idempotent by client-generated `object_id`. Retrying the
same object init/complete request with the same envelope and payload descriptors
must return the already committed `created_seq` when the object is complete, or
the existing pending upload state when it is not complete yet. Reusing an
`object_id` for different object data is a conflict, not a second object.

The current file model has no update events. Files are create/delete only, so
file sync can stay simple until mutable file metadata exists.

## Client Reconciliation State

The client does not need a large in-memory merge map. The local store can be the
merge surface, as long as rows carry enough reconciliation state.

The local reconciliation store should stay file-backed JSON for now, matching
the existing local cache style. Do not introduce a local embedded database for
this work. On native targets, store one metadata record per object plus payload
files where needed. On web, mirror the same logical records in browser storage.
Each record should carry at least `kind`, `sync_state`, `seen_generation`,
`event_seq`, `created_seq` for all create-derived states, and enough tombstone
fields to reject older snapshot or fetch results. A materialized `present` row
without `created_seq` is invalid state and should be rejected or repaired, not
left for list ordering or sweeps to interpret.

Each WebSocket connection or invalidation starts a new `sync_generation`.
Snapshot builders mark rows as seen in that generation with a separate
`seen_generation` field. Live WebSocket events write pending rows or tombstones
directly to the local store.

Generation checks happen at local write-commit time. Snapshot builders, live
event handlers, targeted materializers, and retry tasks may carry an expected
generation, but before mutating the local store they must read/check the current
generation atomically and skip the write if it no longer matches. Do not rely on
a generation captured when an async task was spawned.

Because those writers can run concurrently, the file-backed local store needs a
serialized write path. Native can use `tokio::sync::Mutex<LocalStore>` or an
equivalent single-writer queue; web should use the equivalent serialized storage
operation. The generation/tombstone check, metadata mutation, and any payload
file cleanup for that row must happen inside the same serialized write operation.

Both clipboard and file rows should support these states:

- `present`: metadata and, for clipboard, payload have been fetched and are
  materialized locally with a known `created_seq`. `present` is not
  generation-scoped.
- `pending_create`: a create event has been seen, but object fetch/decryption has
  not completed. The row carries the create event seq as `created_seq`.
- `deleted`: a tombstone has been seen for this object in the current generation.

Files and clipboard rows need `created_seq`, because snapshots are bounded by the
stream watermark. Server object rows should carry `created_seq` for all complete
objects, including clipboard, so retained clipboard snapshots can also be
bounded by the same WebSocket watermark. Clipboard rows may also keep `event_seq`
as the newest known event for that object. If update events are added later, both
kinds should keep a `last_event_seq` and reject older state.

The visible lists are derived from local store rows:

- clipboard list: `present` clipboard rows, not tombstoned, sorted by
  `created_seq` descending;
- file list: `present` file metadata rows, not tombstoned, sorted by
  `created_seq` descending.

Visibility is not gated by `seen_generation`. Existing `present` rows remain
visible while a new generation's snapshots are in progress. The
`seen_generation` value is only the sweep gate after a snapshot succeeds.

`created_at` remains display metadata from the signed object envelope. It should
not drive sync pagination or default visible ordering because it is
client-supplied and can be clock-skewed across devices.

`pending_create` rows are internal reconciliation state for now. They should
not be exposed through app-visible state or rendered as loading rows unless a
later UI requirement adds pending-item affordances.

Tombstones are how the client prevents stale HTTP snapshot pages or late fetch
responses from resurrecting deleted objects. A tombstone does not need to keep
payload or metadata; it only needs object id, kind, generation, and event seq.
Tombstones are generation-scoped protection for in-flight work. Keep
current-generation tombstones for the life of that generation; a snapshot through
`H` is not authoritative for live events with `seq > H`, so completing that
snapshot is not enough to drop current-generation live tombstones. Tombstones are
not an unbounded local history: after a full snapshot for an object kind has
completed successfully and the matching sweep has run, compact only tombstones
from older generations for that kind.

## Connect Workflow

This roadmap intentionally does not preserve a legacy bootstrap plus broad
refresh path. The implementation can break old client behavior while the new
transport, API contracts, and local-store record shapes land together.

After `HelloAck { stream_start_seq: H }`, the client starts several processes in
parallel:

1. Start the WebSocket live-event reader. It processes events with `seq > H`.
2. Start the file snapshot builder over HTTP for file objects with
   `created_seq <= H`.
3. Start the clipboard snapshot builder over HTTP for the retained clipboard
   view through `H`, scoped to `created_seq <= H`.
4. Start targeted object materializers for any `pending_create` rows produced by
   live events or snapshot pages.

These processes all write to the same local store using generation and tombstone
checks. They do not call a broad top-N refresh after every WebSocket event.

If the WebSocket is invalidated while snapshots are in progress, the client
starts a new generation and old in-flight snapshot writes must be ignored unless
they still match the current generation.

The same write-commit generation check applies to live event handlers and
targeted object materializers, not only snapshot pages. An invalidate can happen
while any of those async tasks are in flight.

Manual/user-triggered refresh should also use this generation-based snapshot
reconciliation path by forcing a WebSocket reconnect: close the current socket if
one is open, then start a fresh connection and hello flow using the returned
`stream_start_seq`. Do not send a second hello on an already-established socket.
Do not keep the old broad top-N refresh as a parallel sync system, and do not
add an HTTP-only watermark endpoint for manual refresh, because those would have
different sweep and tombstone semantics. If WebSocket connect or hello fails,
manual refresh fails and leaves normal reconnect/backoff behavior to recover
later.

## Live File Sync

Live file sync handles WebSocket events with `seq > H`.

On `file.created`:

1. Apply the local create merge rule. If the file row is already `present` with
   an equal or newer event seq, ignore the event. If it is `present` with an
   older seq, stamp/confirm `created_seq = seq` and keep it `present`. Only
   insert or update `pending_create` when the object is not already materialized
   and no newer tombstone exists. A `present` file row without `created_seq` is
   invalid local state, not a valid merge case.
2. When the merge rule writes or refreshes `pending_create`, fetch that specific
   file object metadata over HTTP with `GET /api/objects/{object_id}`.
3. Verify the object envelope and decrypt file metadata.
4. If no newer tombstone exists for the object, replace the pending row with a
   `present` file metadata row.
5. Do not download file blobs. Blob download remains user-initiated.

If the targeted fetch fails, keep the pending row and retry with bounded
exponential backoff while the row remains in the current generation. Pending
rows remain hidden from app-visible state until materialized, and the client
should not fall back to a broad refresh.

On `file.deleted`:

1. Write a local tombstone with the delete seq.
2. Remove the local file metadata row and any cached blob for that file.
3. If a snapshot page or create fetch later returns that file, ignore it when the
   tombstone is newer.

Because the file snapshot sweep is scoped to `created_seq <= H`, a live
`file.created` with `seq > H` does not need special protection from the sweep. It
is outside the snapshot range.

## Live Clipboard Sync

Live clipboard sync handles WebSocket events with `seq > H`.

On `clipboard.created`:

1. Apply the local create merge rule. If the clipboard row is already `present`
   with an equal or newer event seq, ignore the event. If it is `present` with an
   older seq, stamp/confirm `created_seq = seq` and keep it `present`. Only
   insert or update `pending_create` when the object is not already materialized
   and no newer tombstone exists. A `present` clipboard row without `created_seq`
   is invalid local state, not a valid merge case.
2. When the merge rule writes or refreshes `pending_create`, fetch that specific
   clipboard object over HTTP with `GET /api/objects/{object_id}`.
3. Verify the object envelope, download the small clipboard payload, and decrypt
   metadata and payload.
4. If no newer tombstone exists for the object, replace the pending row with a
   `present` clipboard row and payload.

If the fetch fails, keep the pending row and retry with bounded exponential
backoff while the row remains in the current generation. This preserves
knowledge that the object exists without requiring a broad refresh. The visible
clipboard list only shows materialized `present` rows.

On `clipboard.deleted`, if such events are added:

1. Write a local tombstone with the delete seq.
2. Remove the local clipboard payload and metadata.
3. If an older snapshot page or pending fetch later returns the object, ignore it
   because the tombstone wins.

Today clipboard removal can also happen by server retention rather than an
explicit live delete event. That is handled by the ephemeral clipboard snapshot
sweep below.

Because the clipboard snapshot sweep is scoped to `created_seq <= H`, a live
`clipboard.created` with `seq > H` does not need special protection from the
sweep. It is outside the snapshot range. Current-generation `pending_create`
rows are also excluded from the sweep; they represent known live objects whose
targeted fetch has not completed yet.

## File Snapshot Build Over HTTP

Files need a different path because file objects outlive event-log retention.

Add a durable `created_seq` column to the `objects` table. Populate it for every
complete object with the same sequence as the matching `*.created` event, in the
same transaction as object completion and event insertion. Files require this
for durable snapshots; clipboard uses the same column for retained snapshots
through the WebSocket watermark.

Invariant: `objects.created_seq` must be exactly equal to the matching
`event_log.seq`. Allocate the seq once, write it to both places in the same
transaction, and cover both inline object creation and delayed object completion
paths with tests. Do not derive `created_seq` from wall-clock time, max seq
queries, or a second counter allocation.

The migration that adds `objects.created_seq` must handle existing complete
objects. Backfill from the matching `event_log` row when possible, or use an
explicit one-time reset/rebuild strategy for local development data. Do not ship
a state where complete object rows have `created_seq = NULL` and are silently
excluded by `created_seq_lte=H` snapshots.

After this migration, complete server objects without `created_seq` must be
treated as data corruption. API responses, targeted fetches, and snapshot pages
must not omit `created_seq` for complete objects.

After WebSocket hello with `stream_start_seq = H`, the client fetches a paginated
file metadata snapshot:

```text
GET /api/objects?kind=file&created_seq_lte=H&...
```

Use `<= H`, not `< H`, so the object created at exactly the stream watermark is
included in the snapshot.

Pagination should be keyset-based on `(created_seq, id)`, not offset-based. Use
explicit keyset parameters such as `after_created_seq` and `after_id`; this API
does not need opaque cursor compatibility for external consumers. The page
traversal order is a snapshot implementation detail, not UI display order;
clients derive visible ordering from local materialized rows. The cursor must be
stable and must not skip or duplicate rows within the bounded
`created_seq <= H` snapshot. The snapshot response should include enough
encrypted file metadata, payload descriptors, and `created_seq` for the client to
build its local file list without downloading file blobs.

Snapshot pages do not need to run inside one long database transaction if the
server preserves the seq/commit-order invariant: every object with
`created_seq <= H` was already committed before `H` was selected, and any object
committed after that point must receive `created_seq > H` and arrive through the
live WebSocket stream. This relies on allocating event seq while the surrounding
write transaction holds the write lock. If the storage engine or transaction
model changes such that commit order can diverge from seq order, the snapshot
API must use a read transaction or equivalent snapshot isolation.

The server migration for the snapshot API must add a compound index for this
access pattern. Prefer an index covering the scoped predicates plus keyset
suffix, such as `(user_id, kind, status, created_seq, id)` or the equivalent for
the final schema. At minimum, do not ship without a keyset pagination access
path compatible with `(kind, created_seq, id)`.

On successful completion of the file snapshot:

- mark returned files as seen in the current generation;
- apply any WebSocket tombstones received during the snapshot;
- delete local file metadata/cache entries with `created_seq <= H` that were not
  seen in the current generation, including any cached blob for those files.

Live `file.created` events with `seq > H` do not need special sweep protection if
the sweep is correctly scoped to old rows: `created_seq <= H AND not seen`.
They are outside the snapshot range. Live `file.deleted` events should write
tombstones so a snapshot page cannot resurrect a deleted file.

Detailed file snapshot workflow:

1. Mark existing local file metadata rows with `created_seq <= H` as unconfirmed
   for the new generation.
2. Fetch file metadata pages with `kind=file` and `created_seq <= H`, paginated
   by a stable keyset cursor over `(created_seq, id)`.
3. For each returned file, check for a newer tombstone. If one exists, skip the
   row.
4. Verify the object envelope and decrypt file metadata.
5. Upsert the file metadata as `present`, set `created_seq`, and mark it seen in
   the current generation.
6. Continue until all pages have been fetched successfully.
7. Sweep local file rows with `created_seq <= H` that were not seen in the
   current generation. Those files are no longer in the durable server snapshot.
   Remove their metadata/cache rows, including any local blob cache for the file.
8. Leave local file rows with `created_seq > H` alone. They came from live
   WebSocket events after the snapshot watermark.

If any page fails, stop and do not sweep unseen rows. A partial file snapshot is
not authoritative enough to delete local metadata.

## Clipboard Snapshot Build Over HTTP

Clipboard sync can use retained event history or a retained clipboard object
snapshot through `stream_start_seq`. This snapshot is ephemeral: it represents
the server's currently retained clipboard view, not durable all-time history.

Detailed clipboard snapshot workflow:

1. Mark existing local clipboard rows as unconfirmed for the new generation.
2. Fetch retained clipboard state through `H` over HTTP using object snapshot
   pages, for example `GET /api/objects?kind=clipboard&created_seq_lte=H`.
   The server query must return only the currently retained clipboard view for
   the authenticated user: complete clipboard objects with `created_seq <= H`,
   not already expired by `expires_at`, and still inside the server's
   `max_items` retention policy. The snapshot and the later sweep must use the
   same retained-view definition.
3. Fetch/decrypt each returned clipboard object, set `created_seq`, and mark it
   `present` in the current generation, unless a newer tombstone already exists.
4. Live creates that arrive while the snapshot is running use targeted object
   fetches and write to the same local store with the same generation and
   tombstone checks.
5. Continue until all retained pages have completed successfully.
6. Sweep local clipboard rows with `created_seq <= H` that were not confirmed in
   the current generation. This includes on-disk clipboard objects from older
   runs that fell outside server retention. Delete both the metadata row and the
   local payload file or browser-storage payload. Do not sweep current-generation
   `pending_create` rows, and leave rows with `created_seq > H` alone.
7. Compact older-generation tombstones after the successful snapshot and sweep.

If a clipboard create event or object is absent from the retained HTTP view, the
client can delete its local copy. For clipboard, absence means the item is
outside the current retention contract; it does not have to be recovered from old
event history.

If the clipboard snapshot fails, do not sweep. The UI may show only live
materialized items for the current generation or remain in a loading state until
the next successful reconciliation.

Clipboard and file snapshots can use the same object metadata API shape:
encrypted metadata, envelope, source-device signing key, and payload
descriptors, plus `created_seq`. The client behavior differs after metadata
arrives. Clipboard sync downloads the small clipboard payload bytes during sync
and materializes the local row as `present`; file sync stores metadata only and
leaves file blob download user-initiated. Clipboard payload downloads may be
individual requests or a server-bounded batch endpoint. If batching is added, the
server should bound the batch by total response bytes, not by item count alone.

## Local Merge Rule

The client-side model is a key-value merge, not a complex state machine. For each
object id, the newest known event wins.

For immutable create/delete objects:

- create inserts, materializes, or confirms the object;
- delete removes it or records a tombstone;
- duplicate or older events are ignored.

Create events must never downgrade a `present` local row to `pending_create`.
On `*.created`, first inspect the local row for `object_id` at write-commit time:

- if a newer tombstone exists, ignore the create;
- if the row is already `present` with an equal or newer event seq, ignore it as
  duplicate or older;
- if the row is already `present` with an older seq, stamp the incoming event
  seq/`created_seq`, mark it seen for the current generation when applicable,
  and keep it `present`;
- if the row is already `pending_create`, update its known event seq when the
  incoming create is newer and ensure a targeted materialization attempt is
  scheduled. A repeated create event after a failed fetch should restart or
  re-arm the bounded retry path, not leave the item abandoned;
- only write `pending_create` and schedule targeted materialization when the
  object is absent or not yet materialized.

A `present` row missing `created_seq` is outside the merge model. The client
should treat it as corrupt local cache state and either repair it with targeted
materialization or remove it; it must not leave it visible indefinitely.

If update events are introduced later, the local store should record the highest
applied seq per object and only allow newer state to replace older state.

## Lag And Invalidation

If the WebSocket receiver lags past the server's live-event buffer, the server
should send an invalidate message and close the connection cleanly:

```text
Invalidate { target: "all" }
```

The client must not try to re-baseline on the same connection. It should treat
invalidate as "this stream is no longer reliable," let the socket close, and
start a fresh WebSocket hello. The reconnect hello establishes the new
`stream_start_seq = H2` and starts a new reconciliation generation:

- clipboard can clear or rebuild from retained clipboard history/snapshot;
- files must run the bounded file snapshot through `H2`;
- stale-object sweeps run only after the relevant snapshot succeeds.

## Which Current P1s This Addresses

This design removes the WebSocket subscribe-after-replay race by avoiding server
replay-before-subscribe. The server subscribes first, then chooses
`stream_start_seq`; HTTP covers state through that seq and WebSocket covers later
events.

It also removes the client cursor-advance-before-refresh problem. The client does
not treat `HelloAck.stream_start_seq` as applied state, and create events are not
acknowledged as locally applied until their targeted local mutation or fetch has
succeeded.

The pruned-event issue is handled by contract:

- clipboard history may disappear when it falls out of retained history and
  clipboard retention;
- files are not reconstructed from retained event history, but from durable
  object rows bounded by `created_seq <= stream_start_seq`.

## Implementation Order

1. Add API types for `stream_start_seq`, explicit object event kinds, and
   invalidate watermarks. Remove `latest_seq` from bootstrap-style sync state;
   login/register responses should carry needed device/server info, and sync
   should not call bootstrap.
2. Add committed event sequence fields to mutation responses, make object
   init/complete retries idempotent by `object_id`, and suppress self-originated
   WebSocket fanout once local writers learn their seq from the HTTP response.
3. Add `objects.created_seq` and populate it for every completed object create in
   the same transaction as the event log row. Test that `objects.created_seq`
   equals the matching `event_log.seq` for inline and delayed completion paths,
   and backfill or explicitly reset any existing complete objects before the
   snapshot API depends on `created_seq_lte`.
4. Add the snapshot pagination index, preferably covering
   `(user_id, kind, status, created_seq, id)` or the schema-equivalent scoped
   query.
5. Add targeted object fetch by id and paginated snapshot filtering by
   `created_seq <= H`; both targeted fetch and snapshot responses must include
   `created_seq`. Clipboard snapshot queries must filter to the retained
   clipboard view, including TTL and `max_items` retention.
6. Extend the local store with reconciliation generations, pending creates, and
   tombstones.
7. Serialize local-store writes so snapshot builders, live event handlers,
   materializers, and retry tasks cannot race their generation/tombstone checks.
8. Change WebSocket connect so the server subscribes before choosing the
   watermark, and test out-of-order live delivery.
9. Change the client to process targeted events and snapshots instead of calling
   broad `refresh()` after every WebSocket event.
