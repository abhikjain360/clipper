# Backend Review Flow

Use this document to rebuild server context before reviewing security-sensitive
changes. The frontend, daemon, desktop app, Android app, web client, and any
future clients should be treated as untrusted clients.

## 1. Architecture Map

The backend is the coordination and storage service. Clients do crypto locally,
then send encrypted records and non-decrypted metadata to the server.

- `crates/server/src/main.rs`
  - boots the Axum HTTP server;
  - loads built-in defaults, optional TOML config, environment overrides, and
    CLI overrides;
  - loads the required server pepper from `CLIPPER_SERVER_SECRET` or
    `CLIPPER_SERVER_SECRET_FILE`;
  - runs migrations;
  - checks `server_config` exists and verifies the supplied pepper can unwrap
    server configuration before binding;
  - wires public registration/login routes, authenticated routes, tracing,
    cleanup, rate-limit pruning, and typed process-level error handling.
- `crates/server/src/error.rs`
  - owns server process error variants, display text, and exit-code mapping.
- `crates/server/src/config.rs`
  - owns server runtime config defaults, TOML/CLI override shapes, and `garde` validation rules.
- `crates/server/src/routes/auth.rs`
  - handles invite-key-gated OPAQUE registration, OPAQUE login challenge creation/finalization, device registration/update, session creation, and logout.
- `crates/server/src/auth.rs`
  - validates bearer tokens and injects authenticated `user_id`/`device_id`/`session_id` into request handlers.
- `crates/server/src/routes/objects.rs`
  - handles generic encrypted objects: multi-payload init, streamed payload upload, completion, listing, payload download, and delete. Both clipboard items and files flow through this route — there is no longer a separate clipboard or files route. Clipboard items are `kind = "clipboard"` with a TTL and a per-user `max_items` trim; files are `kind = "file"`.
- `crates/server/src/routes/health.rs`
  - unauthenticated `GET /api/health` liveness probe.
- `crates/server/src/routes/sync.rs` and `crates/server/src/ws.rs`
  - provide bootstrap state and event notification. `bootstrap` returns device info, `latest_seq`, and an empty reserved `ServerInfo`; the client rebuilds clipboard/file state from `GET /api/objects`. WebSocket replay reads `event_log` and falls back to `Invalidate` only when the replay query errors — it does not yet detect pruned-event gaps or cap replay size (see `docs/rust-code-review.md` P1).
- `crates/server/src/state.rs`
  - owns shared DB connection, data directories, WebSocket broadcast channel, in-memory auth challenges, and in-memory pending registrations.
- `crates/server/src/cleanup.rs`
  - periodic loop plus on-write trims, all over the `objects` table: `cleanup_expired_clipboard_objects` (clipboard past TTL), `cleanup_excess_clipboard_objects` / `trim_user_clipboard` (clipboard beyond per-user `max_items`), `cleanup_old_events` (event-log retention), and `cleanup_orphan_object_uploads` (non-`complete` objects past `orphan_upload_ttl_secs`). Each deletes the on-disk payload files before the rows.
- `crates/api-types/src/lib.rs`
  - owns the HTTP/WebSocket API contracts shared by server and clients. Object endpoints exchange `postcard`-encoded bodies (`POSTCARD_CONTENT_TYPE`); auth endpoints use JSON with base64 fields.
- `crates/core/src/models.rs`
  - compatibility re-exports `clipper-api-types` for existing imports.
- `crates/client/src/api_client.rs`
  - is the reference client implementation and should be checked when route models or crypto flow change.
- `crates/client/src/engine.rs`
  - owns client-side auth completion, key derivation, encryption/decryption, HTTP/WebSocket sync, decrypted in-memory state, and adapts the multi-payload object API to the single-payload clipboard/file behavior the UI assumes.
- `crates/client/src/local_store.rs`
  - per-profile, filesystem-backed durable clipboard cache (roadmap step 1). Stores clipboard payloads and metadata as plaintext files under the profile root; the network boundary stays encrypted. File metadata is not yet cached here (still server-derived).
- `crates/app-types/src/lib.rs`
  - owns decrypted app-visible state shared by the sync engine, daemon state events, and Flutter bridge adapters.
- `docs/local-store-p2p-roadmap.md`
  - describes the planned client-side local store, on-demand file cache, signed object envelopes, and explicit-pairing LAN P2P transport.
