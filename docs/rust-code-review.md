# Rust Code Review Findings

Known security and correctness issues in the Rust backend, plus the invariants
worth protecting. Threat model: every client (frontend, daemon, desktop,
Android, web, and any future client) is untrusted; the server is
honest-but-curious storage and coordination; clients encrypt content locally
before upload. A relevant attacker may hold a database dump plus on-disk blobs,
be a malicious authenticated client/device, be a malicious or buggy
server/relay tampering with ciphertext, or (for daemon IPC) be same-user local
malware.

Scope: `crates/core`, `crates/server`, `crates/client` (including
`src/local_store.rs`), `crates/daemon`, `crates/daemon-types`, `app/rust`.

Findings are grouped by severity. Some are intentional product tradeoffs or
future-facing risks; they stay listed so the decisions remain explicit.

## Overall Read

The crate boundaries are sound:

- `clipper-api-types` owns HTTP and WebSocket payloads.
- `clipper-daemon-types` owns daemon IPC.
- `clipper-app-types` owns decrypted app-visible state.
- `crates/client/src/local_store.rs` owns the durable client-side clipboard
  cache.
- `app/rust` bridge structs are thin adapter types.

The server auth model is directionally good. Passphrases go through OPAQUE; the
per-user `opaque_server_setup` / `opaque_password_file` and the server-wide
`access_key_hash_salt` are AEAD-wrapped at rest with a server pepper; session
tokens are random and stored as hashes; private routes derive
`user_id` and `device_id` from authenticated middleware instead of trusting
request-body provenance; registration is gated by a one-time Argon2id-verified
access key consumed atomically. The object upload state machine is solid:
per-payload `pending` -> `uploading` -> `uploaded` -> `complete`, gated by the
authenticated source device, with `create_new` inline writes, `.tmp` +
atomic-rename streaming writes, and hash/size validation before completion.
Both clipboard items and files flow through one generic `objects` route.

Invariants that are correct today and must not regress:

- `crypto::decrypt` rejects any non-24-byte nonce before use (malformed server
  records become decrypt errors, not panics); `garde` enforces 24-byte nonces
  and 32-byte SHA-256 fields at the API edge.
- At-rest pepper wrapping uses per-column subkeys and AAD, so a ciphertext
  cannot be moved between columns.
- Access keys are stored only as Argon2id verifiers (server pepper passed as
  Argon2's `secret`) and consumed atomically (`UsedAt.is_null()` filtered
  update with a `rows_affected` check).
- Session tokens are random, stored as `sha256(token)`, required on all private
  routes; logout deletes only the authenticated session.
- All object reads, writes, deletes, bootstrap responses, and WebSocket events
  are scoped by authenticated `user_id`; payload upload/complete additionally
  require `source_device_id == auth.device_id`.
- Client-supplied object/payload IDs are validated as 36-character UUIDs before
  becoming filenames; payload paths are built from `state.objects_dir()` only.

Highest-priority open risks: AEAD AAD does not bind object identity; the
streaming upload path does not enforce `limits.max_file_blob_bytes`; WebSocket
replay can silently miss pruned events and is uncapped; crypto config
validation floors permit a trivially insecure deployment.

## P0: Local Daemon IPC Trusts Any Same-User Process

The daemon hardens the socket against *cross-user* access:
`crates/daemon-types/src/ipc_path.rs` puts the socket in a short per-user
private runtime directory (`/tmp/clipper-$uid` on macOS, `XDG_RUNTIME_DIR` with
the same fallback on Linux), verifies ownership, keeps the directory at `0700`,
and `crates/daemon/src/main.rs` sets the socket to `0600`. IPC also caps each
line at `MAX_IPC_REQUEST_LINE_BYTES = 32 MiB` and runs an HMAC-SHA256
challenge/response (`handler.rs` `authenticate_connection` /
`verify_auth_request`, constant-time `verify_slice`) binding protocol version,
daemon nonce, and client nonce before accepting any command. `app/rust`
mirrors the protocol.

