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
The user can permanently delete a clipboard item. Purge removes its server
object, revisions and payloads and emits a delete event. Each device removes
its local copy immediately during live sync or at the next successful snapshot.

Files, schedule objects and app documents stay until the user deletes them.
They do not disappear because old event history was pruned; each reconnect
rebuilds them from the server's object listing. App documents are the records
of document collections in [app-data.md](app-data.md); the browser client does
not sync them and ignores their events.

Files, schedule objects and app documents have revisions. Each edit appends a
new signed
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

A client skips a listed object or a live event whose kind it does not know, so
a server that has a newer kind does not break an older client's listings.

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
There are two kinds of anchor:

- Absent: the object was missing from a snapshot, the server answered 404, or
  a delete event's tombstone was missing or failed its checks. The same head
  may come back later; only an older or different revision is refused.
- Tombstone: the anchor is a signed tombstone, either one this device signed
  or one a live delete event carried and the client verified.

Native clients store objects in a SQLite database. Cached content and anchors
live in separate tables, so dropping the cache never drops an anchor, and a gone
object is an anchor row with nothing else. The browser keeps the same records in
`localStorage`. `local-at-rest-encryption.md` describes the storage format and
file permissions.

Stored records hold only encrypted material: metadata ciphertext, payload
descriptors, the signed envelope, and for clipboard and schedule objects and
app documents the payload ciphertext (for an app document, its first payload). Decrypted display state lives only in memory and is rebuilt
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

When this device revises or deletes a file, schedule object or app document,
it signs a
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
revision restores a deleted object. For a file, schedule object or app
document, an `updated` event means a new head was published. The client handles both the same way:

1. If the object is gone with a later sequence number than the event, ignore the
   event: the delete came after it.
2. If the object is present with the same or a later sequence number, ignore the
   event as a duplicate.
3. If the object is present with an earlier sequence number, keep showing it,
   record the new sequence number, and fetch the new head.
4. Otherwise record a hidden pending fetch, keeping any anchor the object
   already has.
5. Fetch that object from the server.
6. Verify the envelope and check the revision against the anchor. For schedule
   and clipboard objects, compare the revision and envelope body hash with the
   current session's in-memory record. If both match and its verified payload
   ciphertext is available, mark the held object seen without downloading its
   payload. Otherwise download, verify and decrypt as before.
7. Before storing the result, check again that the generation is still current
   and that no later delete has arrived. The revision check runs again at this
   point, because another event may have landed during the fetch.
8. If every check passes, store the object as present.

For a clipboard object the fetch downloads and decrypts the payload. For a
schedule object it downloads and decrypts the record payload. For an app
document it downloads and decrypts the first payload, the document's value. For
a file it decrypts the metadata only; the user starts the download of the file
itself.

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
file, schedule object or app document, and when a collab doc is deleted. Purging an object
that is already tombstoned sends no event, because every client dropped it when
the tombstone arrived. Purging a clipboard item sends a `deleted` event without
a tombstone. HTTP DELETE removes its object, all revisions and all payloads.

When the client receives a `deleted` event for a file, schedule object, app
document, clipboard item or collab doc:

1. If the local record has a later sequence number, ignore the event. With the
   same sequence number, only a tombstone for an object already stored as gone
   is checked and kept, under the rules in step 3.
2. Otherwise drop the cached content and payload ciphertext, and remove the
   object from the visible lists.
3. Store the object as gone with the delete's sequence number. For a file,
   schedule object or app document the event carries the signed tombstone; the client checks
   it against the event and the held anchor and keeps it as a Tombstone
   anchor. A missing or failing tombstone leaves an Absent anchor on the held
   head.
4. Later snapshot or fetch results with an earlier sequence number than the
   delete are ignored. Sweeps never remove a gone record, so this holds across
   reconnects.

