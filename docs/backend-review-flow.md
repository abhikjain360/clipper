# Backend Review Flow

Use this document to rebuild server context before reviewing security-sensitive
changes. The frontend, daemon, desktop app, Android app, and any future clients
should be treated as untrusted clients.

## 1. Architecture Map

The backend is the coordination and storage service. Clients do crypto locally,
then send encrypted records and opaque metadata to the server.

- `crates/server/src/main.rs`
  - boots the Axum HTTP server;
  - runs migrations;
  - checks `server_config` exists;
  - wires public routes, authenticated routes, tracing, cleanup, rate-limit pruning, and typed process-level error handling.
- `crates/server/src/error.rs`
  - owns server process error variants, display text, and exit-code mapping.
- `crates/server/src/routes/auth.rs`
  - handles OPAQUE login challenge creation/finalization, device registration/update, session creation, and logout.
- `crates/server/src/auth.rs`
  - validates bearer tokens and injects authenticated `device_id`/`session_id` into request handlers.
- `crates/server/src/routes/clipboard.rs`
  - stores encrypted clipboard blobs on disk and metadata in SQLite.
- `crates/server/src/routes/files.rs`
  - handles multi-step encrypted file upload, listing, download, and delete.
- `crates/server/src/routes/sync.rs` and `crates/server/src/ws.rs`
  - provide bootstrap state and event notification.
- `crates/server/src/state.rs`
  - owns shared DB connection, data directories, WebSocket broadcast channel, and in-memory auth challenges.
- `crates/server/src/cleanup.rs`
  - removes expired clipboard items, old event-log rows, and abandoned pending file uploads.
- `crates/api-types/src/lib.rs`
  - owns the HTTP/WebSocket API contracts shared by server and clients.
- `crates/core/src/models.rs`
  - compatibility re-exports `clipper-api-types` for existing imports.
- `crates/client/src/api_client.rs`
  - is the reference client implementation and should be checked when route models or crypto flow change.
- `crates/client/src/engine.rs`
  - owns client-side auth completion, key derivation, encryption/decryption, HTTP/WebSocket sync, and decrypted in-memory state.
- `crates/app-types/src/lib.rs`
  - owns decrypted app-visible state shared by the sync engine, daemon state events, and Flutter bridge adapters.
- `docs/local-store-p2p-roadmap.md`
  - describes the planned client-side local store, on-demand file cache, signed object envelopes, and explicit-pairing LAN P2P transport.
- `crates/daemon-types/src/protocol.rs`
  - owns the daemon/app IPC request, response, event, command, parameter, and result shapes.
- `app/rust/src/runtime.rs`
  - selects the Flutter bridge runtime:
    - macOS talks to the local `clipper-daemon` over a Unix socket;
    - Android runs the shared Rust `SyncEngine` in-process and talks to the server directly.

Current app client paths:

- macOS Flutter app -> Rust FRB bridge -> local daemon -> `clipper-client` -> server.
- Android Flutter app -> Rust FRB bridge -> in-process `clipper-client::SyncEngine` -> server.

Android system integration, such as writing sensitive clipboard contents to the
local OS clipboard, stays in Flutter/Kotlin through a `MethodChannel`. The Rust
client should stay portable and should not need JNI for Android framework calls.

SQLite stores durable metadata. The filesystem stores larger encrypted blobs:

- `server_config`
  - one row;
  - stores OPAQUE server setup, OPAQUE password file/verifier, and `enc_salt`;
  - the legacy column names are still `auth_salt` for OPAQUE server setup and `auth_hash` for the OPAQUE password file;
  - does not store the raw passphrase.
- `sessions`
  - stores random session token hashes, not bearer tokens themselves.
- `devices`
  - stores device ID, device name, platform, and last-seen timestamps.
- `clipboard_items`
  - stores nonce, ciphertext path, ciphertext size/hash, source device, and expiry.
- `files`
  - stores encrypted metadata, blob nonce/path, ciphertext size/hash, source device, and upload status.
- `event_log`
  - stores ordered object events used by bootstrap/WebSocket replay.