The residual gap is *same-user*: the shared IPC secret is keychain-backed on
macOS but, on Linux (`keychain.rs`), is a `0600` file in the user's data dir.
Any process running as the same user can read that file (or reach the
keychain), complete the handshake, and issue commands that expose decrypted
clipboard payloads or write decrypted files. The file-path commands
(`UploadFile { file_path }`, `DownloadFile { target_path }`) also have no
authorization separate from the handshake.

Recommended fix: if same-user malware is in scope, move to OS-mediated app
identity / user consent, and add separate authorization for the file-path
commands.

## P1: Encrypted Payload Authentication Does Not Bind Object Metadata

AAD constants are type-level only (`AAD_CLIPBOARD_META_V1`,
`AAD_CLIPBOARD_PAYLOAD_V1`, `AAD_FILE_META_V1`, `AAD_FILE_BLOB_V1` in
`crates/core/src/crypto.rs`); the client passes them verbatim
(`crates/client/src/api_client.rs:444`, `:470`, `:504`, `:558`). AAD never
references object ID, payload ID, object kind, source device, timestamps, or
the metadata/payload pairing.

A malicious or buggy server/relay can therefore move ciphertext between
identities without AEAD detecting it: replay an old ciphertext under a new
object ID, or pair one object's file metadata with another object's blob, and
the client decrypts both as valid. The multi-payload object split widened this
surface because metadata and payload are separately encrypted but unbound to
each other.

Recommended fix:

- Bind stable object identity into AAD (at minimum object ID, payload ID,
  object kind, source device), or
- Move to signed/encrypted object envelopes as in
  `docs/local-store-p2p-roadmap.md` (bind object ID, kind, source device,
  created time, operation type, payload set, ciphertext hashes, version).

## P1: WebSocket Replay Can Silently Miss Pruned Events And Is Uncapped

Server side (`crates/server/src/ws.rs`): `handle_socket` sends
`Invalidate { target: "all" }` only when the `get_events_since` query *errors*.
It does not track the oldest retained `event_log.seq` per user, so once
`cleanup_old_events` prunes rows (default `event_log_retention_days = 3`), a
client reconnecting with a `last_seq` older than the surviving range gets a
successful query that returns only the suffix and silently advances past the
pruned creates/deletes. There is also no replay cap: a client can send a very
old or negative `last_seq` (`get_events_since` maps negatives to `i32::MIN`)
and force the server to load and stream every retained event — a memory/CPU
amplification vector.

Client side (`crates/client/src/engine.rs:970`): `last_seq` is advanced to the
event's `seq` *before* `refresh()` runs (`:973`). If refresh/download/decrypt
then fails (transient error or hostile server response), the next reconnect
sends the advanced `last_seq` and skips the failed event.

Recommended fix:

- Track the oldest available `event_log.seq` per user; send `Invalidate` when
  `last_seq` precedes it.
- Cap replay count; `Invalidate` instead of streaming an unbounded backlog.
- Advance client `last_seq` only after the refresh/bootstrap succeeds, or
  persist an explicit "needs bootstrap" state.

## P1: Streaming Upload Does Not Enforce `max_file_blob_bytes`

`limits.max_file_blob_bytes` (default 512 MiB) is defined and `garde`-validated
in config but is never referenced by any route. The streaming upload
(`PUT /api/objects/{id}/payloads/{payload_id}`) takes the payload's declared
`ciphertext_size` as `expected_size` (`routes::objects::upload_payload`;
`validate_object_payload_size` at `objects.rs:254` only rejects negatives) and
streams up to that many bytes to disk. A malicious authenticated client can
declare a payload near `i64::MAX` and fill the server disk.

Current partial bounds (not a substitute for the cap):

