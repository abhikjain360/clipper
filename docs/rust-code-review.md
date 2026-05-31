# Rust Code Review Findings

Open security and correctness issues in the Rust backend. This document tracks
only what is wrong _now_; fixed items are removed rather than archived.

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

Highest-priority open risks: three distinct paths can silently lose a user's own
sync events (WebSocket subscribe/replay race, pruned events, and the client
advancing its cursor before refresh), and decrypted plaintext escapes the local
trust boundary (world-readable file downloads and a local cache that logout
never wipes).

## P1: WebSocket Subscribe-After-Replay Race Drops Concurrently-Committed Events

`handle_socket` reads `latest_seq` (`crates/server/src/ws.rs:72`), replays
events via `get_events_since(last_seq)` over a committed DB snapshot in an
await-per-event loop (`:86-103`), and only _after_ replay finishes calls
`state.ws_tx().subscribe()` (`:116`). Any event another device commits and
broadcasts during that window is in neither the replay (committed after the
snapshot read) nor the live stream (`broadcast::Sender` drops messages for
not-yet-subscribed receivers). The server write paths broadcast after commit
outside the write lock (`crates/server/src/routes/objects.rs:250/253`,
`:662/671`, `:1061/1071`), so the window is real under concurrent multi-device
writes.

The client cannot recover the gap on its own: it never re-derives `last_seq`
from `HelloAck.latest_seq` (`crates/client/src/engine.rs:970-972` only logs it)
and advances the cursor only from received `Event` messages (`:980`), so the
cursor sits at the last replayed seq and the dropped event is never re-requested.
Recovery happens only if a later event triggers a `refresh()` whose top-100-per-
kind window still contains the missed object, or a full `Invalidate` occurs —
otherwise it is permanent cross-device divergence.

Recommended fix: subscribe to the broadcast channel _before_ issuing the replay
query, buffer live messages during replay, then flush buffered live events while
de-duplicating by seq (drop any seq ≤ the last replayed seq). This closes the
window so no committed event can fall between the snapshot and the live stream.

## P1: WebSocket Replay Can Silently Miss Pruned Events And Is Uncapped

`handle_socket` (`crates/server/src/ws.rs:86-113`) sends
`Invalidate { target: "all" }` only when the `get_events_since` query _errors_
(`:104-112`). It does not track the oldest retained `event_log.seq` per user, so
once `cleanup_old_events` prunes rows (`crates/server/src/cleanup.rs:152-159`,
default `event_log_retention_days = 3`), a client reconnecting with a `last_seq`
older than the surviving range gets a successful query that returns only the
suffix and silently advances past the pruned creates/deletes — pruning is not a
DB error. `get_events_since` also has no `LIMIT` or lower bound, so a very old or
negative `last_seq` forces the server to materialize and stream every retained
event (`ws.rs:171-183`) — a memory/CPU amplification vector.

Recommended fix:

- Track the oldest available `event_log.seq` per user; send `Invalidate` when
  `last_seq` precedes it.
- Cap replay count; `Invalidate` instead of streaming an unbounded backlog.

## P1: Client Advances `last_seq` Before Refresh Succeeds

`SyncEngine::ws_connect` (`crates/client/src/engine.rs:980`) writes
`*self.last_seq.write().await = seq` _before_ calling `self.refresh()` at
`:983`. If refresh/download/decrypt then fails (transient error or hostile
server response) it only warns (`:984`) and continues, and the next reconnect
sends the advanced `Hello { last_seq }` (`:947-948`), so the server never
replays the failed event. `refresh()` is a full top-100-per-kind re-fetch, so a
later successful event self-corrects an existing create and any delete;
permanent loss is narrowed to a missed create that falls outside the latest-100
window with no further events before reconnect.

Recommended fix: advance client `last_seq` only after the refresh/bootstrap
that consumes it succeeds, or persist an explicit "needs bootstrap" state.

## P2: File Download Writes Decrypted Plaintext World-Readable And Follows Symlinks

`cmd_download_file` passes the IPC-supplied `target_path` straight to
`engine.download_file` (`crates/daemon/src/handler.rs:521-531`), which decrypts
the blob and writes it with `tokio::fs::write(Path::new(target_path), &plaintext)`
(`crates/client/src/engine.rs:656-664`, write at `:658`). There is no
`set_permissions` / `OpenOptions::mode(0o600)` / `O_NOFOLLOW`, so the write
(a) creates the file `0666` masked by umask → `0644` world-readable under the
usual `022` umask, (b) follows symlinks on `target_path` and parent components,
and (c) truncates/overwrites whatever exists. This is the daemon's
decrypted-content egress, using the identical bare-write mechanism as the
local-cache permission bug above.

