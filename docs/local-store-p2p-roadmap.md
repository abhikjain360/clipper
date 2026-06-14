# Local Store And P2P Roadmap

This document records the client storage and LAN P2P direction for Clipper. The
goal is to keep the current simple product model:

- clipboard history is small, fast, locally available, and retention-bounded;
- files appear as a shared repository of metadata;
- file bytes are downloaded only when the user asks for them;
- every network transport moves encrypted objects, not plaintext;
- LAN P2P is explicit-pairing only.

The server remains useful as an always-available encrypted relay and index, but
the client does not depend on server bootstrap for every view of local history.

> Status legend: **Done** describes behavior that ships today; **Planned**
> describes future/roadmap direction that is not yet implemented. The original
> roadmap predicted some safeguards (notably local at-rest encryption) as
> explicitly out of scope; the implementation later changed direction and ships
> them today, so those sections have been corrected.

## Current Model

The client owns a per-profile `LocalStore` (`crates/client/src/local_store.rs`)
that is the source of truth for displayed state. It is wired as
`SyncEngine -> LocalStore` (`crates/client/src/engine.rs`); `ApiClient` performs
no persistence, and the daemon talks to `SyncEngine`, not to `LocalStore`
directly.

What the local store keeps today:

- **clipboard objects** — both the encrypted clipboard payload ciphertext and an
  encrypted metadata record are persisted locally;
- **file objects** — encrypted file metadata records are persisted locally, so
  file lists survive offline restart without a fresh bootstrap. File blobs are
  **not** cached; downloads fetch the ciphertext from the server on demand.
- **device signing identity** — the per-device Ed25519 signing secret used to
  sign object envelopes, persisted wrapped (see "Signing And Provenance").

In memory, the store keeps only bounded display state: a decrypted clipboard
*preview* (text bounded to `CLIPBOARD_TEXT_PREVIEW_MAX_CHARS = 512` characters,
or a `"<mime> clipboard payload (<n> bytes)"` label for non-text), plus decrypted
file metadata. Full clipboard payload bytes are never kept resident; they are
decrypted from the local ciphertext record only for the one operation that needs
them (`clipboard_payload`, `copy_to_local`, duplicate-suppression checks). This
means `AppState.clipboard_items` is a lightweight view model rebuilt from the
local store, not a durable archive: `SyncEngine` republishes
`AppState.clipboard_items` / `AppState.files` from `LocalVisibleState` after each
mutation.

This repository is durable storage for the current retained clipboard working
set, not an all-time local-first clipboard archive: server sync may sweep
clipboard rows that fall outside the server retention contract
(`sweep_kind`). See `docs/ws-sync-flow.md` for the WebSocket sync model that
separates retention-bounded clipboard history from durable file snapshots.

The server still stores the durable canonical repo:

- clipboard ciphertext blobs on disk plus metadata in SQLite;
- encrypted file metadata in SQLite;
- encrypted file blobs on disk;
- event log rows for sync notification and replay.

### Local store on-disk layout (native)

The recommended single-`client.db` layout below never materialized; the client
uses no SQLite. On native platforms the store is JSON sidecar records plus raw
ciphertext files, keyed by a per-profile directory. The profile id is
`hex(sha256(encryption_key))` (`profile_id_from_encryption_key`), so each
account/key gets an isolated subtree:

```text
<data_dir>/client/
  device-identity-v1.<profile_id>.json
                                # wrapped device signing secret for that profile
  <profile_id>/
    objects/
      <object_id>.json           # StoredObjectRecord: Present | PendingCreate | Deleted
    clipboard/
      <object_id>.payload.ciphertext   # XChaCha20-Poly1305 clipboard payload ciphertext
```

A `Present` object record stores the `EncryptedObject` (meta nonce + meta
ciphertext, the per-payload descriptors, the signed `ObjectEnvelopeV1`, plus
`created_at` / `source_device_id`). `PendingCreate` and `Deleted` records are
sync markers carrying only sync bookkeeping (`event_seq`, `created_seq`,
`seen_generation`) — no plaintext and no ciphertext.

On the **web (wasm) target** the same record shapes are stored in
`window.localStorage` under a `clipper.client.v1.<base_dir>.<profile_id>.*`
key prefix, with an explicit object-id index capped at `OBJECT_INDEX_LIMIT =
1000`.

### Local at-rest protection (native)

The local cache is **encrypted at rest**, and the directory tree is locked down:

- Object metadata and clipboard payloads are stored only as ciphertext. The
  records never contain plaintext clipboard text; `restricts_cache_permissions_and_does_not_store_plaintext`
  asserts the stored record and payload file do not contain the plaintext.
