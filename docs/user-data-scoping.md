# User Data Scoping

Clipper is multi-user. Access keys only authorize registration. Private objects,
revisions, payloads, devices, sessions, collab docs and sync events must be
scoped to the authenticated `user_id`.

SQLite has no PostgreSQL-style row-level security. Views, triggers or
application-defined functions can approximate some checks, but they do not form
a real policy layer. With a pooled SQLite connection, a "current user" variable
would also be fragile, because connection state is not request state. The
server therefore scopes data in application code.

## Tables

User-owned tables carry a user column with a foreign key to `users(id)`,
`ON DELETE CASCADE`:

- `objects.user_id`
- `event_log.user_id`
- `devices.user_id`
- `sessions.user_id`
- `collab_docs.owner_user_id`

Two tables are owned through a parent object instead:

- `object_revisions` belongs to `objects` through `object_id`. A revision is
  keyed by `(object_id, revision)`.
- `object_payloads` belongs to one revision through `(object_id, revision)`. A
  payload is keyed by `(object_id, revision, payload_id)`.

Neither carries its own `user_id`, so ownership is proven through the parent
object.

Private route handlers receive `AuthInfo` from `auth_middleware`
(`crates/server/src/auth.rs`). `AuthInfo` carries `session_id`, `user_id` and
`device_id`, resolved from the bearer session token. Every query that returns or
changes private data uses `auth.user_id` directly, and also `auth.device_id`
where only one device may act.

## How Handlers Scope Queries

There is no scoped database wrapper. Private handlers call SeaORM entities
directly (`objects::Entity::find()`, `object_payloads::Entity::find()`, and so
on) and add an explicit `user_id` filter at each call site.

### Objects (`crates/server/src/routes/objects.rs`)

Every object read and write filters on `objects::Column::UserId`:

- `init_object` checks for an existing object with
  `find_by_id(object_id).filter(UserId.eq(auth.user_id))`, so a UUID that
  belongs to another user is not an existence oracle. A foreign id falls through
  to the primary-key constraint on insert.
- `list_objects`, `get_object` and `download_payload` filter
  `UserId.eq(auth.user_id)`, join the head revision, and leave out tombstoned
  objects and collab objects. Clipboard reads are further limited to the
  retained set (`retained_clipboard_object_ids`, `ensure_object_read_retained`),
  which is filtered by user as well.
- `get_object_revision` and `download_revision_payload` go through
  `load_readable_revision`, which filters `UserId.eq(user_id)` and accepts only
  a complete revision.
- `object_for_upload`, used by `upload_payload` and `complete_object`, finds the
  object with `UserId.eq(user_id)` and takes its newest revision. It answers 403
  unless that revision's `source_device_id` is `auth.device_id`, so only the
  device that started a revision can upload its payloads and complete it.
- `revise_object` finds the head through `head_revision_for_write`, which
  filters `UserId.eq(user_id)`. Any device of the user may append a revision.
  `validate_object_envelope` checks the envelope signature against the
  requesting device's key, looked up with `devices::Column::UserId.eq(user_id)`.
- `advance_object_head` moves the head only on an `objects` row that matches
  both the object id and `UserId.eq(user_id)`.
- `purge_object` locks and reads the object with `UserId.eq(auth.user_id)`
  before it deletes the row, inside one transaction. It accepts only a
  tombstoned file or schedule object.
- `object_list_items` resolves each revision's source-device signing key with
  `devices::Column::UserId.eq(user_id)`, so it never returns another user's
  device key. A revision whose `source_device_id` is NULL, because its device
  was removed, returns `source_device_signing_public_key = None`.

### Revisions and payloads

Handlers first prove that the parent object belongs to the caller, then read
revisions and payloads by the object id they just checked:

- `upload_payload` and `complete_object` call `object_for_upload` before they
  touch a payload row.
- `download_payload` and `download_revision_payload` run the user-scoped object
  lookup and the clipboard retention check. Then `read_revision_payload` loads
  the payload by its full `(object_id, revision, payload_id)` key, limited to
  `complete` payloads. The payload query does not repeat the `user_id` filter:
  the object id is part of the key, and that object was just shown to belong to
  the caller.
- `idempotent_init_response` runs only after the user-scoped existence check in
  `init_object`, so its payload reads by object id are covered too.

### Collab docs (`crates/server/src/routes/collab.rs`)

- `list_collab_docs` filters `objects::Column::UserId.eq(auth.user_id)`.
- `get_collab_doc_meta`, `rename_collab_doc` and `delete_collab_doc` call
  `load_owned_collab_doc_id`. It finds the collab object by id, answers 404 if
  there is none, and answers 403 if the caller is not its owner.
- `create_collab_doc` sets `collab_docs.owner_user_id`, `objects.user_id` and
  the event's `user_id` from `auth.user_id`.
- The Y-sync WebSocket (`collab_ws_handler`) and the share metadata lookup
  (`get_share_meta`) are public routes. The share token is their only
  credential. On the socket, a missing doc, a non-collab object and a wrong
  token all get the same 403.

### Sync events (`event_log`)

Production code reads `event_log` in three places:

- `ws::get_latest_seq` filters `event_log::Column::UserId.eq(user_id)`, and
  `objects::Column::UserId.eq(user_id)` for published objects, to compute the
  live stream's `stream_start_seq`.
