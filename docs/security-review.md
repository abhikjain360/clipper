# Security Review

The current set of **known, unresolved** security issues in clipper: items that
need a product/design decision, and accepted/residual risks. Resolved issues are
not tracked here — git history records them.

Most recent full audit: **2026-06-14**, workspace-wide (docs/code reconciliation,
a 26-finder vulnerability + bug hunt, and adversarial per-finding verification).
That pass fixed roughly two dozen issues; what remains is below. No
anonymous/remote critical was found — every item here requires a local position,
an authenticated account, or the already-assumed partially-untrusted server.

## Needs a decision

Ordered by severity. Each notes *why* it needs a call (the tradeoff or policy
choice) and a recommendation.

### Medium

1. **Mobile UniFFI bridge blocks the JS thread → ANR.**
   `crates/mobile-uniffi/src/lib.rs` (+ `packages/mobile-bridge/src/adapter.ts`) ·
   Every networked method runs `block_on` inline on the React Native JS thread, and
   there is no overall HTTP deadline, so a slow/hostile server (within the trust
   model) — or even normal login latency — freezes the UI. The `adapter.ts`
   `async () =>` wrappers and the 250 ms `waitForStateChange` busy-poll do *not*
   yield the JS thread. *Decision (primary):* making the FFI non-blocking (async
   UniFFI mapped to JS Promises, or off-thread dispatch) changes the bridge
   contract — and intersects the deliberate long-poll design. *Decision
   (secondary):* a reqwest overall `.timeout()` is close to mechanical but the
   bound trades off slow-link large-file transfers, so it needs a chosen value
   (ideally per-request: short for auth/metadata, generous/none for file
   transfers). *Recommend:* add per-request timeouts now; schedule the async-FFI
   redesign deliberately.

### Low

2. **Native client has no cap on stored object records.**
   `crates/client/src/local_store.rs` · A malicious server can stream unlimited
   `Created` events; each writes a marker file, filling disk/inodes (bounded to one
   connection; reconnect reaps old-generation markers). *Decision:* a cap +
   eviction policy (evict-oldest-and-delete vs reject-new) and/or per-connection
   acceptance rate limit — interacts with the generation/reconciliation model. Note:
   the existing wasm `OBJECT_INDEX_LIMIT` only truncates the in-memory index and
   leaks the evicted item's stored payload; fix that in the same change.

3. **Linux Tauri "Add Current Clipboard" fails open + reads a different backend
   than it probes.** `web/src-tauri/src/lib.rs:262-271` · On GNOME/non-wlroots
   Wayland the wlr privacy-marker probe returns `MissingProtocol` → treated as "no
   marker", and the text is then read via arboard's X11/XWayland fallback and
   synced. *Decision:* fail-closed-when-indeterminate makes the feature never work
   on GNOME Wayland (a real UX regression); the correct fix reads the marker and the
   payload through the *same* backend (arboard, matching the macOS path). *Recommend:*
   same-backend read; interim fail-closed + a clear notice.

4. **Mobile app is hard-pinned to `http://127.0.0.1:8787`.**
   `crates/mobile-uniffi/src/lib.rs` · As shipped this is loopback-only (no wire
   exposure) but the editable "Server URL" field rejects every non-default value,
   and a developer pointing it at a real host would send the 30-day bearer token in
   cleartext (folds into the accepted no-TLS finding). *Decision:* the deployment
   model — loopback-only vs configurable remote, http vs enforced https, URL
   persistence. *Recommend:* plumb a persisted user URL, require https for
   non-loopback, and fix/disable the misleading field.

### Info

5. **Web CSP `connect-src` is broad** (`https:` / `wss:`). `web/index.html` · No
   `*`/`data:`/`blob:`/plaintext-`http:` wildcard is present, but allowing any
   https/wss host is inherent to the user-configurable-server model. *Decision:*
   accept-and-document vs. a per-deployment / build-time allowlist of the
   configured backend origin.

6. **Dead config knob `max_file_meta_ciphertext_bytes`.** `config.rs` /
    `routes/objects.rs` · Parsed, validated, documented, and plumbed through
    overrides, but never enforced (`max_object_meta_ciphertext_bytes` already
    bounds File metadata). An operator changing it gets no effect. *Decision:*
    remove the knob, or enforce a distinct File-metadata cap. *Recommend:* remove
    it unless a separate file-meta ceiling is wanted.

7. **Residual username-existence oracle at `register_finish`.** The invite-burn
    is fixed, but a holder of a valid *unused* invite can still distinguish
    existing usernames (200 at start, 409 at finish) — now without cost to the
    invite. *Decision:* accept (probing needs a valid invite) vs. return a
    non-distinguishable finish result.