- The decrypted preview is *re-derived from the ciphertext* on hydrate, not
  trusted from any caller-supplied field, so a tampered record cannot inject a
  misleading preview (`derives_bounded_preview_without_trusting_caller_text`).
- Cached payload integrity is checked before decryption:
  `verify_payload_ciphertext` rejects a size mismatch or a SHA-256 mismatch
  against the signed payload descriptor.
- The device signing secret is wrapped with XChaCha20-Poly1305 under a key
  derived from the OPAQUE export key
  (`derive_device_identity_wrapping_key_from_opaque_export_key`, AAD
  `clipper:wrap:device-signing-secret:v1`) and stored as a version 2
  `device-identity-v1.<profile_id>.json` record. The slot is profile-scoped
  because the wrapping key is per user; plaintext or malformed identity records
  are rejected rather than migrated.
- Directories are created `0700` and files `0600` (`ensure_private_dir`,
  `write_private_file_atomic`). `ensure_private_dir` inspects the path with
  `symlink_metadata` (no symlink following) and refuses a directory not owned by
  the current effective uid, to stop a pre-positioned symlink or
  attacker-owned directory from redirecting plaintext-derived or signing-key
  writes.

The wrapping/data keys are derived from the user's OPAQUE export key, so the
local cache is only readable after a successful login that reconstructs that
key. The clipboard data-encryption key
(`derive_data_key_from_opaque_export_key`) and the device-identity wrapping key
are HKDF-separated from the same export key.

### Atomic local writes

Native local writes are atomic-by-rename: `write_private_file_atomic` writes to a
unique `*.tmp` sibling (`create_new`, chmod `0600`, flush) and then `rename`s it
over the destination, so a crash mid-write cannot leave a partially written
record or payload in place.

(The separate `crates/fs-txn` `FsTransaction` rollback guard is used by the
**server** object-upload path — `crates/server/src/routes/objects.rs` — to delete
staged blob files when an upload transaction fails. It provides cleanup, not
write isolation, and is not used by the client local store.)

## Target Model

`LocalStore` is used by the sync engine and, transitively, the daemon and app
runtime. Unlike the original plan, the local store does **not** keep plaintext at
rest: persisted clipboard/file records hold only encrypted object material, and
the security boundary therefore covers both the network and the local disk.
Network sync still requires encryption before anything leaves the client; the
local-disk protection above is additional, not a replacement.

The current native layout is described under "Local store on-disk layout"
above. The separation that matters:

- ciphertext records are the local representation; previews are bounded and
  re-derived;
- small per-object sidecar metadata avoids pulling in a client-side database;
- metadata can be listed without downloading file blobs;
- file blobs remain on-demand.

The UI does not require all decrypted clipboard text in memory: `AppState` is a
lightweight view model built from the local store (bounded previews +
metadata), and full payloads are read lazily.

## Clipboard

Clipboard history is "retention-bounded encrypted records on disk plus
lightweight bounded state for the UI."

Behavior (current):

- retained clipboard payloads are stored locally as ciphertext, alongside an
  encrypted metadata record;
- clipboard text is encrypted (XChaCha20-Poly1305) before it is sent to the
  server, and the *same* ciphertext is what the local cache retains, so there is
  no second plaintext copy on disk;
- the UI sees bounded recent metadata plus a bounded preview; the full payload
  is decrypted only when selected/copied;
- the full clipboard history is not held decrypted in `AppState`;
- server-synced clipboard history is treated as retention-bounded. Local
  retention may become independently configurable later; until then, sync may
  sweep local clipboard rows absent from the retained server snapshot.

The OS clipboard remains the source for the current paste buffer. Clipper does
not treat "latest item in memory" as the durable paste source.

## Files

Files are a shared repository of file metadata plus on-demand blobs. This is
different from Syncthing-style folder mirroring and stays that way.

Behavior (current):

- decrypted file **metadata** is cached locally (as an encrypted record), so file
  lists work without a fresh bootstrap and survive offline restart;
- file blobs are **not** auto-downloaded during bootstrap, refresh, or live
  events;
- when a user downloads a file, the plaintext is written to the requested path
  (`download_file_path`) — **the downloaded blob is not yet cached** in the local
  store (see step 3, Planned);
- file metadata and blobs are encrypted when sent to the server;
- no encrypted blob copies are retained locally.

If only metadata is cached locally, that is a valid repo state. Blob fetch may
fail until a server (or, later, a peer) with the blob is reachable.