Decrypted protected plaintext thus lands on disk world-readable, so any other
local user on a multi-user host can read synced files even though network and
at-rest crypto are sound. Symlink-following plus overwrite also means a
pre-planted symlink at a predictable target can redirect the plaintext or
clobber an unrelated file. This is broader than the same-user
arbitrary-path tradeoff documented below: that note covers path arbitrariness,
not egress file permissions or symlink-following.

Recommended fix: write downloads atomically with explicit `0600` mode
(`OpenOptions::mode(0o600)` + `create_new`/atomic rename), refuse to follow
symlinks at the final component (`O_NOFOLLOW` or pre-resolve + verify), and
apply the same `0600`-in-`0700` policy recommended for `local_store`.

## P2: Logout Is Neither Local-First Nor Destructive

`ApiClient::logout` (`crates/client/src/api_client.rs:233-245`) sends the server
logout request and returns `Err` before clearing `self.token` when the request
itself fails. `SyncEngine::logout` (`crates/client/src/engine.rs:231-244`)
awaits that call before clearing the encryption key and app state, and the
Flutter logout button swallows the error
(`app/lib/screens/home_screen.dart:47-51`). If the server is down or the revoke
times out, the user can appear to have pressed logout while the client remains
locally logged in with token/key material and background work still alive.

Even on a fully successful logout, `SyncEngine::logout` clears only
`encryption_key` and the in-memory `AppState`; it never touches `LocalStore`.
The plaintext clipboard cache (`{profile}/clipboard/{uuid}.payload`/`.json`) is
left on disk indefinitely, `LocalStore.profile_id` is never reset to `None`
(`crates/client/src/local_store.rs:35-41`), and
`profile_root() = base_dir/hex(sha256(data_key))` is deterministic from the
passphrase (`crates/client/src/engine.rs:196-197`, `:1151`), so re-login with
the same passphrase re-attaches to the exact same plaintext directory. No API in
client/daemon/bridge deletes this cache, so logout is not a secure erase — the
standard hand-off-the-laptop action leaves every cached secret in place.

This is a correctness and privacy issue, especially on shared devices, and it
also stacks duplicate background work because `logout` does not cancel the tasks
spawned by `finish_auth` (see the duplicate-background-work item below).

Recommended fix: make logout local-first and destructive. Best-effort
`remove_dir_all(profile_root())` (or at least the clipboard dir), reset
`LocalStore.profile_id` to `None`, and clear the local token, encryption key,
app state, and background task handles regardless of server reachability; then
best-effort revoke the server session and surface a non-blocking warning if
revocation failed. Combine with the `0600`/`0700` permission fix.

## P2: Object Delete Unlinks Payload Files After Commit With No Reconciliation

`delete_object` commits the transaction that removes the `objects` row (cascading
payload rows) and inserts the `file.deleted` event
(`crates/server/src/routes/objects.rs:1061`), _then_ calls `remove_paths(paths)`
to unlink the ciphertext `.bin` files (`:1070`). `remove_paths` is best-effort
and logs-and-continues on non-`NotFound` errors (`:1376-1388`). If the process is
killed, the disk fills, or an unlink fails in that gap, the rows are gone but the
ciphertext survives on disk, and nothing ever reclaims it:
`cleanup_orphan_object_uploads` only targets `Status.ne("complete")` rows
(`crates/server/src/cleanup.rs:171`) and there is no directory-scanning GC
anywhere in the server crate (every `remove_file` is keyed off a DB-derived
path).

A user who deletes a sensitive file expects the encrypted blob gone from the
server; after a crash or unlink error during delete it remains on disk
indefinitely with no record and no reclamation, defeating the delete intent.
This is also an unbounded disk-leak vector. (No confidentiality break beyond
ciphertext the server already held — a durability/data-retention defect.)

Recommended fix: record deletions durably and reconcile — a tombstone row
written before commit that a cleanup pass drains by retrying unlinks until
cleared, or move file removal into the same unit of work. At minimum add a
periodic reconcile of `objects_dir()` against `object_payloads.ciphertext_path`
that deletes files with no backing row.

