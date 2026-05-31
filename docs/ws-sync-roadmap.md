# WebSocket Sync Flow

This document describes the sync flow between a signed-in client and the
server.

The sync model has two jobs:

- WebSocket carries live changes after a connection starts.
- HTTP materializes exact objects and bounded snapshots when the client needs
  encrypted object contents.

The client does not treat a server watermark as already-applied local state. A
change is only locally applied after the corresponding local write succeeds.

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
2. The server starts listening for live events for that user.
3. The server chooses a stream watermark representing all events committed up to
   that moment.
4. The server sends the watermark to the client.
5. The client starts a new reconciliation generation.
6. HTTP snapshots are responsible for objects created at or before the
   watermark.
7. WebSocket live processing is responsible for events after the watermark.

The server does not replay old events on the WebSocket. The WebSocket only
delivers live events after the watermark. This avoids gaps between replay and
live subscription.

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
3. The mutation response returns that sequence.
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

When the client receives a live file delete:

1. Write a delete marker with the delete sequence.
2. Remove the local file metadata from visible state.
3. Remove any cached local blob for that file.
4. Ignore later snapshot or fetch results for that object if they are older than
   the delete marker.

Clipboard currently disappears through retention rather than live delete events.
If clipboard delete events are added later, they follow the same delete-marker
flow.

## File Snapshot

After receiving the connection watermark, the client builds a file snapshot for
all files created at or before that watermark.

1. The client asks the server for file metadata pages in create-sequence order,
   bounded by the watermark.
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
   retention window.
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

Manual refresh does not use a separate sync path.

## Invalidation And Lag

If the live WebSocket stream is no longer reliable, the server invalidates the
connection and closes it.

On invalidation:

1. The client stops trusting that connection.
2. The client starts a fresh WebSocket connection.
3. The server sends a new watermark.
4. The client starts a new generation.
5. New snapshots and live processing take over.
6. In-flight work from the old generation is ignored at local write time.

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
