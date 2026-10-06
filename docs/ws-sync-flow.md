# WebSocket Sync Flow

This document describes how a signed-in client keeps its local copy of the
user's objects in step with the server.

Sync has two parts:

- The WebSocket carries live change events after a connection starts.
- HTTP fetches single objects, and lists of objects called snapshots, when the
  client needs encrypted object contents.

The server gives every committed change a sequence number (`event_log.seq`). A
sequence number is a microsecond timestamp that the server assigns and that
only increases, so sequence order is commit order. The client never treats a
sequence number the server reports as a change it has already applied: a change
counts as applied only after the matching local write succeeds.

## Object Semantics

Clipboard history is retention-bounded. A clipboard item leaves sync when it
falls outside the server's clipboard retention window: older than the TTL, or
outside the newest `clipboard.max_items`. Clients then remove their local copy.
The user cannot delete a clipboard item, so clipboard items never produce
delete events.

Files and schedule objects stay until the user deletes them. They do not
disappear because old event history was pruned; each reconnect rebuilds them
from the server's object listing.

Files and schedule objects have revisions. Each edit appends a new signed
revision, and the newest published revision is the object's head. A delete
appends a tombstone revision, and a later revision can restore the object.
Purging removes the whole chain from the server. `object-envelopes.md`
describes the chain.

Every object the server lists carries the sequence number at which its head was
published (`created_seq` on the wire). An edit or a restore publishes a new
head, so the object's sequence number moves forward. The client must not show
an object without a sequence number, because ordering and snapshot sweeps
depend on it.

Collab docs follow different rules; see "Collab Docs" below.

## Connection Start

1. The client opens a WebSocket after login or reconnect.
2. The server subscribes to the user's live broadcast channel before it reads
   the latest sequence number, so no event can fall between the two.
3. The server reads the stream start (`stream_start_seq`): the largest sequence
   number committed for the user. It takes the larger of the newest
   `event_log` row and the newest published object, because old `event_log`
   rows are pruned after a retention window.
4. The server sends the stream start to the client in a `hello_ack` message.
5. The client starts a new reconciliation generation.
6. HTTP snapshots cover objects whose sequence number is at or before the
   stream start.
7. The WebSocket covers events after the stream start.

The server does not replay old events on the WebSocket. It forwards only live
events, and it drops any buffered event whose sequence number is at or below the
stream start, because the snapshots already cover it.

The server does not echo a device's own changes back to it: live broadcasts are
filtered by source device id. The originating device learns the committed
sequence number from the HTTP response to its own write.

## Reconciliation Generation

A generation is one pass of reconciliation. Each connection, reconnect,
invalidation or manual refresh starts a new one, and every local sync write
carries the generation it belongs to.

During a generation:

1. Objects already shown stay visible while snapshots run.
2. Each object a snapshot lists is marked as seen in the current generation.
3. Live events record pending fetches and deletes in local state.
4. Every sync write checks that its generation is still the current one before
   it commits. A write from an older generation is dropped.
5. When a snapshot finishes, the client sweeps that kind: local objects at or
   before the stream start that were not seen in this generation become gone.
6. A failed snapshot sweeps nothing. A single listed item that fails
   verification, download or decryption does not fail the snapshot: it still
   counts as seen, so the copy this device already holds is kept.

The client applies local state changes one at a time, under one lock, so
snapshot work, live events and object fetches cannot overwrite each other.

## Local Object State

The client holds each object it knows about in one of three states:

1. Present: the object is fetched, verified and stored with its sequence
   number.
2. Pending fetch: a live event named the object and the fetch has not finished.
3. Gone: the object was deleted, swept, or reported missing by the server. A
   gone object keeps only its sequence number and its anchor.

An anchor is the newest revision of an object this device has accepted: its
revision number and the hash of its signed envelope body. The client refuses
any served revision that would move an object behind its anchor, including
after a delete or a sweep, so a server cannot bring back an older revision.
There are three kinds of anchor:

- Absent: the object was missing from a snapshot, or the server answered 404.
  The same head may come back later; only an older or different revision is
  refused.
- Observed delete: a live delete event arrived. The event does not carry the
  tombstone's signed body, so a revision that restores the object must be at
  least two revisions past the last head this device held.
- Tombstone: this device signed the tombstone itself, and the anchor is that
  tombstone.

Native clients store objects in a SQLite database. Cached content and anchors
live in separate tables, so dropping the cache never drops an anchor, and a gone
object is an anchor row with nothing else. The browser keeps the same records in
`localStorage`. `local-at-rest-encryption.md` describes the storage format and
file permissions.

Stored records hold only encrypted material: metadata ciphertext, payload
descriptors, the signed envelope, and for clipboard and schedule objects the
payload ciphertext. Decrypted display state lives only in memory and is rebuilt
by decrypting the stored ciphertext when the client starts.

Visible lists come only from present objects:

1. Clipboard shows present clipboard objects, newest sequence number first.
2. Files shows present file objects, newest sequence number first.
3. Pending and gone objects are hidden.

The signed creation time is display metadata. It does not control sync
ordering.

## Local Writes

When this device creates an object:

