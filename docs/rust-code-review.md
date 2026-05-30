# Rust Code Review Findings

Original review: 2026-05-25.
Re-verification: 2026-05-25 (this revision). The re-verification was static and
did not run `cargo test` or `cargo clippy`.

Scope reviewed:

- `crates/core`
- `crates/server`
- `crates/client`
- `crates/daemon`
- `crates/daemon-types`
- `app/rust`

Each finding records both the historical status and the verified status against
the current tree. Some items are intentional product tradeoffs or future-facing
risks, but they are still tracked here so the security and compatibility
decisions stay explicit.

## Overall Read

The crate boundaries are sound:

- `clipper-api-types` owns HTTP and WebSocket payloads.
- `clipper-daemon-types` owns daemon IPC.
- `clipper-app-types` owns decrypted app-visible state.
- `app/rust` bridge structs are thin adapter types.

The server auth model is directionally good. Passphrases go through OPAQUE,
session tokens are random and stored as hashes, and private routes derive
`user_id` and `device_id` from authenticated middleware instead of trusting
request body provenance. The object upload state machine is also a good base:
per-payload `pending` -> `uploading` -> `uploaded` -> `complete`, gated by the
authenticated source device, with hash and size validation on the disk file
before completion.

The highest-priority remaining risks are missing request-body and init-time
size limits on the new object route, AAD that still does not bind object
metadata, WebSocket replay gaps when events have been pruned, and the dual
clipboard write path that is now dead but still mounted.

## P0: Local Daemon IPC Is Too Powerful For A Bare Unix Socket

Status: addressed for the current local IPC threat model. Verified.

`crates/daemon/src/main.rs` requires the platform data directory (creates
`~/Library/Application Support/Clipper` with `0700`, socket file with `0600`)
and refuses to use `/tmp`. `crates/daemon/src/handler.rs` caps each IPC line at
`MAX_IPC_REQUEST_LINE_BYTES = 4 MiB` and runs an HMAC-SHA256 handshake against a
Keychain-stored 32-byte secret (`ipc_auth_message` binds protocol version,
daemon nonce, and client nonce) before accepting any command or sending state.
`app/rust/src/transport.rs` and `app/rust/src/ipc_auth.rs` mirror that protocol
on the client side and also cap inbound lines.

Remaining hardening to consider:

- If same-user malware with Keychain or process-memory access is in scope,
  stronger platform identity controls such as signing/sandboxing/entitlements
  are needed.
- Consider separate authorization for commands that can read/write arbitrary
  local files (`UploadFile { file_path }`, `DownloadFile { target_path }`).

## P1: Malformed Nonce Input Can Crash Clients

Status: fixed. Verified.

`crypto::decrypt` now rejects any nonce whose length is not
`XCHACHA20_NONCE_BYTES` before calling `XNonce::from_slice`
(`crates/core/src/crypto.rs`). The API types enforce
`length(equal = XCHACHA20_NONCE_BYTES)` and `length(equal = SHA256_BYTES)` via
`garde` for both clipboard (`ClipboardUploadRequest`) and object payload
(`ObjectPayloadInit`, `ObjectPayloadComplete`) requests, so the server rejects
malformed values before any disk write. Tests in `routes::clipboard::tests`,
`routes::objects::tests`, and `crypto::tests` cover the wrong-length cases.

Keep the invariants:

- XChaCha20-Poly1305 nonce length is 24 bytes.
- SHA-256 fields are 32 bytes.
- Malformed server records must become decrypt errors, not process panics.

## P1: Encrypted Payload Authentication Does Not Bind Object Metadata

Status: open. Verified.

AAD constants remain type-level only: `clipper:clipboard:v1`,
`clipper:clipboard-meta:v1`, `clipper:clipboard-payload:v1`,
`clipper:file-meta:v1`, `clipper:file-blob:v1`. The new object split made the
mix-up risk worse: an object header now decouples metadata (`AAD_*_META_V1`)
from payload bytes (`AAD_*_PAYLOAD_V1` / `AAD_FILE_BLOB_V1`), but AAD never
references the object ID, payload ID, source device, timestamps, or the
metadata/payload pairing. A malicious or buggy server/relay can still mix
ciphertext, nonces, IDs, timestamps, device attribution, and metadata/payload
pairs without AEAD detecting the metadata tamper.