## P2: Orphan-Upload Cleanup Has A Delete/Complete Race (Both Directions)

`cleanup_orphan_object_uploads` (`crates/server/src/cleanup.rs:171-207`) captures
orphan object IDs by filter (`Status.ne("complete")` + `CreatedAt.lt(cutoff)`,
`:176-183`), removes their payload _files_ (`:187-194`), then re-runs the
deletion **by filter** (`:197-202`) rather than by the captured ID set, across
three un-transacted awaits. Two interleavings both corrupt state:

- An upload that _completes_ in the window leaves a now-`complete` object whose
  blob file was already unlinked — download 404 / data inconsistency
  (`crates/server/src/routes/objects.rs:929`).
- A concurrent `upload_payload` that recreates the `.bin` after cleanup unlinked
  it (`remove existing final, then rename tmp → final`,
  `crates/server/src/routes/objects.rs:374-375`) leaves a file on disk _after_
  the filtered delete cascades the row away — a stranded file with no backing
  row, never reclaimed (no directory sweep exists).

Deleting by the captured ID set alone closes only the first direction; file
_creation_ in `upload_payload` must also be fenced against the concurrent orphan
delete.

Recommended fix: delete by the captured ID set in a single transaction with a
status re-check, and have `upload_payload` abort and unlink its tmp/final if the
object/payload row no longer exists or is no longer uploadable after the rename
(ideally under a row lock). Add the periodic `objects_dir` vs `object_payloads`
reconcile noted above.

## P2: No Cap On Payloads Per Object

`ObjectInitRequest.payloads` is validated only as `length(min = 1)`
(`crates/api-types/src/lib.rs:248`); `init_object` builds one `payload_models`
entry per payload (`crates/server/src/routes/objects.rs:155-177`) and one
`upload_urls` entry per non-inline payload (`:145-153`), with `insert_many`
inside the write transaction (`:213`), and no upper bound on payload count.
Within the implicit request-body limit a client can declare tens of thousands of
payload rows and upload URLs in one request — a large batch insert, a long
write-lock hold, and a large response. The client only uses one payload per
object today, so a tight cap is free.

Recommended fix: add an explicit maximum payload count.

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

## P2: Client File Upload/Download Is All In Memory With No Descriptor Check

The server streams payload uploads to disk, but the client reads full plaintext
files into memory, encrypts to a full ciphertext buffer, uploads that buffer,
downloads payload bytes into a full buffer, decrypts into another buffer, and
writes the plaintext file. The `INLINE_OBJECT_PAYLOAD_MAX_BYTES = 64 KiB` split
(`crates/client/src/engine.rs:22`) only changes which API call carries the
bytes. `download_object_payload` does `checked_response(resp).await?.bytes().await?.to_vec()`
(`crates/client/src/api_client.rs:335-352`) with no `Content-Length` cap, and
neither `download_file_bytes` (`engine.rs:626-653`) nor the clipboard download
path (`engine.rs:819-852`) compares the received length/hash against the
descriptor's `ciphertext_size`/`sha256_ciphertext` (`crates/api-types/src/lib.rs:281-286`),
even though upload computes and sends both. Because a payload download is
auto-triggered on every WS event (`engine.rs:982-985`), a malicious or
compromised server can answer any payload GET with a multi-GB body → client OOM.

Recommended fix: check the declared size before reading on upload; on download,
enforce a size bound and verify received length and SHA-256 against the
descriptor before decrypting. Consider capping max file size until streaming
encryption exists; longer term use a chunked encrypted file format with a
manifest hash.

## P2: Timestamps Are Stored And Compared As Text

The schema stores timestamps as text and code compares them lexicographically
for sessions, access keys, cleanup, and pagination. `ObjectListQuery.before` is a
free-form `Option<String>` (`crates/server/src/routes/objects.rs:684`) injected
into `CreatedAt.lt(before)` (`:757-759`) with no RFC3339/UTC validation against a
text column. All server-generated timestamps come from
`chrono::Utc::now().to_rfc3339()` (fixed `+00:00` form), so intra-server compares
are stable and SeaORM parameterizes the value (not SQL injection). The risk is
cross-source mixing: the client-supplied `before` is unvalidated, and any future
backfill in a different format (`Z`, fractional seconds, non-UTC offsets)
silently changes ordering for everyone.

