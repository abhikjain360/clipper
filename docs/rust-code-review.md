# Rust Code Review Findings

Open security and correctness issues in the Rust backend. This document tracks
only what is wrong *now*; fixed items are removed rather than archived.

Threat model: every client (frontend, daemon, desktop, Android, web, and any
future client) is untrusted; the server is honest-but-curious storage and
coordination; clients encrypt content locally before upload. A relevant
attacker may hold a database dump plus on-disk blobs, be a malicious
authenticated client/device, be a malicious or buggy server/relay tampering
with ciphertext, or (for daemon IPC) be same-user local malware.

Scope: `crates/core`, `crates/server`, `crates/client` (including
`src/local_store.rs`), `crates/daemon`, `crates/daemon-types`, `app/rust`.
Release-readiness notes at the bottom also mention Flutter, Android, and CI
metadata when they affect whether the project is safe to publish.

Highest-priority open risks: AEAD AAD does not bind object identity; WebSocket
replay can silently miss pruned events and is uncapped; the client advances its
sync cursor before the refresh that consumes it succeeds; crypto config
validation floors permit a trivially insecure deployment.

## P1: Encrypted Payload Authentication Does Not Bind Object Metadata

AAD constants are type-level only (`AAD_CLIPBOARD_META_V1`,
`AAD_CLIPBOARD_PAYLOAD_V1`, `AAD_FILE_META_V1`, `AAD_FILE_BLOB_V1` in
`crates/core/src/crypto.rs`); the client passes them verbatim
(`crates/client/src/api_client.rs:466-609`). AAD never references object ID,
payload ID, object kind, source device, timestamps, or the metadata/payload
pairing.

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

Server side (`crates/server/src/ws.rs:86-104`): `handle_socket` sends
`Invalidate { target: "all" }` only when the `get_events_since` query *errors*.
It does not track the oldest retained `event_log.seq` per user, so once
`cleanup_old_events` prunes rows (default `event_log_retention_days = 3`), a
client reconnecting with a `last_seq` older than the surviving range gets a
successful query that returns only the suffix and silently advances past the
pruned creates/deletes. There is also no replay cap: a client can send a very
old or negative `last_seq` and force the server to load and stream every
retained event — a memory/CPU amplification vector.

Recommended fix:

- Track the oldest available `event_log.seq` per user; send `Invalidate` when
  `last_seq` precedes it.
- Cap replay count; `Invalidate` instead of streaming an unbounded backlog.

## P1: Client Advances `last_seq` Before Refresh Succeeds

`SyncEngine::ws_connect` (`crates/client/src/engine.rs:964`) writes
`*self.last_seq.write().await = seq` *before* calling `self.refresh()` at
`:967`. If refresh/download/decrypt then fails (transient error or hostile
server response), the next reconnect sends the advanced `last_seq` and skips the
failed event permanently.

Recommended fix: advance client `last_seq` only after the refresh/bootstrap
that consumes it succeeds, or persist an explicit "needs bootstrap" state.

## P2: Crypto Config Floors Permit A Trivially Insecure Deployment

`CryptoConfig` validation only requires `range(min = 1)` for
`session_token_bytes` and `access_key_hash_salt_bytes`
(`crates/server/src/config.rs:289-292`), and `Argon2Params` only requires
`range(min = 1)` for `m_cost`, `t_cost`, `p_cost`
(`crates/api-types/src/lib.rs:78-83`, reached via `#[garde(dive)]`). A TOML/CLI
deployment can therefore start with 1-byte session tokens (≈8 bits, remotely
guessable → account takeover), 1-byte access-key salts, and Argon2 reduced to
1 KiB / 1 pass. Validation accepts all of it.

Recommended fix: enforce hard security floors (e.g. session tokens ≥ 32 bytes,
salts ≥ 16 bytes, documented Argon2 minimums) unless an explicit, clearly named
dev/unsafe mode is selected.

## P2: Local Plaintext Clipboard Cache Is World-Readable