- There is no explicit `DefaultBodyLimit`, so `init_object` / `complete_object`
  (which buffer the whole postcard body via `Bytes`) are bounded only by
  axum's implicit 2 MiB request-body default — a framework default, not an
  explicit policy.
- `garde` validates each inline payload: `inline_ciphertext.len()` must equal
  the declared `ciphertext_size` and match `sha256_ciphertext`
  (`api-types` `validate_inline_ciphertext`).

Recommended fix:

- Enforce `max_file_blob_bytes` against each declared `ciphertext_size` and
  inline length during `init_object`, and re-check it while streaming in
  `upload_payload`.
- Add an explicit `DefaultBodyLimit` per route, sized above the 64 KiB inline
  cap but bounded.

## P2: Crypto Config Floors Permit A Trivially Insecure Deployment

`CryptoConfig` validation only requires `range(min = 1)` for
`session_token_bytes` and `access_key_hash_salt_bytes`
(`crates/server/src/config.rs`), and `Argon2Params` only requires
`range(min = 1)` for `m_cost`, `t_cost`, `p_cost`
(`crates/api-types/src/lib.rs:92-100`, reached via `#[garde(dive)]`). A TOML/CLI
deployment can therefore start with 1-byte session tokens (≈8 bits, remotely
guessable → account takeover), 1-byte access-key salts, and Argon2 reduced to
1 KiB / 1 pass. Validation accepts all of it.

Recommended fix: enforce hard security floors (e.g. session tokens ≥ 32 bytes,
salts ≥ 16 bytes, documented Argon2 minimums) unless an explicit, clearly named
dev/unsafe mode is selected.

## P2: List/Bootstrap Query Performance And Missing Indexes

`routes::objects::list_objects` applies `.limit(limit + 1)` at SQL level and
exposes `next_before` cursoring, and `routes::sync::bootstrap` reads no
clipboard rows (it returns device, `latest_seq`, and `ServerInfo` only). The
remaining costs:

- No DB indexes exist beyond primary keys and the `UNIQUE` constraints on
  `username`, `access_key_hash`, `ciphertext_path`, and `token_hash`. The hot
  read paths (`objects(user_id, kind, status, created_at)`,
  `event_log(user_id, seq)`, `object_payloads(object_id, status)`,
  `sessions(expires_at)` for cleanup) are unindexed, so a user with many
  objects forces table scans and sorts; a malicious authenticated user can
  amplify this by creating many objects.
- `list_objects` issues an N+1 payload query (one per returned object).
- `cleanup::trim_user_clipboard` uses `offset(max_items).limit(i64::MAX)` to
  select overflow rows.

Recommended fix: add the indexes above (at minimum
`objects(user_id, kind, status, created_at DESC)` and `event_log(user_id, seq)`),
and consider a single joined payload load for `list_objects`.

## P2: Timestamps Are Stored And Compared As Text

The schema stores timestamps as text and code compares them lexicographically
for sessions (`auth.rs:59`, `sess.expires_at < now`), access keys
(`auth.rs:137`/`:249`), cleanup (`ExpiresAt.lt`, `CreatedAt.lt`), and pagination
(`objects.rs:566`, `CreatedAt.lt(before)`). All server-generated timestamps come
from `chrono::Utc::now().to_rfc3339()` (fixed `+00:00` form), so intra-server
compares are stable. The risk is cross-source mixing: the client-supplied
`before` pagination parameter is not validated as a normalized timestamp, and
any future backfill in a different format (`Z`, fractional seconds, non-UTC
offsets) silently changes ordering.

Recommended fix: store integer Unix milliseconds, or normalize one fixed-width
UTC string everywhere and validate `before`, or parse to `DateTime<Utc>` before
comparing.

## Resolved: Encryption KDF Parameters Are Not Persisted Per User

Client object keys now derive from OPAQUE's stable `export_key`, so the server
no longer returns encryption KDF parameters or salts. A future schema cleanup
can remove the legacy `users.encryption_salt` column.

## P2: File Download Only Finds Metadata In The First Page

