# User Data Scoping

Clipper is multi-user: access keys only authorize registration, and private
objects, payloads, devices, sessions, and sync events must be scoped to the
authenticated `user_id`.

SQLite does not support PostgreSQL-style row-level security. It can approximate
some checks with views, triggers, or application-defined functions, but those
do not form a real policy layer for this server. With SeaORM and a pooled SQLite
connection, a "current user" variable would also be fragile because connection
state is not the same thing as request state.

The server therefore uses application-level scoping as the primary guardrail.

## Current Shape

User-owned tables (each carries a `user_id` column with a foreign key to
`users(id)`, `ON DELETE CASCADE`):

- `objects.user_id`
- `event_log.user_id`
- `devices.user_id`
- `sessions.user_id`

Indirectly user-owned tables:

- `object_payloads` belongs to `objects` through `object_id` (foreign key to
  `objects(id)`, `ON DELETE CASCADE`); it does not carry its own `user_id`.
  Ownership is proven through the parent object.

Private route handlers receive `AuthInfo` from `auth_middleware`
(`crates/server/src/auth.rs`). `AuthInfo` carries `session_id`, `user_id`, and
`device_id`, resolved from the bearer session token. Every query that returns or
mutates private data uses `auth.user_id` (and, for source-device-restricted
mutations, `auth.device_id`) directly.

## How Scoping Works Today

There is **no `UserScope`/`UserDb` wrapper** in the codebase. Private route
handlers call SeaORM entities directly (`objects::Entity::find()`,
`object_payloads::Entity::find()`, `event_log::Entity::find()`, …) and attach an
explicit `user_id` filter at each call site. The patterns below are what is
actually implemented.

### Objects (`crates/server/src/routes/objects.rs`)

Every object read and mutation filters on `objects::Column::UserId`:

- `init_object` — pre-insert existence check is
  `find_by_id(object_id).filter(UserId.eq(auth.user_id))`, so a cross-user UUID
  collision cannot act as an existence oracle; a foreign id falls through to the
  primary-key uniqueness constraint on insert.
- `list_objects` / `get_object` / `download_payload` — filter
  `UserId.eq(auth.user_id)` plus `Status = "complete"` and
  `CreatedSeq IS NOT NULL`.
- `retained_clipboard_object_ids` / `ensure_object_read_retained` — filter
  `UserId.eq(auth.user_id)`; clipboard reads are additionally bounded to the
  retained-window set.
- `object_for_upload` (used by `upload_payload` and `complete_object`) — looks
  up the object with `UserId.eq(user_id)` and then additionally rejects the
  request if `source_device_id != auth.device_id`, so only the originating
  device can drive a pending object's payload upload/completion.
- `complete_object` / `set_object_created_seq` — the status-transition
  `update_many` filters both `UserId.eq(auth.user_id)` and (for completion)
  `SourceDeviceId.eq(auth.device_id)`, so the update is a no-op for any object
  not owned and authored by the caller.
- `delete_object` — looks up the object with `UserId.eq(auth.user_id)` first;
  the subsequent `delete_by_id` runs inside the same transaction, gated by that
  ownership check. Only `file` objects are deletable.
- `object_list_items` resolves source-device signing keys with
  `devices::Column::UserId.eq(user_id)`, so it never surfaces another user's
  device key. Objects whose `source_device_id` is NULL (source device reclaimed)
  contribute no lookup and return `source_device_signing_public_key = None`.

### Payloads — ownership proven through the parent object

`object_payloads` has no `user_id`. Payload access is scoped by **first proving
the parent `objects.user_id`**, then keying the payload by its
`(object_id, payload_id)` primary key:

- `upload_payload` / `complete_object` call `object_for_upload` (user- and
  source-device-scoped) before touching any `object_payloads` row, then select
  payloads by `object_id`.
- `download_payload` runs the user-scoped object lookup
  (`find_by_id(object_uuid).filter(UserId.eq(auth.user_id))`) and the retained
  check, and only then loads the payload by its `(object_id, payload_id)` PK
  filtered to `Status = "complete"`. The payload query itself does not repeat the
  `user_id` filter; it relies on the preceding object ownership proof. Because
  `object_id` is half of the payload PK and that object id was confirmed to
  belong to the caller, a foreign object id cannot reach another user's payload.
- `idempotent_init_response` is only reached after the user-scoped existence
  check in `init_object`, so its `object_id`-keyed payload reads are likewise
  gated.

This is the intended pattern and matches the requirement that payload routes
prove ownership through the parent object.

### Sync events (`event_log`)

`event_log` is read in exactly two production paths, both user-scoped:

- `ws::get_latest_seq` (`crates/server/src/ws.rs`) filters
  `event_log::Column::UserId.eq(user_id)` to compute the live stream's
  high-water `stream_start_seq`.
- `state::seed_event_seq` reads only the global maximum `seq` to seed the
  in-memory monotonic clock; it is a server-wide maintenance read of a single
  non-private scalar (the cursor value), not user data.

`event_log` writes (`insert_created_event`, the delete event in
`delete_object`) always set `user_id` to `auth.user_id`.

### WebSocket sync is partitioned by user

Live sync does not query `event_log` per broadcast. The server keeps a
**per-user tokio broadcast channel** (`AppState::ws_channels:
HashMap<Uuid, broadcast::Sender<WsBroadcast>>`, keyed by `user_id`):

- `subscribe_ws_broadcasts(user_id)` returns a receiver for that user's channel
  only; `broadcast_ws_event` sends only to the channel for `event.user_id`.
- A connected socket subscribes with its own `auth.user_id`, so it can never
  receive another user's events. Within a user, `should_forward_live_broadcast`
  additionally drops events whose `source_device_id` equals the receiving
  device, so a device does not echo its own writes.