The client clipboard cache is plaintext on disk by design, but
`write_file_atomic` (`crates/client/src/local_store.rs:391-401`) uses
`tokio::fs::write` with default permissions (`0644` under the usual `022`
umask), and `clipboard_dir` is not restricted to `0700`. Any local user can
then read cached clipboard contents (passwords, tokens). This is the same
exposure the daemon IPC secret is carefully protected against, but the cache is
not.

Recommended fix: write payload/meta files `0600` inside a `0700` profile
directory on non-wasm targets (mirror `crates/daemon/src/keychain.rs`).

## P2: Orphan-Upload Cleanup Has A Delete/Complete Race

`cleanup_orphan_object_uploads` (`crates/server/src/cleanup.rs:172-211`)
captures orphan object IDs, removes their payload *files*, then re-runs the
deletion **by filter** (`Status.ne("complete")`) rather than by the captured ID
set. If an upload completes in that window, the payload file is deleted but the
now-`complete` object row survives the filtered delete, leaving a "complete"
object whose blob is gone (download 404 / data inconsistency). The window is
bounded by `orphan_upload_ttl_secs` (default 1h) but the race is real.

Recommended fix: delete by the captured ID set in a single transaction, or
re-check/lock status before removing files.

## P2: No Cap On Payloads Per Object

`ObjectInitRequest.payloads` is validated only as `length(min = 1)`
(`crates/api-types/src/lib.rs:248`); `init_object` loops inserts with no upper
bound on payload count. Within the implicit request-body limit a client can
declare tens of thousands of payload rows and upload URLs in one request — row
and response amplification.

Recommended fix: add an explicit maximum payload count (the client only uses
one payload per object today).

## P2: File Download Only Finds Metadata In The First Page

`SyncEngine::download_file_bytes` (`crates/client/src/engine.rs:626`) calls
`list_objects(Some(File), Some(500), None)` and searches that single page for
the requested ID before downloading. Files older than the most recent 500
become undownloadable from the client even though the server retains them. The
server exposes `GET /api/objects/{id}/payloads/{payload_id}` but no object
metadata lookup by ID, and `local_store` caches clipboard items but not file
metadata, so the download path cannot recover the nonce locally either.

Recommended fix: add an object-by-ID metadata endpoint (kind, metadata
nonce/ciphertext, payload descriptors), or extend `local_store` to cache file
metadata and look the nonce up there.

## P2: Repeated Login/Register Can Spawn Duplicate Background Work

`SyncEngine::finish_auth` (`crates/client/src/engine.rs:210-224`)
unconditionally spawns a new `ws_loop` task (non-wasm) and, on macOS/Linux, a
new clipboard watcher, without tracking handles or cancelling prior instances.
Re-authenticating while already logged in stacks a second `ws_loop` (a leak and
a duplicate-refresh-storm vector), and `logout` does not cancel them.

Recommended fix: track background task handles in `SyncEngine`; cancel/replace
existing sync and watcher tasks on re-auth, logout, and server switch.

## P2: Logout Can Fail To Clear Local Auth State

`ApiClient::logout` (`crates/client/src/api_client.rs:233-245`) sends the server
logout request and returns `Err` before clearing `self.token` when the request
itself fails. `SyncEngine::logout` (`crates/client/src/engine.rs:231-244`)
awaits that call before clearing the encryption key and app state, and the
Flutter logout button currently swallows the error
(`app/lib/screens/home_screen.dart:47-51`).

If the server is down, the network is unavailable, or the session revoke request
times out, the user can appear to have pressed logout while the client remains
locally logged in with token/key material and background work still alive. This
is a correctness and privacy issue, especially on shared devices.

Recommended fix: make logout local-first. Clear the local token, encryption key,
app state, profile-sensitive local state, and background task handles regardless
of server reachability; then best-effort revoke the server session and surface a
non-blocking warning if revocation failed.

## P2: Client File Upload/Download Is All In Memory

The server streams payload uploads to disk, but the client reads full plaintext
files into memory, encrypts to a full ciphertext buffer, uploads that buffer,
downloads payload bytes into a full buffer, decrypts into another buffer, and
writes the plaintext file. The `INLINE_OBJECT_PAYLOAD_MAX_BYTES = 64 KiB` split
(`crates/client/src/engine.rs:22`) only changes which API call carries the
bytes. There is no client-side size check before reading.

