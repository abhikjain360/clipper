# Server Resource Limits

This document describes the abuse and resource-limit controls the Clipper
server enforces today: request rate limiting, per-user storage quotas, and the
in-memory caps that bound WebSocket and auth state. It reflects the code in
`crates/server/src`, not an aspirational policy.

All of the numbers below are defaults from `crates/server/src/config.rs`
(`DEFAULT_CONFIG`). Every value is overridable via the config TOML, a CLI flag,
or (for trusted proxies) an environment variable, and is validated by `garde`
before the server starts. Where a limit is a hard-coded constant with no config
knob, that is called out explicitly.

## Rate Limiting

Rate limiting lives in `crates/server/src/rate_limit.rs` and is built on the
[`governor`](https://docs.rs/governor) crate. Each bucket is a `governor`
keyed or direct rate limiter created with `Quota::per_minute(N)`. Because no
`.allow_burst()` override is supplied, this is a GCRA limiter whose steady-state
rate is `N` per minute and whose burst capacity is also `N`: a fresh key may
spend `N` cells immediately, after which cells replenish at `N`/minute. A
rejected check does not consume a cell.

The `RateLimiter` holds six independent buckets:

| Bucket | Key | Default `/min` | Config field |
| --- | --- | --- | --- |
| `auth_by_client` | resolved client IP (see keying below) | 10 | `rate_limit.auth_per_client_per_minute` |
| `auth_by_username` | SHA-256 of the submitted username, truncated to 16 bytes | 30 | `rate_limit.auth_per_username_per_minute` |
| `auth_global` | none (one direct bucket for the whole server) | 3000 | `rate_limit.auth_global_per_minute` |
| `api_by_client` | resolved client IP | 2400 | `rate_limit.api_per_client_per_minute` |
| `api_by_user` | authenticated `user_id` (UUID) | 1200 | `rate_limit.api_per_user_per_minute` |
| `ws_tickets_by_user` | authenticated `user_id` (UUID) | 30 | `rate_limit.ws_tickets_per_user_per_minute` |

`garde` rejects any of these set to `0`. There is no separate burst knob.

### Client IP keying

The "resolved client IP" for the per-client buckets is computed by
`client_ip_from_headers`. By default the TCP peer address is used directly.
`Forwarded` / `X-Forwarded-For` / `X-Real-IP` headers are honored **only** when
the peer is in the configured `server.trusted_proxies` set (IPs or CIDR ranges,
empty by default), in which case the rightmost untrusted address in the
forwarded chain is taken. Untrusted peers cannot spoof their key by sending
forwarding headers.

The IP is then canonicalized by `client_key`:

- IPv4 (including IPv4-mapped IPv6) is keyed exactly.
- IPv6 is masked to its `/64` prefix, so a client that controls an IPv6 block
  shares one bucket per `/64` rather than getting a fresh key per address.

### Auth routes (`public_auth` router)

The auth routes are wired in `serve()` (`crates/server/src/main.rs`) behind a
single `route_layer` running `auth_rate_limit_middleware`:

- `POST /api/auth/register/start`
- `POST /api/auth/register/finish`
- `POST /api/auth/challenge`
- `POST /api/auth/login`

`auth_rate_limit_middleware` calls `check_auth(ip)`, which checks
`auth_by_client` **first** and only then `auth_global`. Ordering matters: a
client that is already over its per-client budget is rejected before it can
spend a cell from the shared global bucket. `auth_global` is therefore a
capacity ceiling sized to sustainable OPAQUE/Argon2 work, not the primary
control.

The per-client middleware cannot see the username (the body is not yet parsed),
so the **per-username** bucket is enforced separately, inside the `challenge`
handler (`crates/server/src/routes/auth.rs`), immediately after the request
body is decoded:

```rust
if !state.rate_limiter().check_auth_username(&req.username) {
    return Err(rate_limited_error());
}
```

This backstops distributed password guessing that rotates source addresses to
stay under every per-client bucket. It is checked before the server unwraps the
OPAQUE password file or runs the OPAQUE login-start math. The per-username
bucket is **not** applied to `register_start`, `register_finish`, or `login`;
only `challenge` consults it.

### Authenticated API routes (`authed` router)

The authenticated routes:

- `POST /api/auth/logout`
- `POST /api/ws-ticket`
- `POST /api/objects/init`
- `GET` / `PUT /api/objects/{id}/payloads/{payload_id}`
- `POST /api/objects/{id}/complete`
- `GET` / `DELETE /api/objects/{id}`
- `GET /api/objects`
- `GET /api/ws`

are wrapped by three layers. From outermost to innermost:

1. `api_rate_limit_middleware` → `check_api(ip)` against `api_by_client`. This
   runs **before** `auth_middleware`, so the per-client API budget bounds the
   database cost of invalid- or missing-token floods.
2. `auth_middleware` (`crates/server/src/auth.rs`) resolves the bearer token
   into `AuthInfo`.
3. `user_rate_limit_middleware` → `check_api_user(user_id)` against
   `api_by_user`. It reads `AuthInfo` from the request extensions, so it must
   sit inside `auth_middleware`.

`/api/ws` (the authenticated WebSocket upgrade) is part of this router, so the
upgrade request itself is rate-limited like any other API call. The long-lived
socket that follows is not metered by these buckets; its inbound traffic is
bounded by the message-size cap (below) and its fan-out by the broadcast
channel capacity (below).

The WebSocket ticket-minting limit is enforced inside the `mint_ws_ticket`
handler (`crates/server/src/ws.rs`) via `check_ws_ticket_user(user_id)`, in
addition to the `api_by_user` bucket that the router layer already applies.
This second, tighter per-user bucket exists because all minted tickets share
one in-memory map; unbounded minting by one account would let it churn that
shared map (see the per-user pending-ticket cap below).

### Routes with no rate-limit middleware

Two routes are merged at the top level of the router with **no** rate-limit
layer:

- `GET /api/health`
- `GET /api/ws-ticket/connect` (the ticket-redeeming WebSocket upgrade handled
  by `ws_ticket_handler`)

`ws_ticket_handler` is unauthenticated (it authenticates by consuming a ticket)
and does a SHA-256 plus an in-memory map lookup per request. Neither route is
covered by a per-client or global bucket. See "Gaps" below.

### Response on rejection

A rejected check returns `ApiError::from_code_with_message(ApiErrorCode::RateLimited, "Too many requests")`
(`rate_limited_error`). `ApiErrorCode::RateLimited` maps to **HTTP 429** in
`crates/api-types/src/lib.rs`. No `Retry-After` header is set.

### Pruning

`governor` keyed limiters retain per-key state. A background task spawned in
`serve()` calls `RateLimiter::prune()` every `rate_limit.prune_interval_secs`
(default 60), which runs `retain_recent` + `shrink_to_fit` on each keyed bucket
to drop idle keys and reclaim memory. The direct `auth_global` bucket holds no
per-key state and is not pruned.

## Per-User Storage Quotas

Storage quotas are enforced per authenticated user against two counters on the
`users` row, maintained as running totals:

- `users.storage_bytes`
- `users.object_count`

The quota logic lives in `crates/server/src/storage_quota.rs`; it is driven from
the object routes (`crates/server/src/routes/objects.rs`) and the background
cleanup (`crates/server/src/cleanup.rs`).

### Limits

| Limit | Default | Config field |
| --- | --- | --- |
| Aggregate stored bytes per user | 10 GiB (`10 * 1024^3`) | `limits.max_user_storage_bytes` |
| Object rows per user | 10,000 | `limits.max_user_objects` |

Both are validated to be non-zero and to fit in a signed 64-bit integer (they
are stored and compared as `i64` database counters).

### What counts toward the quota

The reserved byte amount for an object is computed by
`init_request_storage_bytes`, which sums **only** the declared
`ciphertext_size` of each payload in the init request (with a per-payload
`>= 0` check and a checked add that rejects overflow as `PayloadTooLarge`).

This means:

- **Payload ciphertext bytes count.** This is the encrypted blob/clipboard
  content, whether inline or streamed.
- **Object metadata ciphertext (`meta_ciphertext`) and the signed envelope do
  not count** toward `storage_bytes`. They are separately bounded only by
  `limits.max_object_meta_ciphertext_bytes` (default 64 KiB) per object and by
  the per-user `object_count` cap.
- Every successfully initialized object increments `object_count` by exactly 1,
  regardless of kind or payload count.

Both clipboard and file objects reserve quota at init time. (Clipboard objects
are additionally trimmed to `clipboard.max_items`, which releases their quota;
see below.)

### Where and how it is enforced

Reservation happens inside the `init_object` transaction, via
`reserve_user_storage_quota` → `storage_quota::try_reserve_user_storage`, after
the object and payload rows are inserted but before the transaction commits. The
reservation is a single conditional `UPDATE users` that both increments the
counters and asserts the post-increment values stay within bounds:

```rust
.col_expr(StorageBytes, StorageBytes + storage_bytes)
.col_expr(ObjectCount,  ObjectCount + 1)
.filter(Id.eq(user_id))
.filter(StorageBytes.lte(max_storage_bytes - storage_bytes))
.filter(ObjectCount.lte(max_objects - 1))
```

The update affects exactly one row only if both filters hold, so the check and
the increment are atomic under SQLite's write lock — concurrent inits for the
same user cannot race past the limit. `try_reserve_user_storage` also
short-circuits to `Ok(false)` if a single object's `storage_bytes` already
exceeds `max_storage_bytes`, and returns an error for invalid arguments
(negative bytes, `max_objects < 1`).

Note the per-object payload ceiling is enforced earlier and independently: in
`init_object`, any single payload whose declared `ciphertext_size` exceeds
`limits.max_file_blob_bytes` (default 512 MiB) is rejected with
`PayloadTooLarge` before any quota arithmetic. During upload,
`stream_body_to_payload_file` streams at most the declared `ciphertext_size`
and aborts the moment the body exceeds it, so the stored bytes can never exceed
what was reserved.

### Behavior on exceed

If the reservation update affects zero rows, `reserve_user_storage_quota`
returns `ApiError::from_code_with_message(ApiErrorCode::StorageQuotaExceeded, "User storage quota exceeded")`,
which rolls back the whole init transaction (the object and payload rows are
undone, and any staged inline payload files are removed on drop).
`ApiErrorCode::StorageQuotaExceeded` maps to **HTTP 507 Insufficient Storage**.

### Release

`storage_quota::release_user_storage` decrements both counters (guarded so they
never go negative) and is called whenever an object's bytes leave the system:

- **File delete** (`DELETE /api/objects/{id}`): inside the delete transaction,
  releasing `object_count: 1` and the summed payload bytes.
- **Clipboard trim** (`cleanup::trim_user_clipboard`, spawned after each
  clipboard init/complete and also run by the periodic cleanup loop): deletes
  clipboard objects beyond `clipboard.max_items` and releases their usage.
- **Orphan upload cleanup** (`cleanup::cleanup_orphan_object_uploads`): deletes
  never-completed objects older than `cleanup.orphan_upload_ttl_secs` and
  releases their usage.

`delete_objects_and_release_usage` recomputes the freed usage from the rows
being deleted (`object_usage_by_user`) inside the transaction and asserts the
deleted row count matches, so the counters stay consistent with reality.

## In-Memory Caps

These bound server memory independently of the rate limiters. They live in
`crates/server/src/state.rs`, `crates/server/src/ws.rs`, and the `auth` config
section.

### WebSocket inbound message size

`ws_handler` and `ws_ticket_handler` (`crates/server/src/ws.rs`) cap both the
maximum message size and the maximum frame size at
`WS_MAX_MESSAGE_BYTES = 64 KiB` (`64 * 1024`). Clients only ever send a small
JSON `Hello` and control frames, so this is set well below the transport
default to bound per-connection memory. **This is a hard-coded constant; there
is no config knob.**

### Per-user pending WebSocket tickets

Minted-but-unconsumed tickets live in a single in-memory `HashMap` shared by all
users (`AppStateInner::ws_tickets`). `create_ws_ticket` enforces a **per-user**
cap of `auth.max_pending_ws_tickets` (default 4096): while a user already holds
that many tickets, the oldest ticket *for that same user* (smallest
`expires_at`, since all tickets share a fixed 60-second TTL) is evicted before
the new one is inserted. Eviction is scoped to the minting user, so one
account's burst cannot displace another user's pending ticket. Expired tickets
across all users are also swept on each mint and on each consume. Tickets are
single-use and stored by SHA-256 of the ticket value, not the value itself.

The `ws_tickets_by_user` rate-limit bucket (default 30/min) is the first line of
defense; this per-user map cap is the hard backstop.

### Per-user broadcast channel capacity

Live sync uses one `tokio::sync::broadcast` channel per user
(`AppStateInner::ws_channels`), created lazily on first subscribe with capacity
`WS_BROADCAST_CAPACITY = 256` (hard-coded). A receiver that falls more than 256
events behind gets `RecvError::Lagged`; `handle_socket` responds by sending an
`Invalidate { target: "all" }` message and closing the socket so the client
re-syncs from scratch. Because channels are per-user, one user's burst can only
lag that user's own slow receivers, never another user's. Idle channels are
removed (`prune_idle_ws_broadcast_channel`) once their last receiver drops.

### Pending OPAQUE challenges and registrations

`auth.max_pending_challenges` (default 4096) caps two separate in-memory maps:
the OPAQUE login challenges (`AuthChallenge`) and the pending registrations
(`PendingRegistration`). Both are swept of expired entries (TTL
`auth.challenge_ttl_secs`, default 5 minutes) on each insert, and when still at
capacity an **arbitrary** existing entry is evicted to make room (unlike the
WS-ticket map, which evicts oldest-first and is per-user). These are global, not
per-user, caps.

## Configuration Summary

Resource limits are configured through `ServerConfig` (`crates/server/src/config.rs`).
Each section can be set in the config TOML, and most fields also have a CLI
override flag. The relevant sections:

- `[rate_limit]` — the six bucket rates plus `prune_interval_secs`.
- `[limits]` — `max_user_storage_bytes`, `max_user_objects`,
  `max_file_blob_bytes`, `max_file_meta_ciphertext_bytes`,
  `max_object_meta_ciphertext_bytes`.
- `[auth]` — `max_pending_challenges`, `max_pending_ws_tickets`,
  `challenge_ttl_secs`.
- `[clipboard]` — `max_items` (per-user clipboard retention) and `ttl_days`.
- `[server] trusted_proxies` — also settable via `CLIPPER_TRUSTED_PROXIES`
  (comma-separated IPs/CIDRs), which feeds client-IP resolution for the
  per-client buckets.

The WebSocket message-size cap and the broadcast channel capacity are **not**
configurable; they are constants in `ws.rs` and `state.rs` respectively.

## Gaps / Not Covered Today

These are factual gaps in the current controls, called out so they are not
mistaken for safeguards that exist:

- **No HTTP request body size limit.** The router has no `DefaultBodyLimit` /
  request-body-limit layer. `Postcard::from_request` (`routes/mod.rs`) reads the
  entire request body into memory with `Bytes::from_request` before any
  size-specific check runs. For `init_object`, that means the full postcard body
  — including all inline payload ciphertext and the metadata ciphertext — is
  buffered in memory before `max_object_meta_ciphertext_bytes` and the
  per-payload `max_file_blob_bytes` checks are evaluated. The only thing
  bounding that allocation is the `api_by_client` / `api_by_user` request-rate
  buckets, not a byte cap. (Streamed payload uploads via `PUT .../payloads/...`
  are the exception: they are bounded incrementally against the declared size.)

- **`/api/health` and `/api/ws-ticket/connect` are unthrottled.** Neither is
  behind a per-client or global rate-limit layer. `ws-ticket/connect` is
  unauthenticated and performs a hash + map lookup per request.

- **Metadata and envelope bytes are not charged to the storage quota.** Only
  payload ciphertext counts toward `storage_bytes`. The number of metadata-only
  bytes a user can accumulate is bounded only by `object_count`
  (`max_object_meta_ciphertext_bytes` × `max_user_objects`), not by the
  byte quota.