## P2P Semantics (Planned)

LAN P2P is **not implemented**. The intended model mirrors client-server sync:

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

The server attributes writes from the authenticated session and ignores spoofed
`source_device_id` request fields. To prepare for relays and P2P — which have no
trusted server boundary — replicated objects already carry **signed envelopes**,
and this is wired into server sync today.

Bulk content uses symmetric encryption; device signing keys are for authenticity,
not bulk encryption.

What ships now (`crates/core/src/crypto.rs`, `crates/api-types`):

- each device holds an Ed25519 signing secret (`generate_device_signing_secret_key`),
  persisted wrapped at rest (see "Local at-rest protection");
- every created object is signed with `sign_object_envelope_body` over a
  canonical postcard encoding of `ObjectEnvelopeBodyV1`;
- the signature binds, at minimum:

  ```text
  object_id
  object_type
  object_version
  source_device_id
  created_at
  operation
  meta_nonce
  sha256_meta_ciphertext        # hash of the metadata ciphertext
  payloads[]:                   # per payload:
    id
    nonce
    ciphertext_size
    sha256_ciphertext           # hash of the payload ciphertext
  ```

  The signature itself lives on `ObjectEnvelopeV1.signature`.
- on read, the client calls `verify_object_list_item_envelope`: it checks the
  envelope body matches the listed object fields and per-payload metadata, then
  verifies the Ed25519 signature against the item's
  `source_device_signing_public_key`. Downloaded payload bytes are additionally
  re-hashed (`verify_payload_hash`) against the signed `sha256_ciphertext`.

Because the signed body binds per-payload ciphertext hashes and nonces, a peer or
future server-side validator can reject forged objects, mismatched provenance,
and tampered payloads without decrypting content. The envelope does not, by
itself, authenticate deletes from an untrusted relay — `operation` is part of the
signed body, but cross-device delete provenance over untrusted transports is part
of the P2P work, not yet shipped.

## Implementation Plan

1. Add local clipboard-on-disk. **(Done — `crates/client/src/local_store.rs`.)**

   Retained clipboard items persist locally (now as ciphertext, not plaintext),
   and `AppState.clipboard_items` is a rebuilt lightweight recent view, not the
   durable working set. Retained server sync may prune local clipboard rows.

2. Add local file metadata cache. **(Done.)**

   Decrypted file metadata persists locally as an encrypted record. File lists
   are available from the client store after login (`hydrate_ciphertext_cache`),
   refresh, live create (`materialize_object`), and offline restart, even when
   file blobs are absent.

3. Add downloaded file cache. **(Planned.)**

   Keep downloaded file blobs locally after the user requests them. Today
   `download_file_*` decrypts to the target path and does not retain the blob.
   The cache should stay optional: metadata may exist without the blob, and blob
   fetch should remain explicit.

4. Introduce signed sync envelopes. **(Done for server sync.)**

   Per-device signing keys exist and object provenance is verified independently
   of transport (envelope + signature checked on every list/get; payload hashes
   re-verified on download). This prepares both server sync and P2P sync for
   untrusted relays and direct peer replication.

5. Add explicit-pairing LAN P2P transport. **(Planned.)**

   Peer discovery, pairing, inventory exchange, metadata replication, and
   on-demand blob transfer. P2P should write into the same local store as server
   sync and should not introduce a separate data model.

## Implementation Boundary

`ApiClient` is **not** responsible for local persistence or P2P (it performs no
filesystem or local-storage writes). The shape that ships today is:

```text
UI / daemon
  -> SyncEngine
      -> LocalStore
      -> server transport (ApiClient)
      -> LAN peer transport   (Planned)
```

Server sync passes received encrypted transfer objects through `LocalStore`,
which decrypts to a bounded preview/metadata view and persists the encrypted
local record. The UI reads display-ready state from `LocalStore` through
`SyncEngine` (`publish_visible_state`). P2P sync, when added, should use the same
path.

## Non-Goals

- No implicit pairing.
- No internet-scale NAT traversal in the first P2P version.
- No automatic full file mirroring.
- No plaintext over server, relay, or peer connections.
- No server trust requirement for object contents.
- No in-app reader requirement for locally cached files.
- No duplicate encrypted local blob storage by default (the local clipboard
  ciphertext *is* the network ciphertext, not a second copy).

> Note: an earlier revision listed "no local-at-rest encryption in the first
> local-store version" as a non-goal and described keeping plaintext locally by
> default. That is no longer accurate — the local cache is encrypted at rest
> (see "Local at-rest protection"). The non-goal has been removed.
