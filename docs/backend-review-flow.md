# Backend Review Flow

Use this document to rebuild server context before reviewing security-sensitive
changes. The frontend, daemon, desktop app, and future mobile apps should be
treated as untrusted clients.

## 1. Architecture Map

The backend is the coordination and storage service. Clients do crypto locally,
then send encrypted records and opaque metadata to the server.

- `crates/server/src/main.rs`
  - boots the Axum HTTP server;
  - runs migrations;
  - checks `server_config` exists;
  - wires public routes, authenticated routes, tracing, cleanup, and rate-limit pruning.
- `crates/server/src/routes/auth.rs`
  - handles login challenge creation, challenge/proof login, device registration/update, session creation, and logout.
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
- `crates/core/src/models.rs`
  - defines the HTTP/WebSocket API contracts shared by server and clients.
- `crates/client/src/api_client.rs`
  - is the reference client implementation and should be checked when route models or crypto flow change.

SQLite stores durable metadata. The filesystem stores larger encrypted blobs:

- `server_config`
  - one row;
  - stores `auth_salt`, server-side `auth_hash`, and `enc_salt`;
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

## 2. Expected Application Flow

First-run server setup:

1. Operator runs `clipper-server init`.
2. Server generates auth and encryption salts.
3. Server stores the passphrase-derived auth verifier in `server_config`.
4. Server creates `clipboard/` and `files/` data directories.

Normal client login:

1. Client calls `POST /api/auth/challenge`.
2. Server returns a random challenge ID, random challenge nonce, auth salt, encryption salt, and KDF parameters.
3. Client derives the auth hash locally from the passphrase and auth salt.
4. Client sends `POST /api/auth/login` with challenge ID, HMAC proof, and device info.
5. Server consumes the single-use challenge, verifies the proof against its stored auth hash, creates/updates the device row, and returns a bearer token.
6. Client uses `Authorization: Bearer <token>` on private HTTP routes and WebSocket connections.

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
- Confirm what the server is allowed to know:
  - It may know device IDs, timestamps, ciphertext sizes, event IDs, upload status, encrypted metadata, and ciphertext hashes.
  - It must not receive plaintext clipboard contents, plaintext file bytes, plaintext file metadata, raw passphrases, or reusable client-side encryption keys.
- Confirm whether the route handles attacker-controlled input from a client, even if authenticated.
- Treat SQLite rows and blob filenames as security-sensitive because many routes convert DB values into filesystem paths.

## 4. Authentication And Sessions

Review `routes/auth.rs`, `auth.rs`, `state.rs`, and `rate_limit.rs` first.

- Public auth routes are `POST /api/auth/challenge` and `POST /api/auth/login`.
- Login must use the challenge/proof flow; raw passphrases and reusable auth hashes must not be sent to the server.
- Challenge IDs must be random, short-lived, and single-use.
- Session tokens must be random, stored server-side only as hashes, and required on all private routes.
- Expired sessions must fail closed.
- Logout should delete only the authenticated session.
- Rate limiting must apply to login attempts and must not trust spoofable proxy headers unless deployment config guarantees a trusted proxy.

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
- Nonces must be random and never reused with the same key.
- Server responses must never include plaintext content.
- Server-side logs and errors must not include decrypted data, passphrases, auth proofs, or bearer tokens.
- The server still stores an auth verifier, so weak passphrases remain vulnerable to offline guessing by anyone with DB access. Strong passphrases still matter.
- TLS is still required in real deployments; the challenge/proof flow avoids sending the raw passphrase but does not make plain HTTP safe for bearer tokens or metadata.

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
- Login rejects invalid, expired, reused, or malformed challenge proofs.
- WebSocket replay gap behavior either replays complete history or forces bootstrap.

Avoid broad fixture-heavy tests unless they protect a real invariant.