- `crates/daemon-types/src/protocol.rs`
  - owns the daemon/app IPC request, response, event, command, parameter, and result shapes, plus the HMAC IPC auth message format.
- `app/rust/src/runtime.rs`
  - selects the Flutter bridge runtime:
    - macOS and Linux talk to the local `clipper-daemon` over a Unix socket and authenticate with an HMAC handshake;
    - Android and the WASM web build both run the shared Rust `SyncEngine` in-process and talk to the server directly.

Current app client paths:

- macOS/Linux Flutter app -> Rust FRB bridge -> local daemon -> `clipper-client` -> server.
- Android Flutter app -> Rust FRB bridge -> in-process `clipper-client::SyncEngine` -> server.
- Web Flutter app (WASM) -> Rust FRB bridge -> in-process `clipper-client::SyncEngine` -> server.

Android system integration, such as writing sensitive clipboard contents to the
local OS clipboard, stays in Flutter/Kotlin through a `MethodChannel`. The Rust
client should stay portable and should not need JNI for Android framework calls.

SQLite stores durable metadata. The filesystem stores larger encrypted blobs:

- `server_config`
  - one row;
  - marks server initialization and stores the server-wide access-key hashing salt;
  - new users' OPAQUE material lives in `users`, not in this table.
- `access_keys`
  - stores invite/access keys as server-salted Argon2id verifiers, never as plaintext;
  - tracks creation, optional expiry, and one-time use by a user.
- `users`
  - stores per-user `opaque_server_setup`, OPAQUE password file/verifier, encryption salt, and `access_key_hash` (FK to the consumed access key);
  - does not store the raw passphrase.
- `sessions`
  - stores user-scoped random session token hashes, not bearer tokens themselves; also captures user-agent and IP for audit.
- `devices`
  - stores user ID, device ID, device name, platform, and last-seen timestamps.
- `objects`
  - per-object header: user, kind (`clipboard` | `file`), encrypted metadata (`meta_ciphertext` + `meta_nonce`), source device, `expires_at` (set for clipboard, null for files), status (`pending` | `complete`). There is no longer a separate `clipboard_items` table.
- `object_payloads`
  - per-payload row keyed by `(object_id, payload_id)`: ciphertext path, nonce, ciphertext size, SHA-256, status (`pending` | `uploading` | `uploaded` | `complete`). Each row points at a file under `state.objects_dir()`.
- `event_log`
  - stores user-scoped ordered object events used by bootstrap/WebSocket replay. A check constraint pins event types to `created` or `deleted`, and allows `deleted` only for file objects.

Database schema source of truth lives in `crates/server/src/migration/*.rs`;
SeaORM entity files under `crates/server/src/entity/` are generated from that
schema and should not be treated as the schema owner.

## 2. Expected Application Flow

First-run server setup:

1. Operator generates a 32-byte server pepper with
   `clipper-server generate-secret` and stores it outside the database backup
   path.
2. Operator sets exactly one of `CLIPPER_SERVER_SECRET` or
   `CLIPPER_SERVER_SECRET_FILE`.
3. Operator runs `clipper-server init`.
4. Server creates the database, runs migrations, inserts the singleton
   `server_config` initialization marker and a wrapped access-key hashing salt,
   and creates the `objects/` data directory (the only on-disk blob directory).
5. No user passphrase is entered during server init.

Example:

```sh
test -f /path/outside/db-backups/clipper.secret || \
  clipper-server generate-secret > /path/outside/db-backups/clipper.secret
chmod 600 /path/outside/db-backups/clipper.secret
export CLIPPER_SERVER_SECRET_FILE=/path/outside/db-backups/clipper.secret
clipper-server init --data-dir /path/to/data-dir
```

Access-key provisioning:

1. Operator creates one high-entropy access key per intended user outside the app.
2. Operator adds the access key with `clipper-server add-access-key` while
   providing the same server pepper. The command unwraps
   `server_config.access_key_hash_salt`, hashes with Argon2id using the server
   pepper as Argon2's `secret` input, and stores the base64 verifier in
   `access_keys.key_hash`, with `created_at` set and optional `expires_at`.
3. The access key is a one-time registration authorization secret only. It is
   not the user's passphrase and is not used for data encryption.

Example:

```sh
export CLIPPER_SERVER_SECRET_FILE=/path/outside/db-backups/clipper.secret
ACCESS_KEY="$(openssl rand -base64 32)"
printf 'Access key: %s\n' "$ACCESS_KEY"
clipper-server add-access-key --data-dir /path/to/data-dir
```