Recommended fix: store integer Unix milliseconds, or normalize one fixed-width
UTC string everywhere and validate `before`, or parse to `DateTime<Utc>` before
comparing.

## P3: Username Enumeration Via Register-Start And Challenge Timing

The `challenge` endpoint uses opaque-ke's fake-record path for unknown users, so
its _response content_ no longer reveals account existence. Two gaps remain:

- `register_start` returns `409 "Username already taken"`
  (`crates/server/src/routes/auth.rs:186-191`), a direct content oracle for any
  access-key holder.
- `challenge` runs an extra AEAD `unwrap_opaque_password_file` only on the
  known-user path (`:73-80`); the fake path skips it (`:83`), leaving a timing
  signal.

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

## P3: Data-Encryption Key Is Copied Out Of Its `Zeroizing` Wrapper

`engine.rs` holds the master data key as `Zeroizing<[u8;32]>`
(`crates/client/src/engine.rs:45`), but `refresh` dereferences it with
`**encryption_key` into a bare `let encryption_key: [u8;32]` (`:780`). That copy
is borrowed across the clipboard hydration stream and the file loop (`:785-814`)
and dropped at `:816` with no zeroization. Every other key path keeps the
`Zeroizing` via the `RwLock` guard and passes `&[u8;32]` (put-clipboard
`:316-326`, put-file `:570-575`, download-file `:645-649`), and the downstream
decrypt fns all take `&[u8;32]`, so a borrow or a scoped `Zeroizing` copy would
have sufficed. Separately, `OpaqueLoginFinish.session_key: Vec<u8>`
(`crates/core/src/crypto.rs:41`, `:451`) is not zeroized, though it is discarded.

This is a defense-in-depth memory-hygiene regression, not directly exploitable:
under the post-crash core-dump / swap / same-host memory-read exposure the data
key is already resident, so the incremental risk is the extra non-zeroized copy
living slightly longer in a second location. It undercuts the crate's otherwise
consistent zeroization discipline.

Recommended fix: keep the key behind the `RwLock` guard and pass `&**guard` into
the decrypt helpers, or clone into a fresh `Zeroizing<[u8;32]>` instead of a bare
array. Wrap `session_key` in `Zeroizing` for consistency.

## P3: Server URL Validation Is Not Encapsulated

`validate_server_url` (`crates/client/src/api_client.rs:400`) rejects embedded
credentials and plain HTTP except for loopback / Android-emulator hosts, but it
only runs on the login/register paths. `ApiClient::set_base_url` (`:51`) and
`SyncEngine::set_base_url` accept any string, and object/list/download build
URLs straight from `base_url`.

Recommended fix: validate in `set_base_url`, or use a `ServerUrl` newtype so
invalid URLs are not representable.

## P3: Daemon IPC Listener Has No Handshake/Idle Timeout And No Connection Cap

The accept loop unconditionally `tokio::spawn`s `handle_connection` per accepted
Unix-socket connection (`crates/daemon/src/main.rs:229-246`, spawn at
`:238-240`) with no semaphore or count limit. Each task first calls
`keychain::load_or_create_ipc_secret(data_dir)` per connection
(`crates/daemon/src/handler.rs:143`), then blocks in `read_limited_line` →
`reader.fill_buf().await` (`:168`, `:293`) with no read/handshake deadline. A
same-uid process can open many connections that never send a newline, pinning
tasks and fds forever and forcing repeated keychain reads, with no completed
handshake required.

Unlike the documented same-user "can issue commands" tradeoff, this requires no
IPC secret and no completed handshake — every pre-auth connection consumes
resources. Severity is tempered because a same-user process is already heavily
trusted and can disrupt the daemon more simply; the impact is availability only.

Recommended fix: wrap the handshake in `tokio::time::timeout` and add a
per-connection idle timeout; bound concurrent connections with a `Semaphore`
acquired before spawning; load the IPC secret once at startup instead of
per-connection.

## P3: Forwarded-IP Resolution Falls Back To The Attacker-Controllable Leftmost Hop

`first_untrusted_forwarded_ip` walks the forwarded chain right-to-left for the
first non-trusted IP; if every entry is a trusted proxy it falls back to
`chain.first().copied()` (`crates/server/src/rate_limit.rs:171`) — the leftmost,
most attacker-controllable value. Since `client_ip_from_headers` does
`.unwrap_or(peer_ip)` (`:137`), returning `None` would have correctly resolved to
the trusted-proxy peer IP; the fallback deliberately prefers a client-supplied
header. The rate limiter keys its per-client buckets on this value (`:39-41`),
and CIDR trusted proxies are supported. This also contradicts the documented
invariant that the function returns the rightmost untrusted hop
(`docs/backend-review-flow.md:231`).

