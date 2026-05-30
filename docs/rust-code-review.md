# Rust Code Review Findings

Date: 2026-05-25

Scope reviewed:

- `crates/core`
- `crates/server`
- `crates/client`
- `crates/daemon`
- `crates/daemon-types`
- `app/rust`

This document merges the in-depth review findings with a second static review.
Some items are intentional product tradeoffs or future-facing risks, but they
are still tracked here so the security and compatibility decisions stay
explicit.

## Verification

Commands run during the in-depth review:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Both passed at the time of review. The second review was static-only and did not
run tests or clippy.

## Overall Read

The crate boundaries are sound:

- `clipper-api-types` owns HTTP and WebSocket payloads.
- `clipper-daemon-types` owns daemon IPC.
- `clipper-app-types` owns decrypted app-visible state.
- `app/rust` bridge structs are thin adapter types.

The server auth model is directionally good. Passphrases go through OPAQUE,
session tokens are random and stored as hashes, and private routes generally
derive `user_id` and `device_id` from authenticated middleware instead of
trusting request body provenance. The file upload state machine is also a good
base: pending -> uploading -> uploaded -> complete, with hash and size
validation before visibility.

The highest-priority risks are local daemon IPC authority, malformed encrypted
records, missing request limits, event replay gaps, and persistence/versioning
choices that can break old data.

## P0: Local Daemon IPC Is Too Powerful For A Bare Unix Socket

Status: partially addressed. The current patch removes the `/tmp` socket
fallback, requires a platform data directory, creates the Clipper directory with
`0700`, sets the socket file to `0600`, and caps daemon request lines. Peer UID
checks and IPC tokens remain open.

The daemon socket is the local control plane. A client that can connect can get
initial decrypted state and issue commands for login/register, sending/copying
clipboard text, uploading local files, downloading synced files to an arbitrary
path, deleting files, and refreshing state.

Relevant code:

- `crates/daemon/src/main.rs`
- `crates/daemon/src/handler.rs`
- `app/rust/src/transport.rs`

Risk:

- Same-user local processes are effectively trusted.
- The previous `/tmp` fallback for the socket/data directory is too risky for
  sensitive IPC.
- The request reader accepted unbounded newline-delimited JSON lines.

Immediate remediation already applied:

- Never place the daemon socket under `/tmp`.
- Require a platform data directory for the daemon socket and client data.
- Create the socket directory with owner-only permissions, `0700`.
- Cap daemon IPC request lines.

Remaining hardening to consider:

- Verify peer UID where available (`getpeereid` on macOS/BSD or equivalent).
- Add a per-launch or Keychain-backed IPC secret if same-user local processes
  are not trusted.
- Consider separate authorization for commands that can read/write arbitrary
  local files.

## P1: Malformed Nonce Input Can Crash Clients

Status: confirmed. The current worktree already has staged/uncommitted changes
that appear to address nonce/hash length validation and no-panic decrypt paths.

`crypto::decrypt` previously called `XNonce::from_slice(nonce)` directly. For
XChaCha20-Poly1305, the nonce must be exactly 24 bytes. A wrong-length nonce
can panic in the generic-array layer instead of returning `CryptoError`.

The server accepted decoded clipboard and file nonces without length checks, so
an authenticated client could store a malformed encrypted record that crashes
other clients during bootstrap or refresh.

Required invariant:

- XChaCha20-Poly1305 nonce length is 24 bytes.
- SHA-256 fields are 32 bytes.
- Malformed server records must become decrypt errors, not process panics.

Tests worth keeping:

- `decrypt` rejects malformed nonce lengths.
- Clipboard upload rejects malformed nonce/hash fields before disk writes.
- File init rejects malformed metadata/blob nonces.
- File completion rejects malformed hash fields without deleting a valid blob.

## P1: Encrypted Payload Authentication Does Not Bind Object Metadata

Status: open.

The current AEAD associated data is type-level only, such as
`clipper:clipboard:v1`, `clipper:file-meta:v1`, and `clipper:file-blob:v1`.
That proves the ciphertext belongs to a broad object type, but it does not bind
object ID, source device, timestamps, operation/version, or the metadata/blob
relationship.