8. **Clipboard privacy markers are desktop-only.** Browser (`navigator.clipboard`)
    and Expo (`getStringAsync`) do not expose the macOS/KDE sensitivity markers, so
    web/mobile manual "Add Current Clipboard" cannot honor them. *Decision:*
    document the desktop-only scope and/or show a one-time notice on web/mobile.

9. **Path-based file IPC is a confused-deputy** (same-user, post-HMAC). Accepted
    by design and documented in `local-ipc-security.md`; the unsandboxed daemon
    will read/write any caller-named path. *Decision:* keep accepted, or migrate to
    byte/chunk-oriented IPC (the engine already exposes `upload_file_bytes` /
    `download_file_bytes`) so filesystem access stays in the sandboxed UI.

10. **`opaque-ke` is a pre-release dependency** (`4.1.0-pre.2`). No advisory, exact-
    pinned, usage verified correct. *Decision:* accept-and-track vs. block release
    until a stable/audited version; re-pin and re-run `nix run .#audit` before
    deployment.

## Accepted / residual risks

Known and acknowledged; not currently being fixed.

- **Object-envelope Ed25519 signatures are server-checked provenance, not E2E
  authenticity.** The export-key-derived AEAD AAD is the real mechanism; a
  malicious server can swap the device public key it returns and re-sign. (See
  `object-envelopes.md`.)
- **A malicious server can drop / omit / replay valid objects** — availability and
  history integrity, not confidentiality.
- **No admin/user revocation flow** for sessions or individual devices (a stolen
  30-day bearer token cannot be invalidated without DB surgery). Per-user device
  growth is now bounded (`limits.max_user_devices`, enforced in `issue_session`),
  and the schema is reclamation-ready (`objects.source_device_id` is nullable
  with `ON DELETE SET NULL`, so deleting a device detaches its objects rather
  than blocking), but the user-facing list/revoke endpoints are still unbuilt —
  see `docs/revocation.md`.
- **WebSocket has no `Origin` check** (mitigated by non-cookie bearer tickets a
  cross-origin page cannot obtain).
- **No global aggregate cap on live WebSocket connections.** Per-user concurrent
  connections are bounded (`limits.max_user_ws_connections`, default 32, enforced
  via a slot guard in `handle_socket`), and dead/idle connections are reaped by a
  server Ping every 30s with a 75s idle close that also re-checks
  `session.expires_at` so a long-lived stream cannot outlive its credential.
  Total live connections are therefore bounded only transitively (registered
  users × per-user cap), with no server-wide ceiling. The ping/idle/expiry
  cadences are fixed constants in `ws.rs`, not config. *Fix if it matters:* a
  global Semaphore in `handle_socket`.
- **No SPKI/certificate pinning** on the HTTP transport (TLS verification is on;
  native builds disable redirects).
- **macOS keychain IPC secret uses the default ACL** — another same-user process
  can read it once the login keychain is unlocked.
- **Login challenge has a residual username-existence *timing* oracle** — the
  real-user path does an extra AEAD unwrap + larger row fetch that the fake-record
  path skips (response *shape* is equalized, timing is not).
- **Client-supplied `created_at` controls the user's own item TTL** — now
  UTC-normalized and overflow-safe, but still client-chosen.
- **Passphrase bytes can have residual non-zeroizing copies.** Rust-owned IPC,
  Tauri, wasm, mobile, and CLI boundaries move passphrases/invite keys into
  `Zeroizing`, but the lower-level client/OPAQUE APIs still take `&str` and
  browser/foreign-language runtimes keep their own copies outside Rust's control,
  so bytes can remain in freed heap or swap. *Fix if it matters:* a secret-wrapper
  type for the auth APIs.
- **Per-username auth limiter allows targeted account lockout.** The per-username
  challenge budget caps distributed guessing, but someone who knows a username can
  deliberately exhaust it from many addresses to keep that one account
  rate-limited (blast radius: one named account). *Fix if it matters:* count only
  failed finalizations, or key by `(client, username)`.
- **User data scoping relies on manual discipline.** Server private-data handlers
  use raw SeaORM entity calls with hand-written `user_id` filters rather than a
  `UserScope`/`UserDb` helper, so a future handler could omit the filter without a
  helper or CI guard catching it. The audit confirmed no current IDOR; see
  `user-data-scoping.md`.
- **Linux desktop pulls in unmaintained GTK/glib 0.18 deps** (no fixed line at the
  pinned Tauri version).
- **Dead `rsa` dep in the lockfile** (sqlx MySQL metadata; not compiled for shipped
  targets — keep `cargo tree -i rsa` empty).