The `add-access-key` command prompts for the raw key when `--access-key` is not
supplied, so the invite does not need to be passed as a process argument. Paste
the generated key at the prompt, then give it to the user out of band.

Normal client registration:

1. Client starts OPAQUE registration locally from the user's chosen passphrase.
2. Client calls `POST /api/auth/register/start` with the access key, the chosen `username`, and OPAQUE registration request. Usernames are lowercase ASCII letters/digits/`_-`, 3–32 chars; the server rejects taken usernames at this step.
3. Server hashes the access key, verifies it exists, is unused, and is unexpired, then creates a pending registration with a new `user_id`, the `username`, and per-user OPAQUE server setup.
4. Server returns the OPAQUE registration response, `registration_id`, and `user_id`.
5. Client finishes OPAQUE registration locally, derives its object-encryption key from OPAQUE's `export_key`, and sends `POST /api/auth/register/finish` with the registration upload and device info.
6. Server consumes the pending registration, re-checks and marks the access key used, stores the per-user OPAQUE password file/verifier (and `username`) in `users` under a unique constraint, creates/updates the first device row, and returns a bearer token plus `user_id` and `username`.

Normal client login:

1. Client starts OPAQUE locally from the passphrase.
2. Client calls `POST /api/auth/challenge` with `username` and an OPAQUE credential request. The server looks up the user by username; unknown usernames return `401`.
3. Server starts OPAQUE from that user's stored password file/verifier (OPAQUE `id_U` is bound to the immutable UUID, not the username), stores short-lived server login state under a random challenge ID, and returns the OPAQUE credential response.
4. Client finishes OPAQUE locally, derives its object-encryption key from OPAQUE's `export_key`, and sends `POST /api/auth/login` with challenge ID, OPAQUE credential finalization, and device info.
5. Server consumes the single-use challenge, finishes OPAQUE, creates/updates a device row for that user, creates a user-scoped session, and returns a bearer token plus `user_id` and `username`.
6. Client uses `Authorization: Bearer <token>` on private HTTP routes and WebSocket connections.

Client runtime notes:

- macOS and Linux registration and login requests are sent to the daemon, which uses the shared Rust client engine.
- Android and web registration and login requests are handled by the Rust client engine inside the Flutter app process.
- The Flutter auth screen exposes separate Register and Login modes. Register requires an access key, username, and passphrase; Login requires username and passphrase. The most recently used username is prefilled from the saved profile.
- All paths must produce the same server-facing OPAQUE, bearer-token, encryption, sync, and object/clipboard behavior.
- Android emulator development uses `http://10.0.2.2:8787` for host loopback. Production and physical-device deployments should use HTTPS.

Object upload/download flow (used for both clipboard items and files):

1. Client encrypts metadata locally (`AAD_CLIPBOARD_META_V1` or `AAD_FILE_META_V1`).
2. Client encrypts each payload locally (`AAD_CLIPBOARD_PAYLOAD_V1` or `AAD_FILE_BLOB_V1`). Small payloads (≤ 64 KiB in the current client) may be sent inline in init; larger payloads are uploaded separately.
3. Client calls `POST /api/objects/init` with a postcard-encoded `ObjectInitRequest`: a UUID `id`, `kind`, encrypted metadata nonce + ciphertext, and one or more `payloads` (each with a per-payload UUID `id`, nonce, declared ciphertext size, SHA-256, and optional `inline_ciphertext`).
4. Server validates metadata size against `limits.max_object_meta_ciphertext_bytes`, writes any inline payload ciphertext directly to `objects/<object_id>.<payload_id>.bin` with `create_new` semantics, inserts the `objects` row and one `object_payloads` row per declared payload, and logs/broadcasts a `*.created` event only when every payload was inline (i.e. the object is fully complete after init).
5. Server response (`ObjectInitResponse`) lists `upload_urls` for each non-inline payload (`/api/objects/{object_id}/payloads/{payload_id}`) and a `complete` flag set when init alone finished the object.
6. For each non-inline payload, the client calls `PUT /api/objects/{object_id}/payloads/{payload_id}` with raw ciphertext bytes. Only the authenticated source device may upload. The server claims the payload (`pending` → `uploading`), streams bytes into a `*.tmp` file, refuses any byte count above the initialized size, renames into the final path, and marks the payload `uploaded`.
7. After uploading every non-inline payload, the client calls `POST /api/objects/{object_id}/complete` with a postcard `ObjectCompleteRequest` covering every payload. The server requires the source device to match, re-hashes each on-disk payload, verifies size and SHA-256 against both the init metadata and the completion request, marks every payload `complete`, transitions the object from `pending` to `complete`, inserts a `*.created` event row, and broadcasts it to that user's WebSocket subscribers.
8. List, download, and delete are user-scoped:
   - `GET /api/objects?kind=...&limit=...&created_seq_lte=...&after_created_seq=...&after_id=...` returns `complete` objects ordered by server create sequence, including each object's payload descriptors. The query enforces `limit + 1` at SQL level and exposes a typed `next_after` cursor.
   - `GET /api/objects/{id}/payloads/{payload_id}` streams a `complete` payload's ciphertext from disk.
   - `DELETE /api/objects/{id}` is restricted to `kind = "file"`. It deletes the row, all payload files, logs a `deleted` file event, and broadcasts that event. Clipboard objects cannot currently be deleted through this route.