Recommended fix:

- Bind stable object metadata into AAD (at minimum: object ID, payload ID,
  object kind, source device).
- For future P2P, signed object envelopes as described in
  `docs/local-store-p2p-roadmap.md` should bind object ID, object kind, source
  device, created time, operation type, payload set, ciphertext hash, and
  version.

## P1: WebSocket Replay Can Silently Miss Pruned Events

Status: partially addressed. Verified.

`ws::handle_socket` now sends `Invalidate { target: "all" }` when
`get_events_since` errors, and `SyncEngine::ws_connect` reacts to that by
running a full `refresh()`. The remaining gap: the server still does not know
the oldest retained sequence per user, so a successful query that returns rows
younger than the actual `last_seq` is treated as a complete replay. Cleanup
runs every `cleanup.interval_secs` (default 1 h) and prunes events older than
`event_log_retention_days` (default 3 d), so a client that misses that window
silently advances past pruned events.

The client side also still advances `last_seq` from the WS event itself before
running `refresh()`. If `refresh()` fails, the next reconnect will skip the
failed event.

Recommended fix:

- Track oldest available event sequence per user and send `Invalidate` when
  `last_seq` is older than the retained range.
- Cap replay count; if the gap is too large, send `Invalidate`.
- Advance client `last_seq` only after the refresh/bootstrap caused by the
  event succeeds, or persist an explicit "needs bootstrap" state.

## P1: Object Init Has No Size Cap (New Finding)

Status: open. Introduced with the multi-payload object route.

`routes::objects::init_object` only validates `meta_ciphertext` against
`limits.max_object_meta_ciphertext_bytes`. It does not check:

- `payload.ciphertext_size` declared during init (only `garde(range(min = 0))`,
  no upper bound). A client can declare a payload of `i64::MAX` bytes and the
  streaming upload will then accept up to that many bytes.
- The size of any `inline_ciphertext` carried inside the init request. Inline
  payloads are written straight to disk with `create_new`, and there is no
  Axum body limit middleware on the route, so a single `POST /api/objects/init`
  can write arbitrarily large attacker-controlled bytes to `objects/`.
- The aggregate size of multiple inline payloads in one request.
- `limits.max_file_blob_bytes` against any payload at all.

This regresses the file-blob limit that the legacy `/api/files/blob` path was
designed around.

Recommended fix:

- Validate each declared `payload.ciphertext_size` against
  `limits.max_file_blob_bytes` during init.
