# WebSocket Sync Flow

This document describes the sync flow between a signed-in client and the
server.

The sync model has two jobs:

- WebSocket carries live changes after a connection starts.
- HTTP materializes exact objects and bounded snapshots when the client needs
  encrypted object contents.

The client does not treat a server watermark as already-applied local state. A
change is only locally applied after the corresponding local write succeeds.

## Implementation Status

The generation/snapshot sync flow described in this document is implemented
today. The only forward-looking items are called out explicitly under "Future
Work" below.

Implemented and in use today:

- Per-user server-side live broadcast: each user gets its own in-memory
  `tokio::sync::broadcast` channel, created on first WebSocket subscribe and
  pruned when its last receiver drops
  (`AppState::subscribe_ws_broadcasts` / `broadcast_ws_event` /
  `prune_idle_ws_broadcast_channel`). One user's burst can only lag that user's
  own receivers.
- Generation + create-sequence reconciliation: every connection/reconnect
  starts a new client generation; objects carry a server-assigned
  `created_seq`; snapshots, sweeps, and live events are gated on the current
  generation (`SyncEngine`/`LocalStore`).
- Committed-sequence mutation responses: object init, complete, and delete all
  return the committed sequence to the originating device
  (`ObjectInitResponse::Complete { created_seq }`,
  `ObjectCompleteResponse { created_seq }`,
  `ObjectDeleteResponse { deleted_seq }`), so the device can write the object
  locally as present without waiting for its own event to echo back.
- Live WebSocket events after a watermark, with no historical replay on the
  socket: the server sends a `hello_ack` carrying `stream_start_seq` and then
  forwards only newer live events; HTTP snapshots own everything at or before
  the watermark.
- Web (browser/wasm) WebSocket support via a one-time ticket exchanged over the
  `clipper-ticket` subprotocol; native clients connect with a Bearer token.
- 64 KiB inbound WebSocket message/frame cap on both upgrade paths
  (`WS_MAX_MESSAGE_BYTES`).
- Per-user WebSocket ticket limits: a per-user-per-minute mint rate limit plus a
  per-user cap on simultaneously outstanding unconsumed tickets, evicted
  oldest-first within that user.

See "Transport And Server Mechanics (Current)" for the wire/infra details these
guarantees rest on.

### Future Work

- Clipboard delete events. Today clipboard items leave sync only through
  retention (TTL / max-item trimming), never through a live delete event; only
  file objects support delete. The delete-marker machinery already exists on
  the client, so if clipboard delete events are added later they can reuse the
  same flow.
- Local-store peer-to-peer sync is tracked separately in
  `local-store-p2p-roadmap.md` and is out of scope here.

## Object Semantics

Clipboard history is retention-bounded. A clipboard item may disappear from sync
when it falls outside the server's clipboard retention window. When that happens,
clients may remove their local copy.

Files are durable until the user deletes them. A file must not disappear only
because old event history was pruned. File sync rebuilds from the server's
durable file snapshot.

Every materialized object has a server-assigned create sequence. The client must
not keep a visible present object without that sequence, because ordering and
snapshot sweeps depend on it.

## Connection Start

1. The client opens a WebSocket after login or reconnect.
2. The server starts listening for live events for that user. It subscribes to
   the user's broadcast channel _before_ reading the high-water sequence, so no
   live event can slip between the snapshot watermark and the live
   subscription.
3. The server chooses a stream watermark representing all events committed up to
   that moment (`stream_start_seq`, the largest committed `event_log.seq` for
   the user).
4. The server sends the watermark to the client in a `hello_ack` message.
5. The client starts a new reconciliation generation.
6. HTTP snapshots are responsible for objects created at or before the
   watermark.
7. WebSocket live processing is responsible for events after the watermark.

The server does not replay old events on the WebSocket. The WebSocket only
delivers live events after the watermark, and additionally drops any buffered
event whose `seq` is at or below the watermark. This avoids gaps between replay
and live subscription, and avoids double-applying an event that snapshots
already cover.

The server also does not echo a device's own changes back to that same device:
live broadcasts are filtered by source device id, so a connection never
receives the events it originated. The originating device instead learns the
committed sequence from the mutation's HTTP response.

## Reconciliation Generation

Each connection, reconnect, invalidation, or manual refresh starts a new
generation.

During a generation:

1. Existing visible objects stay visible while snapshots run.
2. Snapshot results confirm matching local objects for the current generation.
3. Live events note pending creates or delete markers in local state.
4. In-flight work checks that its generation is still current before committing
   a local write.
5. A successful snapshot may remove old local objects that were in snapshot scope
   but were not seen.
6. A failed or partial snapshot does not sweep anything.