Database schema source of truth lives in
`crates/server/src/migration/m20260312_000001_create_tables.rs`; SeaORM entity
files under `crates/server/src/entity/` are generated from that schema and
should not be treated as the schema owner.

## 2. Expected Application Flow

First-run server setup:

1. Operator runs `clipper-server init`.
2. Server generates OPAQUE server setup and an encryption salt.
3. Server stores the OPAQUE server setup and password file/verifier in `server_config`.
4. Server creates `clipboard/` and `files/` data directories.

Normal client login:

1. Client starts OPAQUE locally from the passphrase.
2. Client calls `POST /api/auth/challenge` with an OPAQUE credential request.
3. Server starts OPAQUE from the stored password file/verifier, stores short-lived server login state under a random challenge ID, and returns the OPAQUE credential response, encryption salt, and KDF parameters.
4. Client finishes OPAQUE locally and sends `POST /api/auth/login` with challenge ID, OPAQUE credential finalization, and device info.
5. Server consumes the single-use challenge, finishes OPAQUE, creates/updates the device row, and returns a bearer token.
6. Client uses `Authorization: Bearer <token>` on private HTTP routes and WebSocket connections.

Client runtime notes:

- macOS login requests are sent to the daemon, which uses the shared Rust client engine.
- Android login requests are handled by the Rust client engine inside the Flutter app process.
- Both paths must produce the same server-facing OPAQUE, bearer-token, encryption, sync, and file/clipboard behavior.
- Android emulator development uses `http://10.0.2.2:8787` for host loopback. Production and physical-device deployments should use HTTPS.

Clipboard upload/list flow:

1. Client encrypts clipboard text locally using the encryption key derived from the passphrase and `enc_salt`.
2. Client sends `POST /api/clipboard` with a UUID ID, nonce, ciphertext, ciphertext hash, and source-device field.
3. Server validates the UUID, ignores spoofable provenance, writes ciphertext to `clipboard/<id>.bin`, stores metadata, writes an event, and broadcasts it.
4. Other clients learn about the item through WebSocket events or bootstrap/list endpoints.
5. Clients fetch encrypted clipboard items and decrypt locally.

File upload/download flow:

1. Client encrypts file metadata locally.
2. Client encrypts the file blob locally.
3. Client calls `POST /api/files/init` with UUID ID, encrypted metadata, blob nonce, and expected ciphertext size.
4. Server creates a pending file row and returns the blob upload URL.
5. Client uploads ciphertext bytes with `PUT /api/files/{id}/blob`.
6. Server streams the blob to disk and requires the byte count to match the initialized size.
7. Client calls `POST /api/files/{id}/complete` with ciphertext hash and size.
8. Server hashes the stored ciphertext, validates size/hash, marks the row complete, logs/broadcasts `file.created`, and exposes it to list/download.
9. Download returns encrypted blob bytes; clients decrypt locally.

Planned client-side local-store behavior keeps this same abstraction: file
metadata may be cached locally, but file blobs are still downloaded only on user
request. Future LAN P2P must follow the same on-demand blob rule.

Sync flow:

1. Client calls `GET /api/sync/bootstrap` after login or when event replay is uncertain.
2. Server returns recent encrypted clipboard items, complete encrypted file metadata, latest event sequence, device info, and server crypto parameters.
3. Client opens `GET /api/ws` with bearer auth.
4. Client sends `hello { last_seq }`.
5. Server either replays events after `last_seq`, sends `invalidate` if the gap is too old, or continues broadcasting new events.

Cleanup flow:

1. Expired clipboard items are deleted from DB and disk.
2. Old event-log rows are pruned, which can force clients to bootstrap instead of replaying old gaps.
3. Pending file uploads older than the cleanup cutoff are deleted from DB and disk.

## 3. Security Model To Rebuild Before Review

- Identify which routes are public and which routes require `auth_middleware`.
- Identify which client runtime path is affected: macOS daemon, Android in-process engine, or both.
- Confirm what the server is allowed to know:
  - It may know device IDs, timestamps, ciphertext sizes, event IDs, upload status, encrypted metadata, and ciphertext hashes.
  - It must not receive plaintext clipboard contents, plaintext file bytes, plaintext file metadata, raw passphrases, or reusable client-side encryption keys.