- `WsBroadcast` carries a `user_id` field, but that is for routing/eviction
  bookkeeping; cross-user isolation comes from the per-user channel keying, not
  from a filter on the payload. A unit test
  (`ws_broadcasts_are_partitioned_by_user`) asserts a broadcast for one user is
  not visible on another user's receiver.

WebSocket tickets are minted per authenticated session and consumed once; ticket
minting and pending-ticket capacity are both bounded per `user_id` (see
`mint_ws_ticket` and `AppState::create_ws_ticket`).

### Sessions and auth

- `auth_middleware` resolves the session by `token_hash`; `AuthInfo.user_id`
  comes from that session row, so all downstream scoping derives from the
  authenticated session, not from any client-supplied id.
- `issue_session` binds a device to a user: if the requested `device_id`
  already exists under a different `user_id`, or with a different signing public
  key, the request is rejected with `409 Conflict`; reusing an existing device
  on login additionally requires a valid device-key proof signature.
- `logout` deletes the session by `auth.session_id`.

Maintenance code in `crates/server/src/cleanup.rs` (expired/excess clipboard
trim, orphan-upload reaping, old-event and expired-session deletion) is
deliberately **not** request-scoped: it operates across all users by design
(`trim_user_clipboard` takes an explicit `user_id`; the periodic sweeps iterate
the distinct user set or filter on time/status). This is the "explicit
admin/cleanup, separate from request-scoped code" split, achieved by module
boundary rather than by a helper type.

## Database Backstops

SQLite constraints back up the application scoping. They cannot protect reads,
but they prevent some inconsistent cross-user rows. What the migration
(`crates/server/src/migration/m20260312_000001_create_tables.rs`) actually
creates today:

Foreign keys that exist:

- `devices.user_id` → `users(id)` (`ON DELETE CASCADE`).
- `sessions.user_id` → `users(id)` and `sessions.device_id` → `devices(id)`
  (both `ON DELETE CASCADE`).
- `objects.user_id` → `users(id)` (`ON DELETE CASCADE`) and
  `objects.source_device_id` → `devices(id)` (nullable, `ON DELETE SET NULL`):
  reclaiming a device detaches the objects it created rather than blocking the
  delete or cascading into them.
- `object_payloads.object_id` → `objects(id)` (`ON DELETE CASCADE`).
- `event_log.user_id` → `users(id)` (`ON DELETE CASCADE`).
- `users.access_key_hash` → `access_keys(key_hash)` (`ON DELETE RESTRICT`).

Other useful constraints present: `users.username` and `users.access_key_hash`
are unique; `sessions.token_hash` is unique; `object_payloads.ciphertext_path`
is unique; `objects.created_seq` must be non-null whenever `status = complete`;
`users.storage_bytes`/`object_count` have `>= 0` checks (storage quota);
`object_payloads.ciphertext_size >= 0`.

> **Not yet implemented (still a suggestion).** The composite, cross-user
> foreign keys below are **not** in the migration. The device foreign keys
> reference only `devices(id)`, so the database does **not** stop an object or
> session from pointing at a device owned by *another* user. That invariant
> currently rests entirely on application code (`object_for_upload`'s
> `source_device_id` check and `issue_session`'s device-ownership check). Adding
> these would make the guarantee structural:
>
> - `devices(id, user_id)` as a unique pair.
> - `objects(source_device_id, user_id)` referencing `devices(id, user_id)`.
> - `sessions(device_id, user_id)` referencing `devices(id, user_id)`.

## Recommended Pattern (future hardening, not yet present)

SeaORM cannot enforce per-user scoping automatically, and today the
`user_id` filter is repeated by hand at every private call site. A direct call
like `objects::Entity::find()` that forgets the filter would silently read
across users; nothing structural prevents that. Enforcement is currently by
convention and review only.

A future improvement is a `UserScope` / `UserDb` helper for private route code:

```rust
let user_db = UserDb::new(state.db(), auth.user_id);
let object = user_db.object_by_id(object_id).await?;
let events = user_db.events_since(last_seq).await?;
```

Such a helper does not exist yet. If added, it should:

- Always filter `objects`, `event_log`, `devices`, and `sessions` by `user_id`.
- Scope payload access by first proving the parent `objects.user_id` (the
  pattern handlers already follow by hand).
- Provide explicit admin/cleanup methods separately, so global maintenance code
  does not look like request-scoped code (cleanup already lives in its own
  module).
- Keep direct SeaORM entity calls out of private route handlers where practical.

Enforcement would still need to be structural and checked in review or CI: a
simple `rg` check for raw entity access outside the scoped module, auth code,
cleanup code, migrations, and tests would catch most accidental bypasses.

## Tests

Several isolation properties are covered today:

- `ws::tests::ws_broadcasts_are_partitioned_by_user` (in `state.rs`) — a
  broadcast for one user is not visible on another user's receiver.
- `auth::tests` — device-id reuse across users is rejected; existing-device
  login requires a valid device-key proof; the username/challenge paths do not
  reveal whether an account exists.

The following remain worth adding for every private route: a two-user isolation
test where User A reads their own row, User B gets not-found/forbidden for User
A's row, payload routes prove ownership through the parent object, and WebSocket
replay/bootstrap only expose the authenticated user's events. These are the
closest practical equivalent to RLS coverage while the server stays on SQLite.

## If The Storage Backend Changes

If the server moves to PostgreSQL for hosted multi-tenant deployment, use native
PostgreSQL RLS for user-owned tables and keep the application-level scoping
(ideally promoted into the `UserScope`/`UserDb` helper above). The helper would
make policy intent explicit in Rust; database RLS would be the final defense
against a missed filter.