The client applies local state changes one at a time so snapshot work, live
events, and object fetches cannot overwrite each other incorrectly.

## Local Object State

The client tracks each known object in one of three states.

1. Present means the object has been materialized locally with a known create
   sequence.
2. Pending create means a live create event was seen, but the object has not
   been fetched and decrypted yet.
3. Deleted means a delete event was seen in the current generation and older
   snapshot or fetch results must not resurrect the object.

Persisted local object records hold only encrypted object material (metadata
ciphertext, payload descriptors, the signed envelope, and for clipboard the
payload ciphertext). The device signing key is stored wrapped, and on
non-wasm platforms the cache directory is created private (0700, owner-checked)
with records written 0600. Decrypted display state lives only in memory and is
rebuilt by decrypting the cached ciphertext on load.

Visible lists are derived only from present objects:

1. Clipboard shows present clipboard objects sorted newest first by create
   sequence.
2. Files show present file objects sorted newest first by create sequence.
3. Pending objects are hidden.
4. Deleted objects are hidden.

The signed creation time is display metadata. It does not control sync ordering.

## Local Writes

When this device creates an object:

1. The client chooses the object id before sending the create.
2. The server commits the object and assigns the create sequence.
3. The mutation response returns that sequence (`created_seq`).
4. The client writes the object locally as present only after it knows the
   sequence.
5. The server does not need to echo the event back to the same device.

If the response is lost after the server committed:

1. The client retries the same create with the same object id.
2. The server treats the retry as the same object.
3. If the object is complete, the server returns the original create sequence.
4. If the object is still waiting for upload or completion, the server returns
   the existing pending state.
5. If the object id is reused for different data, the server reports a conflict.

The same rule prevents a local visible object from existing without a create
sequence.

## Live Create Events

When the client receives a live create event:

1. If a newer delete marker already exists for that object, ignore the create.
2. If the object is already present with the same or newer sequence, ignore the
   duplicate.
3. If the object is already present but older, refresh its sequence and keep it
   present.
4. If the object is not materialized, write or update a hidden pending-create
   state.
5. Fetch that exact object from the server.
6. Verify and decrypt the object.
7. Before committing the result, re-check the current generation and any delete
   marker for the object.
8. If still valid, replace the pending state with a present object.

For clipboard creates, materialization fetches and decrypts the clipboard
payload during sync.

For file creates, materialization fetches and decrypts file metadata only. File
blob download remains user-initiated.

If materialization fails:

1. The pending object remains hidden.
2. The client retries while the generation is still current.
3. If the server says the object is absent or no longer retained, the pending
   state is removed.
4. The client does not fall back to a broad refresh for a single create.

## Live Delete Events

Live delete events currently exist for file objects only. When the client
receives a live file delete:

1. Write a delete marker with the delete sequence.
2. Remove the local file metadata from visible state.
3. Remove any cached local blob for that file.
4. Ignore later snapshot or fetch results for that object if they are older than
   the delete marker.

A delete event for any non-file object kind is logged and ignored by the client,
because the server only emits delete events for files.

Clipboard currently disappears through retention rather than live delete events.
If clipboard delete events are added later, they follow the same delete-marker
flow.

## File Snapshot

After receiving the connection watermark, the client builds a file snapshot for
all files created at or before that watermark.

1. The client asks the server for file metadata pages in create-sequence order,
   bounded by the watermark (`created_seq_lte = stream_start_seq`).
2. For each returned file, the client skips it if a newer delete marker exists.
3. Otherwise the client verifies and decrypts file metadata.
4. The client writes the file as present and marks it seen in the current
   generation.
5. The client continues until all pages complete.
6. After the full snapshot succeeds, the client removes local files that were
   created at or before the watermark but were not seen in the generation.
7. Objects created after the watermark are left alone because they belong to
   live WebSocket processing.

If any file snapshot page fails, the client keeps existing local file state and
waits for a later successful generation.

## Clipboard Snapshot

After receiving the connection watermark, the client builds a clipboard snapshot
for the server's retained clipboard view through that watermark.

1. The client asks the server for retained clipboard pages bounded by the
   watermark.
2. The server returns only clipboard items that are still inside the current
   retention window (within the TTL and within the most-recent
   `clipboard.max_items`).
3. For each returned item, the client skips it if a newer delete marker exists.
4. Otherwise the client verifies and decrypts metadata and payload.
5. The client writes the item as present and marks it seen in the current
   generation.
6. The client continues until all retained pages complete.
7. After the full snapshot succeeds, the client removes local clipboard objects
   that were created at or before the watermark but were not seen in the
   generation.
8. Objects created after the watermark are left alone because they belong to
   live WebSocket processing.