Planned client-side local-store behavior keeps this same abstraction: object
metadata may be cached locally, but payload bytes are still downloaded only on
user request. Future LAN P2P must follow the same on-demand payload rule.

Sync flow:

1. After login, clients open a WebSocket and send `hello`.
2. Native clients connect to `GET /api/ws` through the normal authenticated
   router, so `auth_middleware` extracts `Authorization: Bearer ...` and injects
   `AuthInfo`.
3. Browser clients cannot set arbitrary WebSocket upgrade headers. They first
   call authenticated `POST /api/ws-ticket` over HTTP, then connect to public
   `GET /api/ws-ticket/connect` with WebSocket subprotocols
   `clipper-ticket` and the short-lived ticket. The server consumes that ticket
   before upgrading and then calls the same socket handler with the recovered
   `AuthInfo`.
4. Server replies with `hello_ack { server_time, stream_start_seq }`, where
   `stream_start_seq` is the current per-user event-log high-water mark.
5. The client snapshots files and clipboard objects from `GET /api/objects`
   using `created_seq_lte = stream_start_seq`, then applies live WebSocket
   events with `seq > stream_start_seq`.
6. If a client lags behind the broadcast buffer, the server sends
   `Invalidate { target: "all" }` and closes the socket so the client reconnects
   and snapshots again.
7. Live broadcasts are scoped to the authenticated user and skip events
   originated by the same authenticated device.

Cleanup flow:

1. Clipboard objects past their `expires_at` TTL are deleted with their payload files.
2. Clipboard objects beyond the per-user `clipboard.max_items` cap are trimmed (oldest first); this also runs opportunistically after each clipboard write.
3. Old event-log rows are pruned, which can force clients to bootstrap or refresh instead of replaying old gaps.
4. Pending or uploading objects older than `cleanup.orphan_upload_ttl_secs` are deleted with their payload files.

## 3. Security Model To Rebuild Before Review

- Identify which routes are public and which routes require `auth_middleware`.
- Identify which client runtime path is affected: macOS/Linux daemon, Android in-process engine, web in-process engine, or all of them.
- Confirm what the server is allowed to know:
  - It may know user IDs, device IDs, timestamps, ciphertext sizes, event IDs, upload status, encrypted metadata, ciphertext hashes, and access-key hashes.
  - It must not receive plaintext clipboard contents, plaintext file bytes, plaintext file metadata, raw passphrases, plaintext access keys after registration request processing, or reusable client-side encryption keys.
- Confirm whether the route handles attacker-controlled input from a client, even if authenticated.
- Treat SQLite rows and blob filenames as security-sensitive because routes convert DB values into filesystem paths (`object_payloads.ciphertext_path`).
- Treat client-supplied `platform` and `device_name` values as display/provenance metadata only.

## 4. Authentication And Sessions

Review `routes/auth.rs`, `auth.rs`, `state.rs`, and `rate_limit.rs` first.