1. The client chooses the object id before sending the create.
2. The server commits the object and assigns its sequence number.
3. The response returns that sequence number (`created_seq`).
4. The client stores the object as present only after it knows the sequence
   number.

When this device revises or deletes a file or schedule object, it signs a
revision that names the head it holds as its parent. The server accepts the
revision only if that parent is still the head, and otherwise answers with a
revision conflict. The response returns the new sequence number. A tombstone
this device signed is stored locally as a Tombstone anchor.

If the response to a create is lost after the server committed:

1. The client retries the same create with the same object id.
2. The server treats the retry as the same object.
3. If the object is complete, the server returns the original sequence number.
4. If the object is still waiting for upload or completion, the server returns
   the existing pending state.
5. If the object id is reused for different data, the server reports a
   conflict.

## Live Create And Update Events

The server sends a `created` event when an object is created, and when a
revision restores a deleted object. For a file or schedule object, an `updated`
event means a new head was published. The client handles both the same way:

1. If the object is gone with a later sequence number than the event, ignore the
   event: the delete came after it.
2. If the object is present with the same or a later sequence number, ignore the
   event as a duplicate.
3. If the object is present with an earlier sequence number, keep showing it,
   record the new sequence number, and fetch the new head.
4. Otherwise record a hidden pending fetch, keeping any anchor the object
   already has.
5. Fetch that object from the server.
6. Verify the envelope, check the revision against the anchor, and decrypt.
7. Before storing the result, check again that the generation is still current
   and that no later delete has arrived. The revision check runs again at this
   point, because another event may have landed during the fetch.
8. If every check passes, store the object as present.

For a clipboard object the fetch downloads and decrypts the payload. For a
schedule object it downloads and decrypts the record payload. For a file it
decrypts the metadata only; the user starts the download of the file itself.

An `updated` event for a clipboard object is logged and ignored, because
clipboard items have no revisions. An `updated` event for a collab doc means it
was renamed; see "Collab Docs".

If the fetch fails:

1. A pending object stays hidden.
2. The fetch is not retried. The next reconnect's snapshot recovers the object.
3. If the server answers 404, the client marks the object gone and keeps its
   anchor. This also hides a copy the device held before the event.
4. The client does not fall back to a broad refresh for one object.

## Live Delete Events

The server sends a `deleted` event when a tombstone revision is published for a
file or schedule object, and when a collab doc is deleted. Purging an object
that is already tombstoned sends no event, because every client dropped it when
the tombstone arrived. Clipboard items never produce delete events.

When the client receives a `deleted` event for a file, schedule object or collab
doc:

1. If the local record has the same or a later sequence number, ignore the
   event.
2. Otherwise drop the cached content and payload ciphertext, and remove the
   object from the visible lists.
3. Store the object as gone with the delete's sequence number. A file or
   schedule object keeps its anchor as an Observed delete anchor.
4. Later snapshot or fetch results with an earlier sequence number than the
   delete are ignored. Sweeps never remove a gone record, so this holds across
   reconnects.

A `deleted` event for any other kind is logged and ignored.

## File And Schedule Snapshots

After receiving the stream start, the client lists all files, and separately
all schedule objects, whose sequence number is at or before it.

1. The client asks for pages of objects in sequence-number order, bounded by the
   stream start (`created_seq_lte = stream_start_seq`). Each page must continue
   from the previous one and stay within the stream start; a page that does not
   fails the snapshot.
2. For each listed object, the client skips it if a later delete is recorded.
3. Otherwise the client verifies the envelope, checks the revision against the
   anchor, and decrypts the metadata, plus the payload for a schedule object.
4. The client stores the object as present and marks it seen in the current
   generation.
5. The client continues until the last page.
6. After the whole snapshot succeeds, the client sweeps: local objects of that
   kind at or before the stream start that were not seen become gone, and keep
   their anchors.
7. Objects after the stream start are left alone, because the WebSocket covers
   them.

The listing leaves out tombstoned objects, so a device that missed a delete
event drops the object at the sweep.

A listed revision older than the one this device holds is an ordinary race with
a live event or with this device's own write. The client keeps its own copy,
marks it seen, and continues.

If any page fails, the client keeps its existing local state and waits for a
later generation.

## Clipboard Snapshot

After receiving the stream start, the client lists the server's retained
clipboard items up to the stream start.

1. The client asks for retained clipboard pages bounded by the stream start.
2. The server returns only clipboard items inside the retention window.
3. For each listed item, the client skips it if a later delete is recorded.
4. Otherwise the client verifies the envelope and decrypts metadata and payload.
5. The client stores the item as present and marks it seen in the current
   generation.
6. The client continues until the last page.
7. After the whole snapshot succeeds, the client sweeps local clipboard objects
   at or before the stream start that were not seen.
8. Objects after the stream start are left alone, because the WebSocket covers
   them.

An item missing from the retained listing is outside the retention window, so
the client removes its local copy at the sweep.

If the clipboard snapshot fails, the client does not sweep clipboard state.

## Collab Docs