Risk:

- A malicious or buggy server/relay can replay or mix ciphertext, nonces, IDs,
  timestamps, device attribution, and file metadata/blob pairs without AEAD
  detecting the metadata tamper.

Recommended fix:

- Bind stable object metadata into AAD, or introduce signed object envelopes as
  described in `docs/local-store-p2p-roadmap.md`.
- For future P2P, signed envelopes should bind object ID, object kind, source
  device, created time, operation type, nonce set, ciphertext hash, and version.

## P1: WebSocket Replay Can Silently Miss Pruned Events

Status: open.

The cleanup job prunes old event rows. WebSocket replay returns all events after
`last_seq` that still exist, but it does not prove the replay is complete. The
server only sends `Invalidate` when the query errors, not when the requested
sequence is older than the retained event window.

The client also advances `last_seq` before refresh succeeds. If refresh fails,
the next reconnect may skip the failed event.

Recommended fix:

- Track oldest available event sequence per user.
- If `last_seq` is older than the retained range, send `Invalidate`.
- Cap replay count; if the gap is too large, send `Invalidate`.
- On `Invalidate`, run a full bootstrap rather than a shallow refresh.
- Advance client `last_seq` only after the refresh/bootstrap caused by the event
  succeeds, or persist an explicit "needs bootstrap" state.

## P1/P2: Request And Payload Size Limits Are Incomplete

Status: partially in progress.

File blob upload is capped at 512 MiB and streamed on the server, which is good.
Other paths are less bounded:

- Clipboard upload decodes JSON/base64 ciphertext into memory.
- File init decodes encrypted metadata into memory and stores it in SQLite.
- Device name, platform, access key, and auth payload fields do not have clear
  application-level length caps.
- Daemon IPC request lines were unbounded.

Recommended fix:

- Add Axum request body limits for JSON routes.
- Add application-level caps for clipboard ciphertext, file metadata
  ciphertext, device name, platform, access key, and OPAQUE payload sizes.
- Keep the streamed file blob path.
- Align reverse proxy request-size limits with server limits.

## P2: List And Bootstrap Queries Fetch All Rows Before Truncating

Status: open.

Clipboard list, file list, and sync bootstrap query all matching rows, then
truncate in Rust. That weakens the value of the `limit` parameter and becomes a
scaling/DoS problem as history grows.

Recommended fix:

- Apply SQL `limit + 1` before `.all(...)`.
- Add DB indexes matching access patterns:
  - `clipboard_items(user_id, created_at)`
  - `files(user_id, status, created_at)`
  - `event_log(user_id, seq)`
- Verify `sessions(token_hash)` has an effective unique index.

## P2: Timestamps Are Stored And Compared As Text

Status: open.

The schema stores timestamps as text and code compares them lexicographically
for sessions, access keys, cleanup, and pagination. This is safe only if every
stored timestamp is normalized to one fixed-width representation. The docs and
code can produce mixed `Z`, `+00:00`, and fractional-second forms.

Recommended fix:

- Store integer Unix timestamps in seconds or milliseconds, or
- Normalize one fixed-width UTC string format everywhere, or
- Parse into `DateTime<Utc>` before comparing.

For SQLite, integer milliseconds is usually simplest.

## P2: Encryption KDF Parameters Are Not Persisted Per User

Status: open.

The API exposes `Argon2Params`, and clients derive encryption keys from
passphrase + salt + params. The `users` table stores the salt, but server
responses currently return `Argon2Params::default()` rather than stored per-user
parameters.

Risk:

- Changing defaults later can make old users derive different keys and lose
  access to existing encrypted data.

Recommended fix:

- Store KDF name/version and params per user.
- Return stored params on login and bootstrap.

## P2: Client Pagination URLs Are Not URL-Encoded

Status: open.

`ApiClient::list_clipboard` and `ApiClient::list_files` concatenate `before`
directly into the query string. RFC3339 timestamps can contain `+`, which query
parsing can treat as a space.

Recommended fix:

