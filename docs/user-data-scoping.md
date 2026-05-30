# User Data Scoping

Clipper is multi-user: access keys only authorize registration, and private
objects, payloads, devices, sessions, and sync events must be scoped to the
authenticated `user_id`.

SQLite does not support PostgreSQL-style row-level security. It can approximate
some checks with views, triggers, or application-defined functions, but those
do not form a real policy layer for this server. With SeaORM and a pooled SQLite
connection, a "current user" variable would also be fragile because connection
state is not the same thing as request state.

Use application-level scoping as the primary guardrail.

## Current Shape

User-owned tables:

- `objects.user_id`
- `event_log.user_id`
- `devices.user_id`
- `sessions.user_id`

Indirectly user-owned tables:

- `object_payloads` belongs to `objects` through `object_id`; it does not carry
  its own `user_id`.

Private route handlers receive `AuthInfo` from auth middleware. Queries that
return or mutate private data should use that `AuthInfo.user_id` directly, or a
small scoped wrapper built from it.

## Recommended Pattern

Prefer a `UserScope` or `UserDb` helper for private route code:

```rust
let user_db = UserDb::new(state.db(), auth.user_id);
let object = user_db.object_by_id(object_id).await?;
let events = user_db.events_since(last_seq).await?;
```

That helper should be the only ergonomic place for user-owned reads and writes.
It should:

- Always filter `objects`, `event_log`, `devices`, and `sessions` by `user_id`.
- Scope payload access by first proving the parent `objects.user_id`.
- Provide explicit admin/cleanup methods separately, so global maintenance code
  does not look like request-scoped code.
- Keep direct SeaORM entity calls out of private route handlers where practical.

SeaORM cannot enforce this automatically. Direct calls like
`objects::Entity::find()` can always bypass a helper, so enforcement should be
structural and checked in review or CI. A simple `rg` check for raw entity access
outside the scoped module, auth code, cleanup code, migrations, and tests would
catch most accidental bypasses.

## Database Backstops

SQLite constraints are still useful. They cannot protect reads, but they can
prevent inconsistent cross-user rows.

Consider adding composite uniqueness and foreign keys where they match the
schema:

- `devices(id, user_id)` as a unique pair.
- `objects(source_device_id, user_id)` referencing `devices(id, user_id)`.
- `sessions(device_id, user_id)` referencing `devices(id, user_id)`.

This would stop an object or session from pointing at a device owned by another
user even if application code makes a mistake.

## Tests

Every private route should have at least one two-user isolation test:

- User A can access their own row.
- User B gets not found or forbidden for User A's row.
- Payload routes prove ownership through the parent object.
- WebSocket replay and bootstrap only expose the authenticated user's events.

These tests are the closest practical equivalent to RLS coverage while this
server stays on SQLite.

## If The Storage Backend Changes

If the server moves to PostgreSQL for hosted multi-tenant deployment, use native
PostgreSQL RLS for user-owned tables and keep the application-level scoping
helpers. The helpers make policy intent explicit in Rust; database RLS would be
the final defense against a missed filter.