When the entire forwarded chain falls inside the trusted set (a misconfigured
proxy that forwards client `X-Forwarded-For` verbatim, or requests genuinely
originating inside the trusted CIDR with an attacker-controlled leftmost token),
an attacker can pin/rotate/collide the per-IP auth rate-limit key, evading the
per-client cap (default 10/min) and leaving only the coarse global cap on OPAQUE
brute force. A correctly configured reverse proxy appends the genuine public
peer IP rightmost, so an external attacker does not normally reach the branch —
but the fallback is still strictly worse than the adjacent `unwrap_or(peer_ip)`.

Recommended fix: drop the `.or_else(|| chain.first().copied())` fallback so the
function returns `None` when every chain entry is trusted (then `peer_ip` is
used); never select the leftmost entry. Optionally cap the trusted hop count and
reject over-long chains.

## P3: Single Shared Global Auth Rate-Limit Bucket

`RateLimiter::check()` returns
`auth_by_ip.check_key(&ip).is_ok() && auth_global.check().is_ok()`
(`crates/server/src/rate_limit.rs:39-41`). `auth_global` is a single unkeyed
`DefaultDirectRateLimiter` (default `auth_global_per_minute = 600`,
`crates/server/src/config.rs:16-18`) gating all four public auth routes. A single
source (or a handful of IPs) emitting ~600 cheap auth requests/min exhausts the
shared global budget, so every other user's challenge/login/register returns
`429`; the per-IP cap does not protect the global gate. It is throttling, not
indefinite lockout (the token bucket refills each minute), and a global ceiling
being global is partly intentional load-shedding — the residual weakness is the
absence of per-source fairness on the global ceiling. It is weaker still when
combined with the leftmost-XFF key spoofing above.

Recommended fix: do not gate legitimate clients on a single global counter
shared with an abuser — set the global ceiling well above realistic aggregate
legitimate load with separate alerting, add a stricter per-IP/per-username
limiter the abuser cannot share with victims, or shed load by source. Document
the intended global ceiling relative to expected concurrency.

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

## P3: OPAQUE Documentation Is Inverted And `id_U` Binds To The Mutable Username

`docs/opaque.md` states `opaque_server_setup` is per-user not global, that this
is why the fake-record enumeration defense "does not apply", and that
`id_U = clipper:user:{user_id}:passphrase:v1` (`docs/opaque.md:27-49`, `:210-218`).
The code does the opposite: it reads a single **global** setup from
`server_config` (`unwrap_global_opaque_server_setup`,
`crates/server/src/routes/auth.rs:489`; "server-wide setup" comment at `:193`),
computes `id_U = clipper:user:{username}:passphrase:v1` from the
client-submitted username (`:485-498`, the only `id_U` derivation in the repo),
and the `challenge` handler _does_ use opaque-ke's fake-record path (`:56-84`).
`docs/backend-review-flow.md` repeats the inversion (per-user setup at `:82`/`:152`,
`id_U` bound to the immutable UUID at `:161`, `fake_sk` not exercised at `:225`).

There is no live exploit today (no username-rename path exists; the username
lookup and `id_U` are byte-exact, so no auth desync), but the threat-model
documentation a reviewer or operator relies on is wrong about global-vs-per-user
setup, the OPRF identity binding, and whether the enumeration defense applies. It
also masks a latent fragility: because `id_U` binds to the _mutable_ username
rather than the immutable `user_id`, any future rename feature would silently
invalidate every existing login (the OPRF key changes).

Recommended fix: correct `docs/opaque.md` and `docs/backend-review-flow.md` to
reflect the global `ServerSetup`, `id_U = username`, and the live fake-record
path. Bind `id_U` to the immutable `user_id` (resolve username → user_id first)
so a future rename cannot break login, and document the username/`id_U`
normalization rule.

## Lower-Priority Hygiene

- **Implicit SQLite durability.** `connect_db` (`crates/server/src/state.rs:101`)
  connects with `Database::connect("sqlite:…?mode=rwc")` and no `ConnectOptions`.
  Foreign-key enforcement (cleanup/delete cascade correctness depends on it),
  WAL, and `busy_timeout` rely on sqlx defaults; a SeaORM/sqlx bump could change
  them silently. Set them explicitly, plus a deliberate pool size.