Collab docs are server-visible documents, not encrypted objects, so
`GET /api/objects` does not list them. The client lists them with
`GET /api/collab-docs` as its own snapshot, alongside the file, clipboard and
schedule snapshots, and sweeps collab docs the listing leaves out. The listing
is one unpaged response.

The collab endpoints report timestamps rather than sequence numbers. The client
therefore orders a collab doc by its server `created_at` in microseconds, which
is on the same scale as a sequence number and does not change on a rename.

Live events for collab docs:

- `created`: the client reads the doc's metadata from
  `GET /api/collab-docs/{id}/meta`.
- `updated`: the doc was renamed, and the client reads its metadata again.
- `deleted`: handled as in "Live Delete Events".

Collab docs have no revisions and no anchors, so a swept collab doc is removed
outright. The document content syncs over a separate Y-sync WebSocket
(`/api/collab-docs/{id}/ws`), not over this event stream.

## Manual Refresh

Manual refresh uses the same flow as reconnect.

1. The client asks the running WebSocket loop to drop the current connection.
2. A new WebSocket connection is established.
3. The server sends a new stream start.
4. The client starts a new generation.
5. Every snapshot runs again against the new stream start.
6. Sweeps happen only after their matching snapshots succeed.

## Invalidation And Lag

If a connection's live stream becomes unreliable, the server invalidates the
connection and closes it. This happens when a connection falls behind the
per-user broadcast buffer (a `Lagged` receive): the server sends an
`invalidate` message and then closes the socket cleanly (close code `AWAY`,
reason `lagged`), so the client reconnects without treating it as an error.

On invalidation:

1. The client stops trusting that connection.
2. The client starts a fresh WebSocket connection.
3. The server sends a new stream start.
4. The client starts a new generation.
5. New snapshots and live processing take over.
6. In-flight work from the old generation is dropped at local write time.

The `invalidate` message carries a `target` field. The client treats every
invalidation as a full reconnect, whatever the `target`.

## Ordering And Duplicates

The client must tolerate duplicate and out-of-order events. For each object:

1. The later sequence number wins.
2. Duplicate or earlier creates and updates are ignored.
3. A later delete hides the object and stops earlier creates from bringing it
   back.
4. An event never turns a present object back into a pending one; the present
   copy stays visible while the new head is fetched.
5. A served revision never moves an object behind its anchor.
6. A visible object without a sequence number is invalid local state and must
   be repaired or removed.

Sequence numbers from the server are compared as given, with no range check
(`docs/issues.md`, entry 35).

This keeps sync correct across retries, reconnects, invalidations and delayed
HTTP fetches.

## Transport And Server Mechanics

### Connecting

There are two WebSocket entry points:

- Native clients connect to `GET /api/ws` behind the normal authenticated
  routes. They authenticate with an `Authorization: Bearer <token>` header (the
  same session token used for HTTP), so this path reuses the standard auth and
  rate-limit middleware.
- Browser clients cannot set request headers on a WebSocket, so they first
  `POST /api/ws-ticket` (authenticated) to mint a short-lived single-use ticket,
  then connect to the public `GET /api/ws-ticket/connect` advertising two
  subprotocols: the literal marker `clipper-ticket` and the ticket value. The
  server consumes the ticket, recovers the authenticated identity, and upgrades.

In both cases the upgrade caps inbound messages and frames at 64 KiB
(`WS_MAX_MESSAGE_BYTES`). Clients only send a small JSON `hello` plus control
frames, so this bound keeps per-connection memory small.

### Hello handshake

After upgrade the client sends `{"type":"hello"}`. The server replies with
`hello_ack` carrying `server_time` and `stream_start_seq`. A first frame that is
not a valid hello is answered with a typed `error` (`expected_hello` or
`invalid_hello`) followed by a clean close. The client must see `hello_ack`
before it starts a generation.

### Live broadcast fan-out

Each user has their own in-memory broadcast channel (capacity 256), created on
the first WebSocket subscribe. Object writes call `broadcast_ws_event` with a
`WsBroadcast` that includes the `user_id`, the `source_device_id`, the committed
sequence number, the event type, and the object's kind, id and creation time. A
connected socket:

- drops events whose sequence number is at or below its stream start, because
  snapshots cover them,
- drops events whose `source_device_id` is its own device, so a device never
  hears its own writes,
- forwards every other event as an `event` message.

Because channels are per user, a flood from one account can only lag that
account's own receivers. A channel is removed once its last receiver drops.

### Ticket limits

WebSocket tickets are minted per user and bounded two ways:

- A per-user mint rate limit (`ws_tickets_per_user_per_minute`, default 30).
  Minting over the limit returns HTTP 429.
- A cap on unconsumed tickets per user (`auth.max_pending_ws_tickets`). At the
  cap, that user's oldest unconsumed ticket is evicted first, so a burst from
  one account cannot displace another account's ticket. Tickets are single-use
  and expire after 60 seconds.

### Connection close cases

- Client close or transport end: the socket loop exits and the server removes
  the idle channel.
- Lagged receiver: `invalidate`, then close with code `AWAY`, reason `lagged`.
- Server channel closed (shutdown): close with code `AWAY`, reason
  `server shutting down`.