- Validate every `inline_ciphertext.len()` against the same cap (and reject
  inline payloads that should have been streamed, e.g. enforce a small inline
  cap such as the client's 64 KiB threshold).
- Add an Axum `DefaultBodyLimit` for the object init/complete routes that is
  comfortably larger than the inline cap but bounded.

## P1/P2: Request And Payload Size Limits Are Incomplete

Status: partially in progress. Verified.

Streaming object payload upload (`PUT /api/objects/{id}/payloads/{payload_id}`)
enforces the declared size during the stream and rejects mismatches, which is
good. Other paths are still loose:

- Clipboard upload (legacy `/api/clipboard`) still decodes the JSON/base64
  ciphertext into memory and has no application-level cap.
- `ObjectInitRequest` is not size-capped (see the dedicated P1 entry above).
- Device name, platform, access key, and OPAQUE payload fields have only
  `length(min = 1)` checks — no upper caps.
- Daemon IPC request lines are capped at 4 MiB on both sides.

Recommended fix:

- Add Axum `DefaultBodyLimit` per-route for JSON and postcard routes.
- Add application-level caps for clipboard ciphertext, encrypted metadata,
  device name, platform, access key, and OPAQUE payload sizes.
- Keep the streamed object payload path.
- Align reverse proxy request-size limits with server limits.

## P2: Legacy Clipboard Route Is Still Mounted But Unused (New Finding)

Status: open. Introduced when the client moved to the object route.

`SyncEngine::send_clipboard_payload` now creates clipboard items via
`ObjectInitRequest { kind: Clipboard, .. }`, and
`SyncEngine::fetch_object_state` lists clipboard items via
`list_objects(Some(ObjectKind::Clipboard), ..)`. The bootstrap response field
`clipboard_items` (sourced from the legacy `clipboard_items` table) is read into
a local but never used. The Axum router still wires `POST /api/clipboard`,
`GET /api/clipboard`, and bootstrap still queries `clipboard_items`; the only
caller is the test suite.

This is a parallel write path that is currently dormant. Risks:

- If anything (a future build, an old client, a misconfigured proxy, a test
  utility) writes through the legacy route, those rows will not show up in the
  client UI and the server will retain plaintext-shaped paths and orphaned
  blobs.
- Bootstrap continues to read all rows then truncate to 100, wasting work and
  carrying the P2 fetch-then-truncate cost forever.
- Two cleanup paths (`cleanup_expired_clipboard` and
  `cleanup_orphan_object_uploads`) walk two different tables.

Recommended fix:

- Either remove the legacy route + table + bootstrap field + cleanup branch,
  or document the legacy route as a deprecated compatibility shim and rate-
  limit / disable it for new deployments. Pick one; do not leave a dual write
  path live.

## P2: List And Bootstrap Queries Fetch All Rows Before Truncating

Status: partially addressed. Verified.

`routes::objects::list_objects` applies `.limit(limit + 1)` at SQL level.
`routes::clipboard::list` and `routes::sync::bootstrap` still call
`.all(state.db())` and truncate in Rust. No DB indexes have been added for the
expected access patterns either; the only indexes today are primary keys and a
few `UNIQUE` constraints on `ciphertext_path` / `token_hash` /
`access_key_hash`.

Recommended fix:

- Apply SQL `limit + 1` to the remaining list/bootstrap reads.
- Add DB indexes:
  - `clipboard_items(user_id, created_at)` (if the legacy table stays);
  - `objects(user_id, kind, status, created_at)`;
  - `object_payloads(object_id)` (covered by PK already) and
    `(object_id, status)`;
  - `event_log(user_id, seq)`;
  - `sessions(token_hash)` (covered by UNIQUE) and `(expires_at)` for cleanup.

## P2: Timestamps Are Stored And Compared As Text

Status: open. Verified.

The schema stores timestamps as text and code compares them lexicographically
for sessions (`sess.expires_at < now`), access keys (`expires_at <= now`),
cleanup (`ExpiresAt.lt(&now)`, `CreatedAt.lt(&cutoff)`), and pagination
(`CreatedAt.lt(before)`). All server-generated timestamps come from
`chrono::Utc::now().to_rfc3339()`, which is a fixed `+00:00` form, so the
intra-server compare is stable today. The risk is cross-source mixing: the
pagination `before` parameter and any future migration that backfills with a
different format (`Z`, fractional seconds, non-UTC offsets) silently changes
ordering.

Recommended fix:

- Store integer Unix timestamps in milliseconds, or
- Normalize one fixed-width UTC string format everywhere and validate inputs
  to `before` accordingly, or
- Parse into `DateTime<Utc>` before comparing.

## P2: Encryption KDF Parameters Are Not Persisted Per User

Status: partially addressed. Verified.

Server responses now return `state.config().crypto.encryption_params` instead
of `Argon2Params::default()`, so a single deployment can change the Argon2
defaults without immediately breaking existing users — as long as the operator
keeps the running config aligned with what users originally registered with.
The parameters are still not stored per-user, so a config change after some
users have registered will silently lock them out.

Recommended fix:

- Store `Argon2Params` per user (and ideally a KDF name/version tag).
- Return stored params on login challenge and bootstrap.

## P2: Client Pagination URLs Are Not Fully URL-Encoded

Status: partially addressed. Verified.

`ApiClient::list_objects` now builds the query string with
`Url::query_pairs_mut`. `ApiClient::list_clipboard` still concatenates the
`before` parameter into the query string by hand. As long as the legacy
clipboard route stays mounted, this remains a real bug for any caller (RFC3339
timestamps can contain `+`, which percent-decodes to a space).

Recommended fix:

- Either delete `list_clipboard` along with the rest of the legacy clipboard
  surface, or move it onto `Url::query_pairs_mut` like `list_objects`.

## P2: File Download Only Finds Metadata In The First Page

Status: open. Verified.

`SyncEngine::download_file_bytes` calls `list_objects(Some(File), Some(500),
None)` and searches that page for the requested ID before downloading. Older
files become undownloadable from the client despite the server retaining the
object. The server already exposes `GET /api/objects/{id}/payloads/{payload_id}`
but does not expose object metadata by ID.

Recommended fix:

- Add an object-by-ID endpoint (returning kind, metadata nonce + ciphertext,
  and payload descriptors), or
- Use durable local object metadata and look up the nonce there.

## P2: Client File Upload/Download Is All In Memory

Status: open. Verified.

The server streams object payload uploads to disk, but the client reads full
plaintext files into memory, encrypts to a full ciphertext buffer, uploads that
buffer, downloads payload bytes into a full buffer, decrypts into another full
buffer, and then writes the plaintext file. The new inline-vs-streamed split
(`INLINE_OBJECT_PAYLOAD_MAX_BYTES = 64 KiB`) only affects which API call
carries the bytes, not the in-memory cost.

Recommended fix:

- Add client-side size checks before reading.
- Consider reducing the max file size until streaming encryption exists.
- Longer term, use a chunked encrypted file format with a manifest hash.

## P2: Repeated Login/Register Can Spawn Duplicate Background Work

Status: open. Verified.

`SyncEngine::finish_auth` unconditionally spawns a new `ws_loop` task (on
non-wasm targets) and, on macOS, a new clipboard watcher. The engine does not
track handles and does not cancel the previous instances on re-login, logout,
or server switching. Each re-auth doubles the number of background WebSocket
loops, which is both a leak and a way to trigger duplicate refresh storms.

Recommended fix:

- Track background task handles in `SyncEngine`.
- Cancel or replace existing sync and clipboard watcher tasks during re-auth,
  logout, and server switching.

## P2/P3: Auth Rate Limiting And Proxy IPs

Status: addressed for trusted-proxy-aware IP extraction. Verified.

The server uses a `governor`-backed in-memory limiter for auth routes, keyed by
resolved client IP, plus a configurable global auth cap. By default it uses the
direct TCP peer IP. When running behind a reverse proxy, operators can configure
trusted proxy IPs or CIDR ranges in TOML, with `--trusted-proxy`, or with
`CLIPPER_TRUSTED_PROXIES`; only then will `X-Forwarded-For`, `X-Real-IP`, or
`Forwarded` be honored, with the rightmost untrusted hop selected.

Remaining hardening:

- Consider per-user and per-access-key rate limits in addition to peer IP.

## P3: Secret-Bearing Types Derive Or Use Ordinary `String`

Status: open. Verified.

`LoginParams` and `RegisterParams` in `crates/daemon-types/src/protocol.rs`
still carry `passphrase: String` and `access_key: String` and derive `Debug`.
They cross the bridge (Dart) → app/rust → daemon IPC boundary and through
`SyncEngine::login_with_platform_and_user`. Bearer tokens are stored as
`Option<String>` on `ApiClient`. The code is not currently logging those
structs directly, but it remains an accidental leak risk and they are not
zeroized when dropped.

Recommended fix:

- Remove or redact `Debug` for secret-bearing request structs.
- Use `Zeroizing<String>` where practical.
- Avoid cloning secrets across bridge/daemon boundaries more than necessary.

## P3: Server URL Validation Is Not Encapsulated

Status: open. Verified.

`validate_server_url` rejects embedded credentials and plain HTTP except for
loopback/Android emulator hosts (now also `10.0.3.2`), which is good. The
`ApiClient::set_base_url` setter still accepts any string, and validation
only runs on login/register paths.

Recommended fix:

- Validate in `set_base_url`, or use a `ServerUrl` newtype so invalid URLs are
  not representable in the client.

## P3: Legacy Single-User OPAQUE Identifier Is Implicit (New Finding)

Status: open. Introduced during the multi-user migration.

`opaque_credential_identifier_for_user` switches to the older
`clipper:passphrase:v1` credential identifier when
`user.access_key_hash == "_legacy_single_user"`, but that sentinel string is
not defined anywhere as a constant, not documented in the migration, and has
no foreign key into `access_keys`. It is purely a magic value in business
logic. If a migration ever rewrites that column, every legacy user silently
loses login compatibility.

Recommended fix:

- Promote the sentinel into a typed constant in `crates/server/src/auth.rs`
  (or use a `users.opaque_identifier_version` column) so the compatibility
  contract is searchable and reviewable.
- Or write a small migration that converts legacy users to the per-user
  identifier and removes the branch.

## P3: Auth Challenge Eviction Is Order-Dependent (New Finding)

Status: open.

`AppState::create_auth_challenge` and `create_pending_registration` drop
entries via `challenges.keys().next().cloned()` once the map hits
`auth.max_pending_challenges`. Since `HashMap` iteration order is randomized,
the evicted entry under pressure may be a fresh challenge whose owner has not
yet replied. Under sustained auth pressure this looks like sporadic
"Invalid challenge" / "Invalid registration" responses to legitimate users.

Recommended fix:

- Track insertion order (e.g. `BTreeMap` keyed by expiry, or a small ring) so
  eviction always drops the oldest entry first.

## Intentional Or Product-Dependent Tradeoffs

- Local clipboard cache is plaintext by design in the roadmap. That is a valid
  local-device tradeoff, but the UI/docs should make it explicit. Server
  clipboard retention is configurable; local cache retention should remain a
  client-side decision.
- Plain HTTP is still allowed for loopback (`127.0.0.1`, `::1`, `localhost`)
  and Android emulator hosts (`10.0.2.2`, `10.0.3.2`). Production and
  physical-device deployments need HTTPS.
- Server-visible plaintext/MCP support is planned separately in
  `docs/server-visible-mcp.md`; private-mode sync must continue to keep
  clipboard text, file metadata, and file blobs encrypted before leaving the
  client.
- Clipboard objects cannot be deleted through `DELETE /api/objects/{id}` —
  only `kind = "file"` is accepted. Clipboard retention is handled exclusively
  by the server-side TTL cleanup today.
- `SyncEngine` only supports a single payload per object
  (`fn single_payload(item)` errors otherwise), even though the server schema
  allows many. Multi-payload reads are reserved for a future client.

## Suggested PR Order

1. Object init size limits: validate `ciphertext_size`, `inline_ciphertext`
   length, and total request body against `limits.max_file_blob_bytes` and a
   small inline cap; add Axum body limits per route.
2. Crypto AAD binding: include at least object ID, payload ID, kind, and source
   device in AEAD AAD, or move to signed object envelopes.
3. Decide the fate of the legacy clipboard route: delete the dual write path
   or deprecate it explicitly and rate-limit it.
4. WS replay correctness: track oldest retained seq per user, send `Invalidate`
   on gap, advance client `last_seq` only after refresh succeeds.
5. Query performance: SQL limits and indexes for the remaining list/bootstrap
   reads; fix `list_clipboard` query encoding (or delete it).
6. Compatibility persistence: per-user KDF params, named legacy-OPAQUE
   constant, integer timestamps.
7. Client robustness: object-by-ID metadata endpoint, file-size checks before
   reading, background task cancellation on re-auth/logout.
8. Daemon IPC follow-ups: redact `Debug` on secret-bearing IPC structs,
   evaluate scoped authorization for file-path commands.