- Use `reqwest` query building instead of manual string concatenation.

## P2: File Download Only Finds Metadata In The First Page

Status: open.

`SyncEngine::download_file` lists up to 500 files and searches that page for the
requested ID before downloading the blob. Older files can become undownloadable
from the client despite the server having `GET /api/files/{id}/blob`.

Recommended fix:

- Add a file metadata-by-ID endpoint, or
- Use durable local file metadata and look up the nonce there.

## P2: Client File Upload/Download Is All In Memory

Status: open.

The server streams blob uploads to disk, but the client reads full plaintext
files into memory, encrypts to a full ciphertext buffer, uploads that buffer,
downloads blob bytes into a full buffer, decrypts into another full buffer, and
then writes the plaintext file.

Recommended fix:

- Add client-side size checks before reading.
- Consider reducing the max file size until streaming encryption exists.
- Longer term, use a chunked encrypted file format with a manifest hash.

## P2: Repeated Login/Register Can Spawn Duplicate Background Work

Status: open.

Successful auth spawns a WebSocket loop and, on macOS, a clipboard watcher. The
engine does not cancel existing loops/watchers before starting new ones.

Recommended fix:

- Track background task handles.
- Cancel or replace existing sync and clipboard watcher tasks during re-auth,
  logout, and server switching.

## P2/P3: Auth Rate Limiting And Proxy IPs

Status: fixed for trusted-proxy-aware client IP extraction; follow-up remains
open for optional per-user and per-access-key limits.

The server uses a `governor`-backed in-memory limiter for auth routes, keyed by
resolved client IP, plus a configurable global auth cap. By default it uses the
direct TCP peer IP. When running behind a reverse proxy, operators can configure
trusted proxy IPs or CIDR ranges in TOML, with `--trusted-proxy`, or with
`CLIPPER_TRUSTED_PROXIES`; only then will `X-Forwarded-For`, `X-Real-IP`, or
`Forwarded` be used.

Remaining hardening:

- Consider per-user and per-access-key rate limits in addition to peer IP.

## P3: Secret-Bearing Types Derive Or Use Ordinary `String`

Status: open.

Daemon IPC auth params contain passphrases and access keys as ordinary
`String`s, and some secret-bearing structs derive `Debug`. The code is not
currently logging those structs directly, but it is an accidental leak risk.
Bearer tokens are also stored in `String` on the client.

Recommended fix:

- Remove or redact `Debug` for secret-bearing request structs.
- Use `Zeroizing<String>` where practical.
- Avoid cloning secrets across bridge/daemon boundaries more than necessary.

## P3: Server URL Validation Is Not Encapsulated

Status: open.

`validate_server_url` rejects embedded credentials and plain HTTP except for
loopback/Android emulator hosts, which is good. The setter still accepts any
string and validation only happens on login/register paths.

Recommended fix:

- Validate in `set_base_url`, or use a `ServerUrl` newtype so invalid URLs are
  not representable in the client.

## Intentional Or Product-Dependent Tradeoffs

- Local clipboard cache is plaintext by design in the roadmap. That is a valid
  local-device tradeoff, but the UI/docs should make it explicit. Server
  clipboard retention is configurable; local cache retention should remain a
  client-side decision.
- Plain HTTP is still allowed for loopback and Android emulator development.
  Production and physical-device deployments need HTTPS.
- Server-visible plaintext/MCP support is planned separately in
  `docs/server-visible-mcp.md`; private-mode sync must continue to keep
  clipboard text, file metadata, and file blobs encrypted before leaving the
  client.

## Suggested PR Order

1. Daemon IPC hardening: no `/tmp` socket fallback, owner-only socket directory,
   bounded request lines, then peer UID or IPC secret.
2. Crypto hardening: no-panic decrypt, nonce/hash length validation, malformed
   record tests.
3. Server limits: JSON body limits and field-specific caps.
4. Query performance: SQL limits and indexes for list/bootstrap/replay.
5. Compatibility persistence: per-user KDF params and integer timestamps.
6. Client robustness: encoded query params, file-size checks, metadata-by-ID
   download path, and background task cancellation.
