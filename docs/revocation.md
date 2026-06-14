# Session & Device Revocation

> Status: **PROPOSAL / NOT IMPLEMENTED.** Today users can self-logout, but there
> is no flow to list or revoke other sessions or individual devices. A stolen
> 30-day bearer token cannot be invalidated without editing the database. This
> doc is the plan to close that. (Tracked as a finding in the security review.)

## Why

In the intended release model the operator hands out access keys to friends, so
access keys and bearer tokens are the circulating secret. End-to-end encryption
protects _content_ but does nothing for a leaked token or key: whoever holds a
valid bearer token reads that account's sync stream until it expires. Revocation
is the missing control.

## Current behavior

- Sessions are bearer tokens: random 32 bytes, stored SHA-256-hashed in
  `sessions`, 30-day TTL, scoped by `user_id`. Auth middleware hashes the
  incoming bearer and looks up the row on each request, so **deleting the row
  revokes the token immediately**. There is no in-memory auth cache.
- `sessions` has no rotation/refresh: valid for 30 days or until self-logout
  (`crates/server/src/routes/auth.rs`).
- `sessions.last_seen_at` already exists and auth middleware refreshes it at
  most once per minute.
- Devices live in `devices`, keyed by `(id, user_id)`, holding the Ed25519
  signing public key plus `last_seen_at`. Existing-device login requires
  proof-of-possession. The number of devices per user is now capped
  (`limits.max_user_devices`, enforced in `issue_session`; see
  [`server-resource-limits.md`](server-resource-limits.md)), but there is still
  no user-facing flow to retire a device — that is the gap this doc plans to
  close.
- The schema is already prepared for device retirement:
  `objects.source_device_id` is nullable with an `ON DELETE SET NULL` foreign
  key, so deleting a device detaches the objects it created (rather than the old
  `ON DELETE RESTRICT`, which would have blocked the delete) without losing the
  objects. A reclaim/revoke flow therefore only needs the endpoint and the
  session cascade, not a schema change to the objects table.

## Plan

All endpoints are scoped to the authenticated `user_id` — a user sees and revokes
only their own sessions/devices. No cross-user admin revocation yet (there is no
admin UI; note as future work).

### Sessions

- `GET /api/auth/sessions` — list the user's sessions by opaque session id, with
  created-at, last-seen, expires-at, source device, and a `current` flag. Never
  return the token or its hash.
- `DELETE /api/auth/sessions/:id` — delete that session row (revoke one token).
- `POST /api/auth/sessions/revoke-others` — delete all of the user's sessions
  except the current one ("log out everywhere else").

Use the existing `sessions.last_seen_at` column for the listing UI.

### Devices

- `GET /api/auth/devices` — list the user's devices (id, label if any,
  created-at, last-seen, `revoked` flag).
- `POST /api/auth/devices/:id/revoke` — mark the device revoked and cascade:
  delete all sessions bound to that device.

Add `state` (`active` | `revoked`) + `revoked_at` to `devices`. A revoked device
id cannot start a new session or re-register; reject it in the device-login and
registration paths.

**Object semantics on device revoke:** objects already signed by a revoked
device stay valid and decryptable — the envelope signature is server-checked
provenance, not the load-bearing authenticity mechanism (that is AEAD+AAD under
`K`; see [`object-envelopes.md`](object-envelopes.md)). Revocation stops _future_
use of the device id; it does not retroactively invalidate history. Document this
so the guarantee is not over-claimed. If a revoke also **deletes** the device row
(rather than just flagging it), the `ON DELETE SET NULL` foreign key nulls each
object's `source_device_id`; the list/get response then returns
`source_device_signing_public_key = None` and the client skips the (now
unavailable) provenance check while still verifying AEAD+AAD.

## Schema / migration

Migration in `crates/server/src/migration/*.rs`, then regenerate SeaORM entities
(`nix run .#server-entities`); do not hand-edit generated entities as the final
change.

- `devices`: add `state` (default `active`) and `revoked_at` (nullable).

## Out of scope (note, don't build yet)

- Token rotation / refresh and shorter TTLs — separate hardening; revocation is
  the must-have.
- Operator/admin cross-user revocation — wait for an admin surface.
- Future bearer-capability revocation — same `state = active | revoked` pattern,
  built with the feature-specific tables, not here.