Recommended fix: add client-side size checks before reading; consider capping
max file size until streaming encryption exists; longer term use a chunked
encrypted file format with a manifest hash.

## P2: Timestamps Are Stored And Compared As Text

The schema stores timestamps as text and code compares them lexicographically
for sessions, access keys, cleanup, and pagination (`objects.rs:655`,
`CreatedAt.lt(before)`). All server-generated timestamps come from
`chrono::Utc::now().to_rfc3339()` (fixed `+00:00` form), so intra-server
compares are stable. The risk is cross-source mixing: the client-supplied
`before` pagination parameter is not validated as a normalized timestamp, and
any future backfill in a different format (`Z`, fractional seconds, non-UTC
offsets) silently changes ordering.

Recommended fix: store integer Unix milliseconds, or normalize one fixed-width
UTC string everywhere and validate `before`, or parse to `DateTime<Utc>` before
comparing.

## P3: Username Enumeration Via Register-Start And Challenge Timing

The `challenge` endpoint now uses opaque-ke's fake-record path for unknown
users, so its *response content* no longer reveals account existence. Two gaps
remain:

- `register_start` returns `409 "Username already taken"`
  (`crates/server/src/routes/auth.rs:186`), directly enumerating usernames.
- `challenge` runs an extra AEAD `unwrap_opaque_password_file` only on the
  known-user path; the fake path skips it, leaving a timing signal.

This is partly inherent to a username-based design. Treat it as an accepted
tradeoff or add a uniform-latency / uniform-response path.

## P3: Secret-Bearing IPC Types Derive `Debug`

`LoginParams` and `RegisterParams` in
`crates/daemon-types/src/protocol.rs:75-90` carry `passphrase: String` /
`access_key: String` and derive `Debug`. They cross the Dart → app/rust →
daemon IPC boundary. Nothing logs them today, but `handler.rs` logs serde parse
errors of `DaemonRequest`, so one stray `{:?}` would leak them; they are also
not zeroized on drop.

Recommended fix: remove/redact `Debug` on secret-bearing IPC structs; use
`Zeroizing<String>` where practical; avoid cloning secrets across boundaries.

## P3: Server URL Validation Is Not Encapsulated

`validate_server_url` (`crates/client/src/api_client.rs:400`) rejects embedded
credentials and plain HTTP except for loopback / Android-emulator hosts, but it
only runs on the login/register paths. `ApiClient::set_base_url` (`:51`) and
`SyncEngine::set_base_url` accept any string, and object/list/download build
URLs straight from `base_url`.

Recommended fix: validate in `set_base_url`, or use a `ServerUrl` newtype so
invalid URLs are not representable.

## P3: Auth Challenge / Pending-Registration Eviction Is Order-Dependent

`AppState::create_auth_challenge` and `create_pending_registration`
(`crates/server/src/state.rs:191`, `:235`) drop expired entries first, then —
while still at the `auth.max_pending_challenges` cap — evict via
`challenges.keys().next().cloned()`. `HashMap` iteration order is randomized, so
under sustained genuine pressure the evicted entry may be a fresh challenge
whose owner has not yet replied, surfacing as sporadic "Invalid challenge" /
"Invalid registration" responses.

Recommended fix: track insertion/expiry order (e.g. `BTreeMap` keyed by expiry,
or a small ring) so eviction drops the oldest entry first.

## P3: CORS Allows Any Origin

`main.rs:266` configures `CorsLayer::new().allow_origin(Any)`. Risk is low under
the bearer-token (non-cookie) model — a foreign origin cannot read the victim's
token from app storage — but it is broader than necessary.

Recommended fix: restrict to the known web-client origin(s).

## Lower-Priority Hygiene

- **Implicit SQLite durability.** `connect_db` (`crates/server/src/state.rs:101`)
  connects with `Database::connect("sqlite:…?mode=rwc")` and no `ConnectOptions`.
  Foreign-key enforcement (cleanup/delete cascade correctness depends on it),
  WAL, and `busy_timeout` rely on sqlx defaults; a SeaORM/sqlx bump could change
  them silently. Set them explicitly, plus a deliberate pool size.