- Public auth routes are `POST /api/auth/register/start`, `POST /api/auth/register/finish`, `POST /api/auth/challenge`, and `POST /api/auth/login`.
- Registration must be gated by a one-time access key stored as an Argon2id verifier. The verifier is derived from the access key, the unwrapped server salt, and the server pepper passed as Argon2's `secret` input.
- Registration and login must use OPAQUE flows; raw user passphrases and reusable authentication secrets must not be sent to the server.
- Usernames are enumerable: `challenge` returns `401 "Unknown user"` for a missing username without doing OPAQUE work, and `register_start` returns `409 "Username already taken"`. Because `opaque_server_setup` is per-user, the OPAQUE `fake_sk` enumeration mitigation is not exercised. Treat this as an accepted tradeoff of the username-based design, not as something `fake_sk` defends (see `docs/opaque.md` and `docs/rust-code-review.md` P3).
- Challenge IDs must be random, short-lived, and single-use. Pending registration IDs must be random, short-lived, and single-use. Both maps in `AppState` cap themselves at `auth.max_pending_challenges`; the eviction order under that cap is HashMap iteration order (effectively arbitrary), so high pressure can drop fresh entries.
- Session tokens must be random, stored server-side only as hashes (`sha256(token)`), and required on all private routes.
- WebSocket tickets are browser compatibility credentials only. `POST /api/ws-ticket` must require a normal bearer session, issue a high-entropy ticket, store only `sha256(ticket)` in memory with the associated `AuthInfo`, cap tickets at `auth.max_pending_ws_tickets` (separate from the OPAQUE challenge cap so the two DoS surfaces tune independently), and make tickets short-lived and single-use. Because all tickets share a fixed TTL, eviction under the cap drops the oldest (smallest `expires_at`) first, so a burst of new tickets cannot displace one that is about to connect. Do not persist tickets or accept them for non-WebSocket APIs.
- Expired sessions must fail closed; `auth_middleware` rejects when `sess.expires_at < now_rfc3339`.
- Logout should delete only the authenticated session.
- Auth rate limiting must apply to registration starts/finishes, OPAQUE challenge starts, and login finalizations. It is `governor`-backed, keyed by resolved client IP, configurable from TOML/CLI, and also has a global auth cap.
- Forwarded client IP headers must only be honored when the immediate peer matches startup trusted-proxy config (`trusted_proxies`, `--trusted-proxy`, or `CLIPPER_TRUSTED_PROXIES`). `client_ip_from_headers` walks `X-Forwarded-For` / `Forwarded` / `X-Real-IP` and returns the rightmost untrusted hop.
- All authenticated handlers must use the `user_id`, `device_id`, and `session_id` injected by `auth_middleware` for authorization and data filtering.

## 5. Object And Path Safety

Review every route that reads, writes, or deletes files.

- Client-provided object and payload IDs must be validated to 36-character UUIDs before becoming filenames (`routes::validate_client_id`). The `objects` route builds filenames as `{object_id}.{payload_id}.bin`; do not relax that.
- File paths must be built from the server-controlled directory (`state.objects_dir()`) plus validated filenames only.
- Uploads must not overwrite existing payloads accidentally. `init_object` writes inline payloads with `create_new`, and streaming uploads write to a per-attempt `.tmp` path before atomically renaming over the final filename.
- Failed database writes after file writes must clean up partial files; `init_object` and the streaming upload paths both call `remove_paths`/`remove_file` on the rollback branches.
- Delete and download routes must reject invalid IDs before touching storage and must scope by `user_id`. The current `delete_object` additionally rejects `kind = "clipboard"`.
- DB-stored paths should be treated as tainted unless they were generated by the server from validated IDs.

## 6. Upload Limits And Streaming

For object and clipboard ingestion:

- Per-payload streaming upload
  (`PUT /api/objects/{id}/payloads/{payload_id}`) refuses any byte beyond the
  `ciphertext_size` declared during init, then rejects the request if the final
  byte count does not match. `init_object` rejects any declared payload
  `ciphertext_size` above `limits.max_file_blob_bytes`, so both inline and
  streamed writes are bounded by the same configured ceiling.
- `init_object` validates encrypted metadata against `limits.max_object_meta_ciphertext_bytes`, and `garde` now validates that each inline payload's length equals its declared `ciphertext_size` and matches `sha256_ciphertext`. There is no explicit `DefaultBodyLimit`, so buffered (`init`/`complete`) routes are bounded only by axum's implicit 2 MiB request-body default — a framework default, not an explicit policy.
- Enforce that uploaded blob size matches initialized metadata. Hash large blobs incrementally on disk before completion.
- Delete corrupt or mismatched partial blobs.
- Prefer status transitions that cannot create visible half-complete objects (`pending` → `uploading` → `uploaded` → `complete` per payload, gated by `object_for_upload` device scoping).