- **`serde_json::to_string(...).unwrap()` in the WS send path** (`ws.rs:78`,
  `:97`, `:110`, `:143`). Won't panic for the current `WsServerMessage` shape
  (`String`/`i64` only), but `.unwrap()` in a per-connection network handler is a
  latent panic hazard and reads worse than the daemon's `if let Ok(...)` pattern.
- **`profile_id_from_encryption_key`** (`engine.rs:1151`) is
  `hex(sha256(data_key))` — the data key doing double duty through a bare hash.
  Derive it via HKDF with its own label for consistency with the rest of the
  crypto.
- **No explicit `DefaultBodyLimit`.** Buffered (`init`/`complete`) routes and the
  auth routes are bounded only by axum's implicit ~2 MiB request-body default —
  a framework default, not an explicit policy. The `init_object` inline check
  compares against `max_file_blob_bytes` (512 MiB), which is unreachable through
  that body limit. State the policy explicitly with a per-route `DefaultBodyLimit`.
- **`max_file_meta_ciphertext_bytes` is a dead config knob.**
  `LimitsConfig.max_file_meta_ciphertext_bytes` is fully wired (default 64 KiB at
  `crates/server/src/config.rs:26`, garde at `:214`, CLI flag at `:408-409`) but
  read nowhere outside `config.rs` and its tests. The only object-metadata size
  gate is `init_object` (`crates/server/src/routes/objects.rs:44`), which checks
  `max_object_meta_ciphertext_bytes` unconditionally for every kind including
  `File`. An operator tightening `max_file_meta_ciphertext_bytes` gets no effect;
  raising the object-meta limit for clipboard unknowingly also raises the
  file-metadata ceiling. Either enforce the file knob for `ObjectKind::File`
  (with a test), or delete it and document that one limit governs all kinds.
- **Unbounded `device_name` / `platform` on auth requests.** `LoginRequest` and
  `RegisterFinishRequest` validate these only as `length(min = 1)` with no
  maximum or charset (`crates/api-types/src/lib.rs:143-147`, `:188-191`), while
  `validate_username` correctly bounds `3..=32` and charset. Values flow into
  `devices.name`/`platform` (`crates/server/src/routes/auth.rs:570-578`) and are
  echoed in `DeviceInfo`/`BootstrapResponse`. A malicious authenticated client
  can store multi-hundred-KB strings (one-time, self-scoped, bounded by the body
  limit; re-login on an existing device updates only `last_seen_at`). Add
  `length(max = …)` and mirror the username charset discipline if rendered.
- **`ttl_days` / `event_log_retention_days` can panic on absurd config.**
  `orphan_upload_ttl_secs` carries `custom(validate_chrono_seconds)`
  (`crates/server/src/config.rs:267`), but `clipboard.ttl_days` (`:235-238`) and
  `cleanup.event_log_retention_days` (`:263-268`) are validated only
  `range(min = 1)` with no maximum, yet both feed `chrono::Duration::days(...)`
  (`crates/server/src/routes/objects.rs:135`, `crates/server/src/cleanup.rs:154`),
  which panics on overflow. Operator-controlled self-DoS only (defaults 7 and 3).
  Apply an upper-bound validator, or use `Duration::try_days` and surface a typed
  error.