- **`serde_json::to_string(...).unwrap()` in the WS send path** (`ws.rs:78`,
  `:97`, `:110`, `:143`). Won't panic for these structs, but `.unwrap()` in a
  network handler reads badly; use `if let Ok(...)` as the daemon does.
- **`profile_id_from_encryption_key`** (`engine.rs:1135`) is
  `hex(sha256(data_key))` — the data key doing double duty through a bare hash.
  Derive it via HKDF with its own label for consistency with the rest of the
  crypto.
- **Inline payload size is bounded only by the implicit ~2 MiB body limit.**
  There is no explicit per-route `DefaultBodyLimit`, and the `init_object`
  inline check compares against `max_file_blob_bytes` (512 MiB), which is
  unreachable through the body limit. State the policy explicitly.
- **`add-access-key --access-key <KEY>`** (`main.rs`) accepts the key as a CLI
  flag (leaks to shell history / process list); the `rpassword` prompt is the
  safe path. Consider stdin-only.

## Release Readiness Gaps

These are not all Rust backend defects, but they matter before making a public
release or advertising the app as secure:

- **Flutter app version says stable 1.0.0.** `app/pubspec.yaml` uses
  `version: 1.0.0+1` while the README and SECURITY policy correctly call the
  project early, experimental, and pre-1.0. Use a pre-release/current version
  such as `0.1.0+N` until there is a real stable release.
- **Flutter lockfile is ignored but CI keys on it.** `.gitignore` excludes
  `app/pubspec.lock`, while `.github/workflows/ci.yml` uses
  `hashFiles('app/pubspec.lock')` for the pub cache key. For an application,
  commit the lockfile for reproducibility, or stop treating it as a CI cache
  key/source of truth.
- **Android release manifest allows cleartext traffic globally.**
  `app/android/app/src/main/AndroidManifest.xml` sets
  `android:usesCleartextTraffic="true"`, even though the documented production
  model requires HTTPS outside loopback/emulator development. Move cleartext
  allowance to debug/dev configuration or a narrow network security config.

## Accepted / Intentional Tradeoffs (Residual Risks By Design)

- **Same-user local IPC trust (P0, out of scope).** The daemon hardens the
  socket against *cross-user* access (private `0700` runtime dir, `0600`
  socket, HMAC-SHA256 challenge/response, 32 MiB line cap). The residual gap is
  *same-user*: the IPC secret is keychain-backed on macOS but a `0600` file on
  Linux (`crates/daemon/src/keychain.rs`), so any same-user process can complete
  the handshake and issue commands (including `UploadFile`/`DownloadFile` with
  arbitrary paths). If same-user malware enters scope, move to OS-mediated app
  identity / user consent and add separate authorization for the file-path
  commands.
- The client `local_store` keeps clipboard plaintext on disk by design; only
  the network boundary is encrypted. (See P2 above for the *permissions* bug,
  which is not intentional.)
- Plain HTTP is allowed only for loopback (`127.0.0.1`, `::1`, `localhost`) and
  Android emulator hosts (`10.0.2.2`, `10.0.3.2`). Production and
  physical-device deployments need HTTPS.
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

1. Crypto AAD binding: bind object ID, payload ID, kind, and source device into
   AEAD AAD, or move to signed object envelopes (P1).
2. WS replay correctness: track oldest retained seq per user, `Invalidate` on
   gap or excessive range, cap replay (P1); advance client `last_seq` only after
   refresh succeeds (P1).
3. Crypto config floors and local-cache `0600`/`0700` permissions — cheap,
   high-value hardening (P2).
4. Orphan-cleanup race, payload-count cap, explicit SQLite pragmas (P2).
5. Background task cancellation on re-auth/logout; make logout local-first;
   object-by-ID metadata lookup for download; client file-size checks (P2).
6. Release readiness: align app versioning, commit or intentionally drop the
   Flutter lockfile, and restrict Android cleartext traffic.
7. Hardening/cleanup: validated `before` cursor and integer timestamps; redact
   `Debug` on secret-bearing IPC structs; validate in `set_base_url`; ordered
   challenge eviction; restrict CORS; uniform challenge/register responses (or
   accept username enumeration explicitly).