For clipboard items, the delete keeps an Absent anchor and removes cached
payloads and visible entries. An open clipboard viewer closes when its item
disappears. The Mac daemon clears an installed or captured pasteboard entry
only while its change count still matches the recorded copy. The phone clears
an installed or captured Android clipboard entry only while its timestamp
still matches. Android 7 uses a unique clipboard token for captured and
installed entries. Ownership is scoped to the account and survives app restart;
logout drops ownership without clearing the clipboard. Mac ownership also records the
boot time, so a reused change count after reboot cannot clear a new copy.
A later copy is kept, even if its content is the same.
The Mac watcher persists the last captured or installed change count and boot
time separately from ownership. It skips that pasteboard entry after restart
or login and waits for the profile cache and ownership to load before polling.
Captured entries are saved only after a successful upload. Failed uploads are
retried while the pasteboard still holds that entry. Sending an already captured
entry returns its existing item ID.
Only a local purge or an explicit delete received live or during reconciliation
clears an owned OS clipboard entry. TTL expiry and the newest-100 limit do not.

Android restricts clipboard reads and clears while an app is in the
background. The phone keeps the ownership record when access is unavailable
and retries clearing when the app becomes active.

A `deleted` event for any other kind is logged and ignored.

## File, Schedule And App Document Snapshots

After receiving the stream start, the client lists all files, and separately
all schedule objects and all app documents, whose sequence number is at or
before it.

1. The client asks for pages of up to 500 objects, the server's default maximum,
   in sequence-number order, bounded by the
   stream start (`created_seq_lte = stream_start_seq`). Each page must continue
   from the previous one and stay within the stream start; a page that does not
   fails the snapshot.
2. For each listed object, the client skips it if a later delete is recorded.
3. Otherwise the client verifies the envelope and checks the revision against
   the anchor. An unchanged schedule head, with the same revision and envelope
   body hash as this session's in-memory record and available verified payload
   ciphertext, is marked seen without downloading its payload.
   Other objects are decrypted as before: metadata, plus the payload for a
   schedule object or an app document. Changed payloads are still hash-checked
   and authenticated before they can replace the held copy.
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

First it reads `GET /api/clipboard-deletes` in pages of 100, with `after_seq`
and `up_to_seq` bounding the cursor and stream start. These are the authenticated
user's retained clipboard delete events, containing only object ids and sequences.
Each delete removes the local payload and entry and clears matching OS ownership.

1. The client asks for retained clipboard pages of up to 500 items, bounded by
   the stream start.
2. The server returns only clipboard items inside the retention window.
3. For each listed item, the client skips it if a later delete is recorded.
4. Otherwise the client verifies the envelope and checks the revision against
   the anchor. If the revision and envelope body hash match this session's
   in-memory record and its verified payload ciphertext is available, mark the
   item seen and keep its cached content without a payload download.
   Otherwise download, verify and decrypt metadata and payload.
5. The client stores the item as present and marks it seen in the current
   generation.
6. The client continues until the last page.
7. After the whole snapshot succeeds, the client sweeps local clipboard objects
   at or before the stream start that were not seen.
8. Objects after the stream start are left alone, because the WebSocket covers
   them.

An item missing from the retained listing has expired or been purged, so the
client removes its local copy and cached payload at the sweep. The sweep does
not clear the OS clipboard; only the explicit delete events do.

If the clipboard snapshot fails, the client does not sweep clipboard state.

Cache and revision checks and marking held items seen finish outside the
bounded download buffer. Buffered futures only download, verify and decrypt;
they never acquire store locks. Each downloaded item is then persisted with
the storage-boundary revision check. This lets store operations finish without
waiting for buffered futures that the consumer has stopped polling.

For N held unchanged schedule or clipboard objects, a reconnect needs only
the list pages, rather than N payload requests. Rollback, same-revision
conflict, parent-link, signature, generation and sweep checks still apply.
An absent or discarded cache entry is fetched again. A persisted head alone
never counts as held content. Browser tabs share localStorage but have separate
decrypted records in memory; another tab's persisted revision cannot make this
tab skip a download. Each in-memory record carries the revision and body hash
of its own content, including when it is used to prepare a write.

Skip checks compare the recorded ciphertext length and hash with the envelope
and check payload presence. Native storage checks the SQLite blob's length
without loading it; browser storage checks that the payload key exists without
reading or decoding its value. Reconnects do not read or hash held payload bytes.
Bytes are still hash-checked and authenticated when downloaded or read to
rebuild decrypted content. A missing native payload or a length mismatch forces
a download.

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