- **No upper bound on `list.max_limit`.** `ListConfig.max_limit` is validated
  `range(min = 1)` plus a check that `default_limit <= max_limit`
  (`crates/server/src/config.rs:248-253`, `:518-527`) with no maximum, and is
  TOML/CLI-overridable. `list_objects` clamps `limit` to `max_limit`
  (`crates/server/src/routes/objects.rs:740-743`) then queries `.limit(limit + 1)`
  on a `u64` (`:762`); at `max_limit = u64::MAX` the `+ 1` wraps to 0 (broken
  pagination in release, panic in debug). Add a hard maximum and use
  `saturating_add(1)`.
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
- **Flutter lockfile is uncommitted, so CI caching is moot and the dependency
  audit silently skips the Dart tree.** `.gitignore` excludes `app/pubspec.lock`
  (`.gitignore:30`), so `.github/workflows/ci.yml`'s pub cache key
  `hashFiles('app/pubspec.lock')` (`:147`) hashes an empty set and
  `flutter pub get` (`:150`) re-resolves caret ranges (e.g.
  `flutter_rust_bridge: ^2.12.0`, `app/pubspec.yaml:9-27`) fresh every run. The
  same uncommitted lockfile means the `audit` flake app's
  `osv-scanner scan source -r` (`flake.nix:189`; osv-scanner respects
  `.gitignore`) never scans the main app's dependency tree
  (`flutter_rust_bridge`, `file_picker`, `build_runner`, `ffigen`, and
  transitives), yet `nix run .#audit` exits success — the frontend has neither
  pinning nor scanning, in contrast to SHA-pinned Actions and `--locked` Cargo.
  For an E2EE app a compromised transitive Dart dependency can exfiltrate
  plaintext before encryption. Commit `app/pubspec.lock`, run
  `flutter pub get --enforce-lockfile`, fix osv-scanner coverage (`--no-ignore`
  or a dedicated Dart advisory scan), and document in `osv-scanner.toml` which
  manifests are actually covered.
- **Android release manifest allows cleartext traffic globally.**
  `app/android/app/src/main/AndroidManifest.xml` sets
  `android:usesCleartextTraffic="true"`, even though the documented production
  model requires HTTPS outside loopback/emulator development. Move cleartext
  allowance to debug/dev configuration or a narrow network security config.

## Accepted / Intentional Tradeoffs (Residual Risks By Design)

- **Same-user local IPC trust (P0, out of scope).** The daemon hardens the
  socket against _cross-user_ access (private `0700` runtime dir, `0600`
  socket, HMAC-SHA256 challenge/response, 32 MiB line cap). The residual gap is
  _same-user_: the IPC secret is keychain-backed on macOS but a `0600` file on
  Linux (`crates/daemon/src/keychain.rs`), so any same-user process can complete
  the handshake and issue commands (including `UploadFile`/`DownloadFile` with
  arbitrary paths). If same-user malware enters scope, move to OS-mediated app
  identity / user consent and add separate authorization for the file-path
  commands. (This covers path _arbitrariness_ only; the download egress file
  permissions and symlink-following are a real defect — see the P2 above.)
- The client `local_store` keeps clipboard plaintext on disk by design; only
  the network boundary is encrypted. (The _permissions_ on that cache and on file
  downloads, and the fact that logout does not wipe it, are real P2 bugs above,
  not part of this tradeoff.)
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
   AEAD AAD and cross-check the returned blob client-side, or move to signed
   object envelopes (P1).
2. WS/sync correctness cluster: subscribe to the broadcast channel before the
   replay query and flush/dedup-by-seq the buffered live events; track the oldest
   retained seq per user and `Invalidate` on gap; cap replay; and advance the
   client `last_seq` only after refresh succeeds (P1).
3. Decrypted-plaintext egress and retention: write file downloads at `0600` with
   `O_NOFOLLOW`, apply `0600`-in-`0700` to the local cache, and make logout
   destructive and local-first regardless of server reachability (P2).
4. Cleanup/delete durability and races: delete orphan rows by captured ID in one
   transaction with a status re-check, fence `upload_payload`'s rename, make
   `delete_object` reconcile via tombstone, add a directory-vs-DB GC, and cap
   payloads per object (P2).
5. Background task cancellation on re-auth/logout; object-by-ID metadata lookup
   for download; client download size/hash checks against the descriptor (P2).
6. Release readiness: align app versioning; commit `app/pubspec.lock` with
   `--enforce-lockfile` and fix osv-scanner Dart coverage; restrict Android
   cleartext traffic.
7. Rate-limit hardening: drop the leftmost-XFF fallback and add per-source
   fairness / a sane global ceiling; add IPC listener handshake/idle timeouts and
   a connection cap; add config upper bounds (`max_limit`, ttl/retention days,
   `device_name`/`platform`) and an explicit `DefaultBodyLimit`; resolve the dead
   `max_file_meta_ciphertext_bytes` knob.
8. Hygiene/cleanup: validated `before` cursor and integer timestamps; redact
   `Debug`, add `Zeroizing`, and stop copying the data key out of its wrapper;
   validate in `set_base_url`; ordered challenge eviction; restrict CORS;
   uniform challenge/register responses (or accept username enumeration
   explicitly); explicit SQLite pragmas; correct the OPAQUE docs and bind `id_U`
   to the immutable `user_id`.