## 7. Authenticated Device Attribution

- Do not trust `source_device_id` from request bodies.
- Use the device ID injected by `auth_middleware`. `objects::init_object` and the legacy `clipboard::upload` both ignore body provenance fields and store `auth.device_id`.
- Scope all reads, writes, deletes, sync bootstrap responses, and WebSocket events by the authenticated `user_id`.
- `object_for_upload` requires `object.source_device_id == auth.device_id` for payload upload and completion. If a route mutates an existing object, verify that cross-device mutation is intentional.
- Device IDs in DB records should be useful for provenance but must not be treated as proof of authorization unless they came from the session.

## 8. Encryption Boundary

Review `clipper-core` and `clipper-client` when server API shapes change.

- Clipboard text and clipboard payload bytes must be encrypted client-side before upload (`AAD_CLIPBOARD_META_V1` for metadata, `AAD_CLIPBOARD_PAYLOAD_V1` for the payload). `AAD_CLIPBOARD_V1` is a leftover from the removed single-blob path and is now unused outside tests.
- File metadata and file blobs must be encrypted separately client-side (`AAD_FILE_META_V1`, `AAD_FILE_BLOB_V1`).
- macOS, Linux, Android, and web should share the same Rust encryption and sync path through `clipper-client`; platform-specific code should only handle local OS integration.
- Nonces must be random and never reused with the same key. `garde` rules enforce that nonces are 24 bytes and SHA-256 fields are 32 bytes at the API edge; `crypto::decrypt` rejects malformed nonce lengths before delegating to `chacha20poly1305`.
- AAD currently binds only the object type/version (e.g. `clipper:clipboard-payload:v1`). It does not bind object ID, payload ID, source device, timestamps, or the metadata/payload relationship; see `docs/rust-code-review.md` P1.
- Server responses must never include plaintext content.
- Server-side logs and errors must not include decrypted data, passphrases, OPAQUE messages, or bearer tokens.
- Server process errors should stay typed and composable. Do not add direct `anyhow` usage or forced stderr printing; log through `tracing` and let the entrypoint exit with the mapped error code.
- The server stores per-user OPAQUE password files/verifiers. DB-only dumps cannot test passphrase guesses without the server pepper, but DB+pepper or live-server compromise degrades to offline guessing. A verifier must not be usable directly as a login secret. Strong passphrases still matter.
- Access keys are authorization invites, not encryption keys. They should still be high entropy even though the DB stores only Argon2id verifiers, because possession of an unused access key permits account registration.
- TLS is still required in real deployments; OPAQUE avoids sending the raw passphrase but does not make plain HTTP safe for bearer tokens or metadata.

## 9. Sync And Event Replay

Review `routes/sync.rs` and `ws.rs`.

- Bootstrap must return encrypted records only.
- WebSocket auth must be identical to HTTP auth.
- Bootstrap, list, download, delete, and WebSocket replay/broadcast must be scoped by authenticated `user_id`.
- Event metadata should not leak plaintext content.
- Event ordering must not allow clients to miss their own user's creates/deletes silently. The server only sends `Invalidate` when the replay query errors, not when `last_seq` is older than the retained `event_log` window; see `docs/rust-code-review.md` P1.

## 10. Tests Worth Having

Prioritize tests where failure is a security bug:

- Invalid object or payload IDs are rejected before disk access.
- Authenticated device ID overrides spoofed request body IDs.
- Duplicate object IDs do not overwrite existing payloads.
- Oversized payload uploads are rejected; declared sizes during init are bounded.
- Payload size/hash mismatches are rejected and partial files are removed.
- Registration rejects missing, invalid, expired, reused, or malformed access keys.
- Registration stores OPAQUE verifier material without receiving the raw passphrase.
- Login rejects invalid, expired, reused, or malformed OPAQUE challenge finalizations.
- User A cannot list, download, delete, bootstrap, or receive WebSocket events for User B's objects.
- Declared `ciphertext_size` (and inline length) are bounded by `limits.max_file_blob_bytes` before a payload is accepted or streamed.
- Crypto config validation rejects insecure floors (tiny session tokens/salts, trivial Argon2 parameters).

Avoid broad fixture-heavy tests unless they protect a real invariant.