- `state::seed_event_seq` reads only the largest sequence number across
  `event_log`, `object_revisions` and `objects`, server-wide, to seed the
  in-memory sequence counter. It reads one non-private number, not user data.
- `cleanup_old_events` deletes rows older than the retention window for all
  users.

Every `event_log` write sets `user_id` to the caller. `insert_object_event` in
`routes/objects.rs` receives `auth.user_id`, and the collab create, rename and
delete handlers set it the same way.

### WebSocket sync is partitioned by user

Live sync does not query `event_log` per broadcast. The server keeps one tokio
broadcast channel per user (`AppState::ws_channels`, a
`HashMap<Uuid, broadcast::Sender<WsBroadcast>>` keyed by `user_id`):

- `subscribe_ws_broadcasts(user_id)` returns a receiver for that user's channel
  only, and `broadcast_ws_event` sends only to the channel for `event.user_id`.
- A connected socket subscribes with its own `auth.user_id`, so it can never
  receive another user's events. Within a user, `should_forward_live_broadcast`
  also drops events whose `source_device_id` is the receiving device, so a
  device does not hear its own writes.
- `WsBroadcast` carries a `user_id` field, but isolation between users comes
  from keying the channels by user, not from a filter on that field.

WebSocket tickets are minted per authenticated session and consumed once.
Ticket minting and the number of unconsumed tickets are both bounded per
`user_id` (`mint_ws_ticket`, `AppState::create_ws_ticket`).

### Devices, sessions and auth

- `auth_middleware` resolves the session by `token_hash`. `AuthInfo.user_id`
  comes from that session row, so all later scoping derives from the
  authenticated session, not from an id the client sends.
- `issue_session` binds a device to a user. If the requested `device_id`
  already exists under a different `user_id`, or with a different signing
  public key, it answers `409 Conflict`. Reusing an existing device at login
  also requires a valid device-key proof signature.
- `list_devices` and `delete_device` filter
  `devices::Column::UserId.eq(auth.user_id)`.
- `logout` deletes the session by `auth.session_id`.

### Maintenance code

Maintenance code in `crates/server/src/cleanup.rs` is not request-scoped and
runs across all users on purpose: trimming expired and excess clipboard items,
removing orphaned uploads, deleting old events and deleting expired sessions.
`trim_user_clipboard` takes an explicit `user_id`; the periodic sweeps go
through each user in turn or filter on time and status. Keeping this code in
its own module is what separates it from request-scoped code.

## Database Backstops

SQLite constraints back up the application scoping. They cannot protect reads,
but they stop some inconsistent rows.

Foreign keys:

- `devices.user_id` → `users(id)`, `ON DELETE CASCADE`.
- `sessions.user_id` → `users(id)` and `sessions.device_id` → `devices(id)`,
  both `ON DELETE CASCADE`.
- `objects.user_id` → `users(id)` and `objects.collab_doc_id` →
  `collab_docs(id)`, both `ON DELETE CASCADE`.
- `object_revisions.object_id` → `objects(id)`, `ON DELETE CASCADE`.
- `object_revisions.source_device_id` → `devices(id)`, nullable,
  `ON DELETE SET NULL`. Removing a device detaches the revisions it signed
  rather than blocking the removal or deleting them.
- `object_payloads(object_id, revision)` →
  `object_revisions(object_id, revision)`, `ON DELETE CASCADE`.
- `event_log.user_id` → `users(id)`, `ON DELETE CASCADE`.
- `collab_docs.owner_user_id` → `users(id)`, `ON DELETE CASCADE`.
- `users.access_key_hash` → `access_keys(key_hash)`, `ON DELETE RESTRICT`.

Other constraints:

- Unique: `users.username`, `users.access_key_hash`, `sessions.token_hash`,
  `object_payloads.ciphertext_path`, `collab_docs.share_token`.
- A complete revision must have a `created_seq`.
- An object with a head revision must have a `published_seq`, and a collab
  object has no head revision.
- Only file, schedule and collab objects may have `updated` or `deleted`
  events.
- `users.storage_bytes`, `users.object_count` and
  `object_payloads.ciphertext_size` must be zero or more.

The device foreign keys reference only `devices(id)`. The database therefore
does not stop a revision or a session from naming a device that belongs to
another user. That rests on application code: the source-device check in
`object_for_upload`, the user-scoped device-key lookup in
`validate_object_envelope`, and the device-ownership check in `issue_session`.

## Tests

These isolation properties have tests:

- `state::tests::ws_broadcasts_are_partitioned_by_user`: a broadcast for one
  user is not visible on another user's receiver.
- `state::tests::one_users_burst_does_not_lag_another_users_receiver`,
  `ws_ticket_capacity_is_per_user` and `ws_connection_cap_is_per_user`: one
  user's WebSocket load does not affect another user.
- `routes::auth::tests::delete_device_cannot_remove_another_users_device`.
- `routes::auth::tests::login_existing_device_requires_device_key_proof`.
- `routes::auth::tests::register_start_does_not_reveal_existing_username` and
  `challenge_for_unknown_user_is_indistinguishable`.

A scoped database helper, composite device foreign keys, and a two-user
isolation test for every private route are open; see `docs/issues.md`, entry 59.