`SyncEngine::download_file_bytes` (`engine.rs:632`) calls
`list_objects(Some(File), Some(500), None)` and searches that single page for
the requested ID before downloading. Files older than the most recent 500
become undownloadable from the client even though the server retains them. The
server exposes `GET /api/objects/{id}/payloads/{payload_id}` but no object
metadata lookup by ID, and `local_store` caches clipboard items but not file
metadata, so the download path cannot recover the nonce locally either.

Recommended fix: add an object-by-ID metadata endpoint (kind, metadata
nonce/ciphertext, payload descriptors), or extend `local_store` to cache file
metadata and look the nonce up there.

## P2: Client File Upload/Download Is All In Memory

The server streams payload uploads to disk, but the client reads full plaintext
files into memory, encrypts to a full ciphertext buffer, uploads that buffer,
downloads payload bytes into a full buffer, decrypts into another buffer, and
writes the plaintext file. The `INLINE_OBJECT_PAYLOAD_MAX_BYTES = 64 KiB` split
(`engine.rs:24`) only changes which API call carries the bytes. There is no
client-side size check before reading.

Recommended fix: add client-side size checks before reading; consider capping
max file size until streaming encryption exists; longer term use a chunked
encrypted file format with a manifest hash.

## P2: Repeated Login/Register Can Spawn Duplicate Background Work

`SyncEngine::finish_auth` (`engine.rs:222-235`) unconditionally spawns a new
`ws_loop` task (non-wasm) and, on macOS/Linux, a new clipboard watcher, without
tracking handles or cancelling prior instances. Each re-auth doubles the number
of background WebSocket loops (a leak and a duplicate-refresh-storm vector), and
`logout` does not cancel them.

Recommended fix: track background task handles in `SyncEngine`; cancel/replace
existing sync and watcher tasks on re-auth, logout, and server switch.

## P2/P3: Auth Rate Limiting Is IP-Only

`rate_limit.rs` is a `governor`-backed limiter: a keyed per-IP limiter plus a
global auth cap, applied to all four public auth routes via a router
`route_layer`. It keys on the direct TCP peer IP by default; forwarded headers
(`X-Forwarded-For`, `Forwarded`, `X-Real-IP`) are honored only when the
immediate peer matches startup trusted-proxy config (`trusted_proxies`,
`--trusted-proxy`, `CLIPPER_TRUSTED_PROXIES`), selecting the rightmost untrusted
hop.

Remaining gap: there are no per-user or per-access-key limits, and object
routes are not rate-limited at all.

## P3: Username Enumeration Via Challenge And Register-Start

Because each user has an independent per-user `opaque_server_setup`, the OPAQUE
client-enumeration mitigation does not apply: `challenge` looks the user up by
username and returns `401 "Unknown user"` immediately for a miss without any
OPAQUE work, while a hit runs the full `opaque_server_login_start`
(`routes/auth.rs:39-47`); `register_start` returns `409 "Username already taken"`
(`:155`). Both the response-content and timing differences let an attacker
enumerate usernames. The `fake_sk` field that `docs/opaque.md` describes is
never exercised on the missing-user path.

This is partly inherent to a username-based design. Treat it as an accepted
tradeoff or add a uniform-latency / uniform-response path; at minimum the docs
must not imply `fake_sk` mitigates it.

## P3: Secret-Bearing Types Derive Or Use Ordinary `String`

`LoginParams` and `RegisterParams` in `crates/daemon-types/src/protocol.rs`
carry `passphrase: String` / `access_key: String` and derive `Debug`. They
cross the Dart → app/rust → daemon IPC boundary. Bearer tokens are stored as
`Option<String>` on `ApiClient`. Nothing logs these structs directly, but they
are an accidental-leak risk and are not zeroized on drop.

Recommended fix: remove/redact `Debug` on secret-bearing IPC structs; use
`Zeroizing<String>` where practical; avoid cloning secrets across boundaries.