- Confirm whether the route handles attacker-controlled input from a client, even if authenticated.
- Treat SQLite rows and blob filenames as security-sensitive because many routes convert DB values into filesystem paths.
- Treat client-supplied `platform` and `device_name` values as display/provenance metadata only.

## 4. Authentication And Sessions

Review `routes/auth.rs`, `auth.rs`, `state.rs`, and `rate_limit.rs` first.

- Public auth routes are `POST /api/auth/challenge` and `POST /api/auth/login`.
- Login must use the OPAQUE challenge/finalization flow; raw passphrases and reusable auth hashes must not be sent to the server.
- Challenge IDs must be random, short-lived, and single-use.
- Session tokens must be random, stored server-side only as hashes, and required on all private routes.
- Expired sessions must fail closed.
- Logout should delete only the authenticated session.
- Rate limiting must apply to OPAQUE challenge starts and login finalizations, and must not trust spoofable proxy headers unless deployment config guarantees a trusted proxy.

## 5. Object And Path Safety

Review every route that reads, writes, or deletes files.

- Client-provided object IDs must be validated before becoming filenames.
- File paths must be built from server-controlled directories plus validated filenames only.
- Uploads must not overwrite existing blobs accidentally.
- Failed database writes after file writes must clean up partial files.
- Delete and download routes must reject invalid IDs before touching storage.
- DB-stored paths should be treated as tainted unless they were generated by the server from validated IDs.

## 6. Upload Limits And Streaming

For file and clipboard ingestion:

- Enforce an explicit maximum size before accepting data.
- Enforce that uploaded blob size matches initialized metadata.
- Hash large blobs incrementally; do not read unbounded files into memory.
- Delete corrupt or mismatched partial blobs.
- Prefer status transitions that cannot create visible half-complete files.

## 7. Authenticated Device Attribution

- Do not trust `source_device_id` from request bodies.
- Use the device ID injected by `auth_middleware`.
- If a route mutates an existing object, verify that cross-device mutation is intentional.
- Device IDs in DB records should be useful for provenance but must not be treated as proof of authorization unless they came from the session.

## 8. Encryption Boundary

Review `clipper-core` and `clipper-client` when server API shapes change.

- Clipboard text must be encrypted client-side before upload.
- File metadata and file blobs must be encrypted separately client-side.
- Android and macOS should share the same Rust encryption and sync path through `clipper-client`; platform-specific code should only handle local OS integration.
- Nonces must be random and never reused with the same key.
- Server responses must never include plaintext content.
- Server-side logs and errors must not include decrypted data, passphrases, OPAQUE messages, or bearer tokens.
- Server process errors should stay typed and composable. Do not add direct
  `anyhow` usage or forced stderr printing; log through `tracing` and let the
  entrypoint exit with the mapped error code.
- The server still stores an OPAQUE password file/verifier, so weak passphrases remain vulnerable to offline guessing by anyone with DB access. The verifier must not be usable directly as a login secret. Strong passphrases still matter.
- TLS is still required in real deployments; OPAQUE avoids sending the raw passphrase but does not make plain HTTP safe for bearer tokens or metadata.

## 9. Sync And Event Replay

Review `routes/sync.rs` and `ws.rs`.

- Bootstrap must return encrypted records only.
- WebSocket auth must be identical to HTTP auth.
- Replay gaps should fail safe by forcing refresh/invalidate.
- Event metadata should not leak plaintext content.
- Event ordering must not allow clients to miss creates/deletes silently.

## 10. Tests Worth Having

Prioritize tests where failure is a security bug:

- Invalid object IDs are rejected before disk access.
- Authenticated device ID overrides spoofed request body IDs.
- Duplicate IDs do not overwrite existing blobs.
- Oversized uploads are rejected.
- Blob size/hash mismatches are rejected and partial files are removed.
- Login rejects invalid, expired, reused, or malformed OPAQUE challenge finalizations.
- WebSocket replay gap behavior either replays complete history or forces bootstrap.

Avoid broad fixture-heavy tests unless they protect a real invariant.