If a clipboard item is absent from the retained snapshot, the client may remove
its local copy after the snapshot succeeds. Absence means the item is outside
the current clipboard retention contract.

If the clipboard snapshot fails, the client does not sweep local clipboard
state.

## Manual Refresh

Manual refresh uses the same flow as reconnect.

1. The client asks the current WebSocket flow to restart.
2. A new WebSocket connection is established.
3. The server sends a new watermark.
4. The client starts a new generation.
5. File and clipboard snapshots rebuild against the new watermark.
6. Sweeps happen only after their matching snapshots succeed.

Manual refresh does not use a separate sync path. Internally it signals the
running WebSocket loop to drop the current connection and reconnect; the
reconnect path does the rest.

## Invalidation And Lag

If the live WebSocket stream is no longer reliable, the server invalidates the
connection and closes it. This happens when a connection falls behind the
per-user broadcast buffer (a `Lagged` receive): the server sends an
`invalidate` message and then closes the socket cleanly (close code `AWAY`,
reason `lagged`) so the client reconnects without treating it as an error.

On invalidation:

1. The client stops trusting that connection.
2. The client starts a fresh WebSocket connection.
3. The server sends a new watermark.
4. The client starts a new generation.
5. New snapshots and live processing take over.
6. In-flight work from the old generation is ignored at local write time.

The `invalidate` message carries a `target` field. The client currently treats
any invalidation as a full reconnect regardless of `target`.

## Ordering And Duplicates

The client must tolerate duplicate and out-of-order events.

For each object:

1. Newer known state wins.
2. Duplicate or older creates are ignored.
3. Newer deletes hide the object and block older creates from resurrecting it.
4. A create never downgrades a present object back to pending.
5. A visible present object without a create sequence is invalid local state and
   must be repaired or removed.

This keeps sync correct across retries, reconnects, invalidations, and delayed
HTTP materialization.

## Transport And Server Mechanics (Current)

This section records the concrete wire and server-side mechanics the flow above
relies on. All of it is implemented.

### Connecting

There are two WebSocket entry points:

- Native clients connect to `GET /api/ws` behind the normal authenticated
  routes. They authenticate with an `Authorization: Bearer <token>` header (the
  same session token used for HTTP), so this path reuses the standard auth and
  rate-limit middleware.
- Browser/wasm clients cannot set request headers on a WebSocket, so they first
  `POST /api/ws-ticket` (authenticated) to mint a short-lived single-use ticket,
  then connect to the public `GET /api/ws-ticket/connect` advertising two
  subprotocols: the literal marker `clipper-ticket` and the ticket value. The
  server consumes the ticket, recovers the authenticated identity, and upgrades.

In both cases the upgrade caps inbound messages and frames at 64 KiB. Clients
only ever send a small JSON `hello` plus control frames, so this bound keeps
per-connection memory small.

### Hello handshake

After upgrade the client sends `{"type":"hello"}`. The server replies with
`hello_ack` carrying `server_time` and `stream_start_seq`. A first frame that is
not a valid hello is answered with a typed `error` (`expected_hello` or
`invalid_hello`) followed by a clean close. The client must see `hello_ack`
before it starts a generation and reconciliation.

### Live broadcast fan-out

Each user has its own in-memory broadcast channel (capacity 256). Object
mutations call `broadcast_ws_event` with a `WsBroadcast` that includes the
`user_id`, the `source_device_id`, the committed `seq`, and the object's kind /
id / created-at. A connected socket:

- drops events whose `seq <= stream_start_seq` (snapshots own those),
- drops events whose `source_device_id` equals its own device (no self-echo),
- otherwise forwards an `event` message.

Because channels are partitioned by user, a flood from one account can only lag
that account's own receivers, not other users'. Idle channels are removed once
their last receiver drops.

### Ticket limits

WebSocket tickets are minted per user and bounded two ways:

- A per-user-per-minute mint rate limit (`ws_tickets_per_user_per_minute`,
  default 30). Over-quota minting returns HTTP 429.
- A cap on simultaneously outstanding unconsumed tickets per user
  (`auth.max_pending_ws_tickets`). At capacity, the oldest unconsumed ticket for
  that user is evicted first, so a burst from one account cannot displace
  another account's about-to-be-used ticket. Tickets are single-use and expire
  after 60 seconds.

These per-user partitions exist specifically so one account cannot evict or
starve another account's tickets or live stream.

### Connection close cases

- Client close / transport end: the socket loop exits and the server prunes the
  idle channel.
- Lagged receiver: `invalidate` then close with code `AWAY`, reason `lagged`.
- Server channel closed (shutdown): close with code `AWAY`, reason
  `server shutting down`.