## P3: Server URL Validation Is Not Encapsulated

`validate_server_url` (`api_client.rs:394`) rejects embedded credentials and
plain HTTP except for loopback / Android-emulator hosts (`10.0.2.2`,
`10.0.3.2`), but it only runs on the login/register paths.
`ApiClient::set_base_url` (`:44`) accepts any string.

Recommended fix: validate in `set_base_url`, or use a `ServerUrl` newtype so
invalid URLs are not representable.

## P3: Auth Challenge / Pending-Registration Eviction Is Order-Dependent

`AppState::create_auth_challenge` and `create_pending_registration`
(`state.rs:139`, `:182`) drop expired entries first, then — while still at the
`auth.max_pending_challenges` cap — evict via `challenges.keys().next().cloned()`.
`HashMap` iteration order is randomized, so under sustained genuine pressure the
evicted entry may be a fresh challenge whose owner has not yet replied,
surfacing as sporadic "Invalid challenge" / "Invalid registration" responses.

Recommended fix: track insertion/expiry order (e.g. `BTreeMap` keyed by expiry,
or a small ring) so eviction drops the oldest entry first.

## P3: Dead And Hardcoded Configuration

- `limits.max_file_meta_ciphertext_bytes` is defined and validated but never
  referenced by any route (file metadata flows as object metadata, capped by
  `max_object_meta_ciphertext_bytes`). Wire it up or remove it.
- `AAD_CLIPBOARD_V1` is unused outside `crypto` tests.
- Session lifetime is hardcoded to 30 days in `issue_session` (`auth.rs:602`);
  it is not configurable like the other auth knobs.

## Intentional Or Product-Dependent Tradeoffs

- The client `local_store` keeps clipboard plaintext on disk by design; the
  network boundary stays encrypted. The UI/docs should keep this explicit.
- Plain HTTP is allowed only for loopback (`127.0.0.1`, `::1`, `localhost`) and
  Android emulator hosts (`10.0.2.2`, `10.0.3.2`). Production and
  physical-device deployments need HTTPS.
- Same-user local malware is out of scope for daemon IPC (see P0).
- Clipboard objects cannot be deleted through `DELETE /api/objects/{id}` — only
  `kind = "file"` is accepted. Clipboard retention is handled by TTL + the
  per-user `max_items` trim.
- `SyncEngine` supports a single payload per object (`single_payload` errors
  otherwise) even though the schema allows many; multi-payload reads are
  reserved for a future client.
- Server-visible plaintext/MCP support is planned separately in
  `docs/server-visible-mcp.md`; private-mode sync must keep clipboard text,
  file metadata, and file blobs encrypted before leaving the client.

## Suggested Fix Order

1. Blob size limits: enforce `max_file_blob_bytes` on declared/inline payload
   sizes in `init_object` and during streaming in `upload_payload`; add explicit
   per-route `DefaultBodyLimit`.
2. Crypto config floors: enforce hard minimums for session-token/salt sizes and
   Argon2 parameters.
3. Crypto AAD binding: bind object ID, payload ID, kind, and source device into
   AEAD AAD, or move to signed object envelopes.
4. WS replay correctness: track oldest retained seq per user, `Invalidate` on
   gap or excessive range, cap replay, and advance client `last_seq` only after
   refresh succeeds.
5. Query performance: add the missing indexes; collapse the `list_objects` N+1.
6. Compatibility persistence: per-user KDF params; integer timestamps and a
   validated `before` cursor.
7. Client robustness: object-by-ID (or local-store) metadata lookup for
   download; file-size checks before reading; background task cancellation on
   re-auth/logout.
8. Hardening/cleanup: redact `Debug` on secret-bearing IPC structs; validate in
   `set_base_url`; ordered challenge eviction; uniform challenge responses (or
   accept username enumeration explicitly); remove dead config/AAD; scoped
   authorization for daemon file-path commands.
