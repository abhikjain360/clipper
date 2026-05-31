# Local Store And P2P Roadmap

This document records the target client storage and LAN P2P direction for
Clipper. The goal is to keep the current simple product model:

- clipboard history is small, fast, and locally available;
- files appear as a shared repository of metadata;
- file bytes are downloaded only when the user asks for them;
- every network transport moves encrypted objects, not plaintext;
- LAN P2P is explicit-pairing only.

The server remains useful as an always-available encrypted relay and index, but
the client should not depend on server bootstrap for every view of local history.

## Current Model

Today the client keeps decrypted display state in memory:

- recent clipboard items are decrypted into `AppState.clipboard_items`;
- recent file metadata is decrypted into `AppState.files`;
- file blobs are not kept in memory, and downloads happen on demand;
- downloaded files are written to a user-selected target path.

Implementation status: **step 1 below has landed**, and the server-sync create
path now uses the signed object envelope shape from step 4. The client has a
durable, per-profile clipboard repository in `crates/client/src/local_store.rs`
(plaintext clipboard payloads + metadata on disk), and `AppState.clipboard_items`
is rebuilt from it. File metadata is **not** yet cached locally (step 2) — file
lists are still rebuilt from `GET /api/objects`, so the download path still
depends on the server.

The server stores the durable canonical repo today:

- clipboard ciphertext blobs on disk plus metadata in SQLite;
- encrypted file metadata in SQLite;
- encrypted file blobs on disk;
- event log rows for sync notification and replay.

This is acceptable for small clipboard history, but it is the wrong boundary for
local-first behavior and future P2P. The client should have its own local cache
of the repo.

## Target Model

Add a client-side `LocalStore` used by the sync engine, daemon, and app runtime.
The local store should keep plaintext for local convenience by default. The
security boundary is network sync: anything sent to a server, relay, or peer
must be encrypted before leaving the client. Local device protection is the
client/device owner's responsibility.

Recommended local layout:

```text
Clipper/
  client.db or metadata sidecar files
  clipboard/
    <clipboard_id>.txt
    <clipboard_id>.json
  files/
    <file_id>/
      meta.json
      blob
```

The exact filenames can change, but the separation matters:

- plaintext files are the default local representation;
- small sidecar metadata is fine when it avoids adding a database too early;
- ciphertext files are optional opportunistic caches, not required durable data;
- metadata can be listed without downloading file blobs;
- file blobs remain on-demand.

The UI should not require all decrypted clipboard text to be held in memory.
`AppState` should eventually become a lightweight view model built from the local
store, with pagination or lazy reads for clipboard history.

## Clipboard

Clipboard history should move from "recent decrypted strings in memory" to
"records on disk plus lightweight state for the UI."

Behavior:

- store clipboard text locally as plaintext text files or equivalent local DB
  rows;
- encrypt clipboard text when sending it to a server, relay, or peer;
- do not keep a second ciphertext copy by default;
- optionally cache ciphertext only for short-lived transfer or performance
  cases where the storage tradeoff is explicit;
- expose recent metadata and the currently selected/visible text to the UI;
- avoid keeping the full clipboard history decrypted in `AppState`;
- keep server retention and local retention independently configurable later.

The OS clipboard remains the source for the current paste buffer. Clipper should
not treat "latest item in memory" as the durable paste source.

## Files

Files are a shared repository of file metadata plus on-demand blobs. This is
different from Syncthing-style folder mirroring and should stay that way.

Behavior:

- cache decrypted file metadata locally so file lists work without a fresh
  bootstrap;
- do not auto-download file blobs during bootstrap, refresh, or peer sync;
- when a user downloads a file, write the plaintext file to the requested path;
- optionally also cache the downloaded plaintext blob under the local store;
- encrypt file metadata and blobs when sending them to a server, relay, or peer;
- do not keep encrypted blob copies by default.

If only metadata is cached locally, that is still a valid repo state. Blob fetch
may fail until a server or peer with the blob is reachable.

## P2P Semantics

LAN P2P should follow the same model as client-server sync:

- exchange inventory and object metadata first;
- transfer clipboard payloads because they are small;
- transfer file metadata during sync;
- transfer file blobs only when requested by the user;
- do not auto-download all files from peers;
- fail clearly when a peer has metadata but not the requested blob.

P2P discovery is not trust. mDNS or local broadcast can find candidate devices,
but trust must come from explicit pairing.

Pairing should establish:

- the shared Clipper user/server identity;
- each device identity public key;
- a pinned peer identity for future connections.

## Signing And Provenance

The current server can attribute writes from the authenticated session and ignore
spoofed `source_device_id` request fields. P2P does not have that trusted server
boundary, so replicated objects need signed envelopes.

Bulk content should keep using symmetric encryption. Device signing keys are for
authenticity, not bulk encryption.

Because local storage may not keep ciphertext, signatures should bind a stable
object/version envelope and the encrypted network representation being sent.
When a plaintext local file is sent later, the client may create a fresh
encrypted transfer representation for that send. Storage efficiency is preferred
over retaining duplicate encrypted blobs locally.

A signed object envelope should bind at least:

```text
object_id
object_type
object_version
source_device_id
created_at
nonce or nonce set
transport_ciphertext_hash
operation type
signature
```

Peers and future server-side validation can then reject forged objects,
conflicting provenance, and unauthenticated deletes without decrypting content.

## Five-Step Implementation Plan

1. Add local clipboard-on-disk. **(Done — `crates/client/src/local_store.rs`.)**

   Persist clipboard items locally and stop using `AppState.clipboard_items` as
   the durable clipboard history. Keep the current UI behavior by rebuilding a
   lightweight recent view from local storage.

2. Add local file metadata cache.

   Persist decrypted file metadata locally. File lists should be available from
   the client store after login, refresh, or offline restart, even when file
   blobs are not present.

3. Add downloaded file cache.

   Keep downloaded file blobs locally after the user requests them. Treat the
   cache as optional: metadata may exist without the blob, and blob fetch should
   still be explicit.

4. Introduce signed sync envelopes.

   Add per-device signing keys and verify object provenance independently of
   transport. This prepares both server sync and P2P sync for untrusted relays
   and direct peer replication.

5. Add explicit-pairing LAN P2P transport.

   Add peer discovery, pairing, inventory exchange, metadata replication, and
   on-demand blob transfer. P2P should write into the same local store as server
   sync and should not introduce a separate data model.

## Implementation Boundary

Do not make `ApiClient` responsible for local persistence or P2P. The intended
shape is:

```text
UI / daemon
  -> SyncEngine
      -> LocalStore
      -> server transport
      -> LAN peer transport
```

Server sync and P2P sync should both pass received encrypted transfer objects
through `LocalStore`, which can decrypt and persist the local plaintext/cache
representation. The UI should read display-ready state from `LocalStore`
through `SyncEngine`.

## Non-Goals

- No implicit pairing.
- No internet-scale NAT traversal in the first P2P version.
- No automatic full file mirroring.
- No plaintext over server, relay, or peer connections.
- No server trust requirement for object contents.
- No local-at-rest encryption in the first local-store version.
- No in-app reader requirement for locally cached files.
- No duplicate encrypted local blob storage by default.
