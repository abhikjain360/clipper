# Security Findings

This file lists current actionable security findings for clipper.

## Fixed in current working tree

- `register_start` now rejects malformed OPAQUE registration requests before
  running access-key Argon2id work.
- IPC authentication is mutual: the daemon returns a directional HMAC proof,
  socket-file ownership/mode validation is available to clients, and the Linux
  fallback no longer uses `/tmp`.
- Native HTTP response reads are bounded. Payload downloads stream into a capped
  buffer and must exactly match the expected descriptor size.
- Rust-owned passphrase and invite-key inputs at IPC, Tauri, wasm, mobile, and
  CLI boundaries now use `Zeroizing`; daemon IPC request buffers are zeroized.
- WebSocket pending-ticket capacity is enforced per `user_id`.
- The standalone browser entry point ships a CSP meta policy.
- CI checks now run on ordinary `pull_request` events instead of a label/merge
  gate.
- User-specific server wraps bind AEAD AAD to both column purpose and `user_id`.
- `FsTransaction::write_new` sets staged file mode to `0600` on Unix.
- Mobile relative data directories now resolve under the platform data
  directory.
- `scripts/clipper-server.ts` pipes generated access keys through stdin instead
  of process argv.

## Low

### Login challenge has a weak username-existence timing oracle

Location: `crates/server/src/routes/auth.rs:78`

Known users perform an AEAD unwrap and fetch a larger row; unknown users take a
fake-record path. That creates a weak timing and behavior asymmetry outside
opaque-ke's fake-record protection.

Status: residual timing risk. Equalizing row/no-row database behavior and AEAD
unwrap work would benefit from a deliberate dummy-record design and timing
measurements.

### Linux desktop depends on unmaintained GTK/glib 0.18 packages

Location: `Cargo.lock` (`glib 0.18.5`)

The Linux Tauri target pulls in unmaintained GTK/glib 0.18 dependencies,
including known RustSec advisories.

Impact: reachable only on the Linux desktop target, with no fixed version at
the pinned dependency line.

Fix: track upstream Tauri/GTK dependency updates and bump when a maintained line
is available.

### Passphrase can still have residual non-zeroizing copies

Location: `crates/daemon-types/src/protocol.rs:77`,
`crates/daemon/src/handler.rs:286`, `crates/client/src/engine.rs:124`,
`web/src-tauri/src/lib.rs:136`, `crates/web-wasm/src/lib.rs:37`,
`crates/mobile-uniffi/src/lib.rs:89`

Rust-owned IPC, Tauri, wasm, mobile, and CLI boundary inputs now move
passphrases and invite keys into `Zeroizing`, and daemon IPC line buffers are
zeroized. The lower-level client/OPAQUE APIs still accept borrowed `&str`, and
browser or foreign-language runtimes may keep their own copies outside Rust's
control.

Impact: passphrase bytes can remain in freed heap memory or swap.

Fix if this becomes a concern: make auth APIs take a secret wrapper type rather
than generic `&str`, and audit platform bindings for unavoidable runtime copies.

## Info

### Per-username auth limiter allows targeted account lockout

Location: `crates/server/src/rate_limit.rs`, `crates/server/src/routes/auth.rs`

The per-username budget on the OPAQUE challenge route caps distributed
password guessing, but an attacker who knows a username can deliberately
exhaust that budget from many addresses and keep the account rate-limited.
The quota is intentionally looser than the per-client limit, and the blast
radius is one named account rather than all users (which is what the former
global-bucket finding allowed).

Fix if this becomes a concern: count only failed challenge finalizations, or
key the bucket by (client, username).

### User data scoping relies on manual discipline

Location: `crates/server/src/routes/objects.rs`, `crates/server/src/ws.rs`

Server object and WebSocket queries are user-scoped, but handlers use raw SeaORM
entity calls instead of the documented `UserScope`/`UserDb` helper.

Risk: future handlers can accidentally omit the user scope without a helper or
CI guard catching it.

Fix: use a user-scoped database helper for private data access and add a CI
search guard for direct entity calls in sensitive routes.

### Client-supplied `created_at` controls item TTL for the user's own data

Location: `crates/server/src/routes/objects.rs:137`,
`crates/server/src/routes/objects.rs:151`

Inline object creation accepts a client-provided `created_at`, which influences
clipboard TTL and event-log retention for that user's objects.

Impact: a client can backdate or forward-date its own items.

Fix: use server time for retention decisions if client-controlled lifetime is
not intentional.

### macOS keychain IPC secret uses the default ACL

Location: `crates/daemon/src/keychain.rs:90`

The macOS keychain item storing the IPC secret uses the default access control
list.

Impact: another same-user process can read the secret once the login keychain is
unlocked, which weakens the daemon IPC boundary.

Fix: store the item with a restrictive access control policy.

### Dead `rsa` dependency remains in the lockfile

Location: `Cargo.lock`

`rsa 0.9.10` is present through sqlx MySQL metadata and has a known timing
advisory, but no active crate in this workspace compiles it.

Risk: activating a future MySQL path could make the advisory relevant.

Fix: keep it on the dependency watchlist and verify `cargo tree -i rsa` stays
empty for shipped targets.

### Object-envelope signatures are not the real authenticity mechanism

Location: `crates/client/src/engine.rs:1807`,
`crates/server/src/routes/objects.rs:1116`,
`crates/core/src/crypto.rs:191`

Clients verify object-envelope Ed25519 signatures against server-supplied
device keys without local device-key pinning. The AEAD AAD, keyed by the user's
export-derived key, is what actually prevents malicious server forgery.

Risk: the signature layer gives a stronger impression of device authentication
than it currently provides.

Fix: either pin device keys locally with TOFU and compare on every verify, or
document that AEAD AAD is the sole authenticity mechanism and remove the
non-load-bearing signature.

### Malicious server can drop, omit, or replay valid objects

Location: `crates/client/src/local_store.rs:509`

The client purges local present items omitted from a server snapshot. A server
can also replay old AAD-valid objects to resurface deleted entries.

Impact: confidentiality holds for object contents, but availability and history
integrity are not guaranteed against malicious storage.

Fix: document the storage trust model, or add client-signed monotonic state if
history integrity is in scope.

### Sessions and devices lack admin revocation

Location: `crates/server/src/routes/auth.rs:466`

Users can self-logout, but there is no admin or user-visible revocation flow for
all sessions or individual devices.

Impact: a stolen 30-day bearer token cannot be invalidated without deleting the
user or changing database state manually.

Fix: add device/session listing and revocation endpoints.

### WebSocket lacks Origin check

Location: `crates/server/src/ws.rs`

WebSocket connect does not check `Origin`. Inbound messages are now capped at
64 KiB, but there is no Origin allowlist for browser clients.

Impact: mitigated by non-cookie bearer tickets, which a cross-origin page
cannot obtain.

Fix: consider an Origin allowlist for browser clients.

### HTTP transport has no certificate pinning

Location: `crates/client/src/api_client.rs`

TLS certificate verification is enabled and redirects are now disabled on
native builds (browser builds follow the browser's fetch policy), but there is
no SPKI/certificate pinning.

Impact: this is hardening, not a current token leak.

Fix: consider optional SPKI pinning.
