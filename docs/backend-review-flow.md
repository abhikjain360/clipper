# Backend Review Flow

Use this document to rebuild server context before reviewing security-sensitive
changes. The frontend, daemon, desktop app, Android app, and any future clients
should be treated as untrusted clients.

## 1. Architecture Map

The backend is the coordination and storage service. Clients do crypto locally,
then send encrypted records and non-decrypted metadata to the server.

- `crates/server/src/main.rs`
  - boots the Axum HTTP server;
  - runs migrations;
  - checks `server_config` exists;
  - wires public registration/login routes, authenticated routes, tracing, cleanup, rate-limit pruning, and typed process-level error handling.
- `crates/server/src/error.rs`
  - owns server process error variants, display text, and exit-code mapping.
- `crates/server/src/routes/auth.rs`
  - handles invite-key-gated OPAQUE registration, OPAQUE login challenge creation/finalization, device registration/update, session creation, and logout.
- `crates/server/src/auth.rs`
  - validates bearer tokens and injects authenticated `user_id`/`device_id`/`session_id` into request handlers.
- `crates/server/src/routes/clipboard.rs`
  - stores encrypted clipboard blobs on disk and metadata in SQLite.
- `crates/server/src/routes/files.rs`
  - handles multi-step encrypted file upload, listing, download, and delete.
- `crates/server/src/routes/sync.rs` and `crates/server/src/ws.rs`
  - provide bootstrap state and event notification.
- `crates/server/src/state.rs`
  - owns shared DB connection, data directories, WebSocket broadcast channel, in-memory auth challenges, and in-memory pending registrations.
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
  - marks server initialization;
  - new users' OPAQUE material lives in `users`, not in this table.
- `access_keys`
  - stores invite/access keys as `base64(SHA-256(access_key_bytes))`, never as plaintext;
  - tracks creation, optional expiry, and one-time use by a user.
- `users`
  - stores per-user `opaque_server_setup`, OPAQUE password file/verifier, encryption salt, and access-key hash;
  - does not store the raw passphrase.
- `sessions`
  - stores user-scoped random session token hashes, not bearer tokens themselves.
- `devices`
  - stores user ID, device ID, device name, platform, and last-seen timestamps.
- `clipboard_items`
  - stores user ID, nonce, ciphertext path, ciphertext size/hash, source device, and expiry.
- `files`
  - stores user ID, encrypted metadata, blob nonce/path, ciphertext size/hash, source device, and upload status.
- `event_log`
  - stores user-scoped ordered object events used by bootstrap/WebSocket replay.

Database schema source of truth lives in `crates/server/src/migration/*.rs`;
SeaORM entity files under `crates/server/src/entity/` are generated from that
schema and should not be treated as the schema owner.

## 2. Expected Application Flow

First-run server setup:

1. Operator runs `clipper-server init`.
2. Server creates the database, runs migrations, inserts the singleton `server_config` initialization marker, and creates `clipboard/` and `files/` data directories.
3. No user passphrase is entered during server init.

Access-key provisioning:

1. Operator creates one high-entropy access key per intended user outside the app.
2. Operator inserts `base64(SHA-256(access_key_bytes))` into `access_keys.key_hash`, with `created_at` set and optional `expires_at`.
3. The access key is a one-time registration authorization secret only. It is not the user's passphrase and is not used for data encryption.

Example manual insert:

```sh
ACCESS_KEY='replace-with-high-entropy-invite'
KEY_HASH=$(printf %s "$ACCESS_KEY" | openssl dgst -sha256 -binary | base64)
sqlite3 /path/to/clipper.db \
  "insert into access_keys (key_hash, created_at) values ('$KEY_HASH', '2026-05-24T00:00:00Z');"
```

Normal client registration:

1. Client starts OPAQUE registration locally from the user's chosen passphrase.
2. Client calls `POST /api/auth/register/start` with the access key and OPAQUE registration request.
3. Server hashes the access key, verifies it exists, is unused, and is unexpired, then creates a pending registration with a new `user_id`, per-user OPAQUE server setup, and per-user encryption salt.
4. Server returns the OPAQUE registration response, `registration_id`, `user_id`, and encryption KDF parameters.
5. Client finishes OPAQUE registration locally and sends `POST /api/auth/register/finish` with the registration upload and device info.
6. Server consumes the pending registration, stores the per-user OPAQUE password file/verifier in `users`, marks the access key used, creates/updates the first device row, and returns a bearer token.

Normal client login:

1. Client starts OPAQUE locally from the passphrase.
2. Client calls `POST /api/auth/challenge` with `user_id` and an OPAQUE credential request. If exactly one user exists, the server can resolve that user for backward compatibility; once multiple users exist, `user_id` is required.
3. Server starts OPAQUE from that user's stored password file/verifier, stores short-lived server login state under a random challenge ID, and returns the OPAQUE credential response plus that user's encryption salt and KDF parameters.
4. Client finishes OPAQUE locally and sends `POST /api/auth/login` with challenge ID, OPAQUE credential finalization, and device info.
5. Server consumes the single-use challenge, finishes OPAQUE, creates/updates a device row for that user, creates a user-scoped session, and returns a bearer token plus `user_id`.
6. Client uses `Authorization: Bearer <token>` on private HTTP routes and WebSocket connections.

Client runtime notes:

- macOS registration and login requests are sent to the daemon, which uses the shared Rust client engine.
- Android registration and login requests are handled by the Rust client engine inside the Flutter app process.
- The Flutter auth screen exposes separate Register and Login modes. Register requires an access key and passphrase; Login sends the saved or entered `user_id` when available.
- Both paths must produce the same server-facing OPAQUE, bearer-token, encryption, sync, and file/clipboard behavior.
- Android emulator development uses `http://10.0.2.2:8787` for host loopback. Production and physical-device deployments should use HTTPS.

Clipboard upload/list flow:

1. Client encrypts clipboard text locally using the encryption key derived from the passphrase and `encryption_salt`.
2. Client sends `POST /api/clipboard` with a UUID ID, nonce, ciphertext, ciphertext hash, and source-device field.
3. Server validates the UUID, ignores spoofable provenance, writes ciphertext to `clipboard/<id>.bin`, stores user-scoped metadata, writes a user-scoped event, and broadcasts it only to that user's WebSocket subscribers.
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
8. Server hashes the stored ciphertext, validates size/hash, marks the user-scoped row complete, logs/broadcasts `file.created` to that user, and exposes it to that user's list/download calls.
9. Download returns encrypted blob bytes; clients decrypt locally.

Planned client-side local-store behavior keeps this same abstraction: file
metadata may be cached locally, but file blobs are still downloaded only on user
request. Future LAN P2P must follow the same on-demand blob rule.

Sync flow:

1. Client calls `GET /api/sync/bootstrap` after login or when event replay is uncertain.
2. Server returns that user's recent encrypted clipboard items, complete encrypted file metadata, latest event sequence, device info, and encryption parameters.
3. Client opens `GET /api/ws` with bearer auth.
4. Client sends `hello { last_seq }`.
5. Server replays that user's events after `last_seq` and then continues broadcasting only that user's new events.

Cleanup flow:

1. Expired clipboard items are deleted from DB and disk.
2. Old event-log rows are pruned, which can force clients to bootstrap instead of replaying old gaps.
3. Pending file uploads older than the cleanup cutoff are deleted from DB and disk.

## 3. Security Model To Rebuild Before Review

- Identify which routes are public and which routes require `auth_middleware`.
- Identify which client runtime path is affected: macOS daemon, Android in-process engine, or both.
- Confirm what the server is allowed to know:
  - It may know user IDs, device IDs, timestamps, ciphertext sizes, event IDs, upload status, encrypted metadata, ciphertext hashes, and access-key hashes.
  - It must not receive plaintext clipboard contents, plaintext file bytes, plaintext file metadata, raw passphrases, plaintext access keys after registration request processing, or reusable client-side encryption keys.
- Confirm whether the route handles attacker-controlled input from a client, even if authenticated.
- Treat SQLite rows and blob filenames as security-sensitive because many routes convert DB values into filesystem paths.
- Treat client-supplied `platform` and `device_name` values as display/provenance metadata only.

## 4. Authentication And Sessions

Review `routes/auth.rs`, `auth.rs`, `state.rs`, and `rate_limit.rs` first.

- Public auth routes are `POST /api/auth/register/start`, `POST /api/auth/register/finish`, `POST /api/auth/challenge`, and `POST /api/auth/login`.
- Registration must be gated by a one-time access key stored as a hash in the DB.
- Registration and login must use OPAQUE flows; raw user passphrases and reusable authentication secrets must not be sent to the server.
- Challenge IDs must be random, short-lived, and single-use.
- Pending registration IDs must be random, short-lived, and single-use.
- Session tokens must be random, stored server-side only as hashes, and required on all private routes.
- Expired sessions must fail closed.
- Logout should delete only the authenticated session.
- Rate limiting must apply to registration starts/finishes, OPAQUE challenge starts, and login finalizations, and must not trust spoofable proxy headers unless deployment config guarantees a trusted proxy.
- All authenticated handlers must use the `user_id` injected by `auth_middleware` for authorization and data filtering.

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
- Scope all reads, writes, deletes, sync bootstrap responses, and WebSocket events by the authenticated `user_id`.
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
- The server stores per-user OPAQUE password files/verifiers, so weak passphrases remain vulnerable to offline guessing by anyone with DB access. A verifier must not be usable directly as a login secret. Strong passphrases still matter.
- Access keys are authorization invites, not encryption keys. They must be high entropy because the DB stores only their hashes, but possession of an unused access key permits account registration.
- TLS is still required in real deployments; OPAQUE avoids sending the raw passphrase but does not make plain HTTP safe for bearer tokens or metadata.

## 9. Sync And Event Replay

Review `routes/sync.rs` and `ws.rs`.

- Bootstrap must return encrypted records only.
- WebSocket auth must be identical to HTTP auth.
- Bootstrap, list, download, delete, and WebSocket replay/broadcast must be scoped by authenticated `user_id`.
- Event metadata should not leak plaintext content.
- Event ordering must not allow clients to miss their own user's creates/deletes silently.

## 10. Tests Worth Having

Prioritize tests where failure is a security bug:

- Invalid object IDs are rejected before disk access.
- Authenticated device ID overrides spoofed request body IDs.
- Duplicate IDs do not overwrite existing blobs.
- Oversized uploads are rejected.
- Blob size/hash mismatches are rejected and partial files are removed.
- Registration rejects missing, invalid, expired, reused, or malformed access keys.
- Registration stores OPAQUE verifier material without receiving the raw passphrase.
- Login rejects invalid, expired, reused, or malformed OPAQUE challenge finalizations.
- User A cannot list, download, delete, bootstrap, or receive WebSocket events for User B's objects.

Avoid broad fixture-heavy tests unless they protect a real invariant.
