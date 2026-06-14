# Security Review

The current set of **known, unresolved** security issues in clipper: items that
need a product/design decision, and accepted/residual risks. Resolved issues are
not tracked here — git history records them.

Most recent full audit: **2026-06-14**, a frontend-weighted follow-up pass
(28-finder fan-out — 10 finders on the newer web/Tauri/React-Native/wasm/UniFFI
surface — with dedup and adversarial per-finding verification). It confirmed 33
issues and fixed the ~16 that needed no product decision (server WebSocket send
timeouts so a read-stalled peer can no longer wedge the idle/expiry backstops;
event-seq seeding from `objects.created_seq`; atomic login device+session;
unconditional local logout/remove-device teardown; `NotAuthenticated` instead of
an empty bearer; corrected wasm localStorage eviction; orphaned-sidecar reclaim;
the macOS clipboard-watcher start/stop race; a redacting `Debug` for daemon-IPC
secrets; the macOS daemon socket-dir fallback; and several web/mobile robustness
clamps). What remains is below. No anonymous/remote critical or high was found —
every item here requires a local position, an authenticated account, or the
already-assumed partially-untrusted server.

## Needs a decision

Ordered by severity. Each notes _why_ it needs a call (the tradeoff or policy
choice) and a recommendation.

### Medium

1. **HTTP server has no header-read timeout, whole-request timeout, or
   connection cap (unauthenticated slow-loris).** `crates/server/src/main.rs`
   (bare `axum::serve`) · Hyper applies no header-read or whole-request deadline
   by default and Tokio caps no accepted connections, so simultaneously-open
   slow-progress connections (plus their tasks/FDs) are unbounded. Critically,
   all three rate limiters run as middleware _after_ hyper parses headers, so a
   peer trickling headers one byte at a time is never counted against any bucket
   — a classic slow-loris reachable fully unauthenticated against `/api/health`
   and the public auth routes. The streaming upload path (`routes/objects.rs`)
   also has no per-chunk read timeout. Distinct from the documented body-
   _buffering_ gap (bounded once headers complete) and the native-client whole-
   request-deadline item below. _Decision:_ enforce server-side vs. delegate to
   the documented reverse-proxy deployment (which usually applies its own header/
   idle timeouts); `tower::limit` is not currently a dependency. _Recommend:_ a
   hyper `http1().header_read_timeout(...)`, a connection/concurrency bound
   (`GlobalConcurrencyLimitLayer` or an accept-loop `Semaphore`), and either a
   short whole-request `TimeoutLayer` for auth/health (excluding upload) or a
   per-chunk timeout on the upload stream. Even if delegated to a proxy, document
   it as an operational requirement in `server-resource-limits.md`.

### Low

2. **Native client has no cap on stored object records.**
   `crates/client/src/local_store.rs` · A malicious server can stream unlimited
   `Created` events; each writes a marker file, filling disk/inodes (bounded to one
   connection; reconnect reaps old-generation markers). _Decision:_ a cap +
   eviction policy (evict-oldest-and-delete vs reject-new) and/or per-connection
   acceptance rate limit — interacts with the generation/reconciliation model.
   (The sibling wasm `OBJECT_INDEX_LIMIT` eviction bug — it kept the oldest and
   leaked the evicted record+payload — was fixed in this pass; the native cap is
   the remaining open decision.)

3. **Mobile app is hard-pinned to `http://127.0.0.1:8787`.**
   `crates/mobile-uniffi/src/lib.rs` · As shipped this is loopback-only (no wire
   exposure) but the editable "Server URL" field rejects every non-default value,
   and a developer pointing it at a real host would send the 30-day bearer token in
   cleartext (folds into the accepted no-TLS finding). _Decision:_ the deployment
   model — loopback-only vs configurable remote, http vs enforced https, URL
   persistence. _Recommend:_ plumb a persisted user URL, require https for
   non-loopback, and fix/disable the misleading field.

4. **OPAQUE passphrase KSF (Argon2id) is pinned to the library-default OWASP
   floor and is not configurable.** `crates/core/src/crypto.rs` · Register/login
   finish pass `*FinishParameters::default()`, whose `ksf` is `None`, so
   opaque-ke falls back to `Argon2::default()` (m=19 MiB, t=2). This is the only
   stretching protecting `users.opaque_password_file` and, transitively, the
   export key. The configurable `Argon2Params` applies only to server-side
   access-key hashing, never this path, so an operator cannot raise passphrase-
   stretching cost; a partially-untrusted server holding the password file mounts
   an offline dictionary attack at this fixed per-guess cost. Parameters are at
   (not below) the OWASP floor — defense-in-depth, not a break. _Decision:_ fixed
   hardened constant vs. a build/deploy knob (register and login must use
   identical params, and cost trades against mobile/wasm login latency).
   _Recommend:_ pass an explicit hardened `ksf` (e.g. ≥64 MiB / t≥3) in both
   finish calls; at minimum document the actual parameters in `docs/opaque.md`.

5. **Standalone browser client ships CSP only via `<meta>`, so `frame-ancestors`
   is ignored (no clickjacking protection).** `web/index.html`,
   `web/vite.config.ts` · Browsers honor `frame-ancestors` only as an HTTP
   response header; the declared `frame-ancestors 'none'` is silently dropped,
   and the static host sends no CSP header and no `X-Frame-Options`, so the web
   deployment is embeddable cross-origin (UI-redress against Logout/Delete/Remove-
   device/Add-Clipboard). Does not break E2E confidentiality; the Tauri desktop
   build is fine (CSP injected as a real header). _Decision:_ production fix is a
   serving-layer choice (which host serves `web/dist` and how it emits headers);
   there is an in-repo lever for dev/preview parity (a `headers` map in
   `vite.config.ts`). _Recommend:_ serve `web/dist` with a real HTTP CSP +
   `X-Frame-Options: DENY`; document the host header requirement.

6. **Auth form fields lack autofill/credential-persistence suppression; web
   renders the one-time access key as a plaintext field.** `web/src/App.tsx`,
   `mobile/src/App.tsx` · Distinct from the in-memory residual-copy risk below:
   passphrase/access-key inputs can be learned by the OS/browser credential and
   keyboard stores. _Decision:_ a UX call (autofill suppression, masking the
   access-key field) on the auth screens both platforms render. _Recommend:_ set
   `autoComplete`/`autocapitalize`/keyboard-learning-off attributes on the secret
   inputs and consider masking the access key.

7. **`init_object` Postcard body is bounded by axum's implicit 2 MiB
   `DefaultBodyLimit`, decoupled from the configured object-size limits.**
   `crates/server/src/main.rs`, `routes/objects.rs` · The 64 KiB `DefaultBodyLimit`
   is a route-layer on the public-auth router only; the authed router inherits the
   implicit 2 MiB cap, and the Postcard extractor buffers the whole body before
   the handler's explicit gates. So an inline payload >~2 MiB is rejected with a
   generic 400 rather than `PayloadTooLarge`, and raising
   `max_object_meta_ciphertext_bytes` above 2 MiB silently fails. Authenticated,
   hard-capped, no DoS. (Distinct from the dead `max_file_meta_ciphertext_bytes`
   knob below.) _Decision:_ are multi-MiB inline payloads a supported shape
   (then size an explicit authed-router limit + config cross-check) or must they
   stream (then add a small explicit inline cap)? _Recommend:_ decide the
   inline-vs-streamed contract and bound the inline path explicitly so clients get
   a precise `PayloadTooLarge`.

8. **Web `upload_file_bytes` has no client-side size cap.**
   `crates/client/src/engine.rs`, `web/src/App.tsx` · Unlike
   `send_clipboard_payload` (authoritative client ceiling), file upload encrypts
   arbitrary data immediately and relies on the server cap; on web the file is
   copied ~3× (read → `to_vec` → encrypt) and fully buffered before
   `max_file_blob_bytes` applies. Self-inflicted (own tab OOM). _Decision:_ the
   ceiling value must not reject large legitimate uploads (native deliberately
   avoids some ceilings for slow links). _Recommend:_ a client-side
   `MAX_FILE_PLAINTEXT_BYTES` checked before encrypt (benefits web and native),
   and check `File.size` before `arrayBuffer()` on web.

9. **Per-user API limiter charges one token per request regardless of payload
   size.** `crates/server/src/rate_limit.rs`, `routes/objects.rs` · The PUT/GET
   payload routes (blobs up to `max_file_blob_bytes`) are bounded only by request
   counts, so a 512 MiB transfer and a 1-byte request cost the same. Net stored
   bytes are capped by `max_user_storage_bytes`, but download read-amplification
   and upload+delete churn are not cost-weighted (requires a valid token; no
   cross-user reach). _Decision:_ byte-volume throttling vs. delegating egress to
   a front proxy. _Recommend:_ either a cost-weighted limiter on the payload
   routes, or document the read-amplification/churn vectors as intentionally out
   of scope.

10. **Reconciliation pagination loop has no page cap and does not require the
    server cursor to advance.** `crates/client/src/engine.rs` ·
    `snapshot_files`/`snapshot_clipboard` exit only when the server returns
    `next_after == None`; a malicious server can return a constant non-advancing
    cursor (or keep emitting decrypt-fail-skipped items) to spin a detached
    reconciliation task forever, and reconnects stack additional loops.
    Availability against a malicious server is out of scope. _Decision:_ the
    strict cursor-monotonicity check is clearly correct and cheap; the page/item
    hard-cap and abort-prior-loops-on-reconnect are availability-only.
    _Recommend:_ at least enforce strict `(created_seq, id)` cursor monotonicity;
    optionally cap pages and supersede prior loops by generation.

11. **Web/mobile display state carries full decrypted clipboard payloads (up to
    16 MiB × 100) and re-serializes the whole `AppState` across the wasm boundary
    on every change.** `crates/app-types/src/lib.rs`, `crates/web-wasm/src/lib.rs`,
    `web/src/App.tsx` · `DecryptedClipboardItem.text` holds the full payload; the
    web frontend re-fetches and re-encodes all previews on every event though the
    list renders only a 3-line preview. Self-/own-data only (AEAD-gated).
    _Decision:_ changing `text` to a bounded preview alters the display contract
    (UI must fetch full payloads on demand for copy/expand). _Recommend:_ a
    char-boundary-safe preview in display state + on-demand full payload via the
    existing `clipboard_payload(id)`; optionally a delta/version-keyed `getState`.

### Info

12. **Web CSP `connect-src` is broad** (`https:` / `wss:`). `web/index.html` · No
    `*`/`data:`/`blob:`/plaintext-`http:` wildcard is present, but allowing any
    https/wss host is inherent to the user-configurable-server model. _Decision:_
    accept-and-document vs. a per-deployment / build-time allowlist of the
    configured backend origin.

13. **Dead config knob `max_file_meta_ciphertext_bytes`.** `config.rs` /
    `routes/objects.rs` · Parsed, validated, documented, and plumbed through
    overrides, but never enforced (`max_object_meta_ciphertext_bytes` already
    bounds File metadata). An operator changing it gets no effect. _Decision:_
    remove the knob, or enforce a distinct File-metadata cap. _Recommend:_ remove
    it unless a separate file-meta ceiling is wanted.

14. **Residual username-existence oracle at `register_finish`.** The invite-burn
    is fixed, but a holder of a valid _unused_ invite can still distinguish
    existing usernames (200 at start, 409 at finish) — now without cost to the
    invite. _Decision:_ accept (probing needs a valid invite) vs. return a
    non-distinguishable finish result.

15. **Clipboard privacy markers are incompletely honored.** _Browser/mobile:_
    `navigator.clipboard` / Expo `getStringAsync` do not expose the macOS/KDE
    sensitivity markers, so web/mobile manual "Add Current Clipboard" cannot honor
    them. _Linux desktop:_ `crates/client/src/clipboard_watcher_linux.rs` probes
    the marker in a separate Wayland session from the content read (no macOS-style
    post-read re-check) and fails _open_ if the `x-kde-passwordManagerHint` read
    stalls or exceeds its cap. Under the threat model the clipboard owner is a
    trusted same-user process, so the realistic leakage window is negligible (a
    malicious same-user process can read the clipboard outright). Full macOS parity
    is impossible on Wayland (no selection-generation primitive). _Decision:_
    document the desktop-only scope and the deliberate Linux fail-open, and/or add
    a post-read marker re-probe + fail-closed on a stalled hint, and/or a one-time
    notice on web/mobile.

16. **Path-based file IPC is a confused-deputy** (same-user, post-HMAC). Accepted
    by design and documented in `local-ipc-security.md`; the unsandboxed daemon
    will read/write any caller-named path. _Decision:_ keep accepted, or migrate to
    byte/chunk-oriented IPC (the engine already exposes `upload_file_bytes` /
    `download_file_bytes`) so filesystem access stays in the sandboxed UI.

17. **Tauri `core:default` capability grants unused JS-reachable core commands**
    (`core:image` from-path/rgba, `core:webview` internal-toggle-devtools).
    `web/src-tauri/capabilities/default.json` · The app never uses them; the
    production CSP is strict and there is no HTML-injection sink, so they are
    unreachable in practice (no marginal E2E/IDOR/cursor/remote break even with
    arbitrary JS). _Decision:_ tighten to an explicit least-privilege allowlist
    (verify the app still functions, since it departs from the framework default)
    vs. accept. Safe to defer.

18. **Daemon Unix socket has no per-connection authentication timeout or
    accepted-connection cap.** `crates/daemon/src/main.rs` · A same-user local
    process can open connections that never complete the HMAC handshake, or many
    at once. Same-user local DoS only (the daemon is same-user-trusted by design).
    _Decision:_ add a handshake timeout + accept cap, or accept as same-user-local.

19. **`opaque-ke` is a pre-release dependency** (`4.1.0-pre.2`). No advisory, exact-
    pinned, usage verified correct. _Decision:_ accept-and-track vs. block release
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
- **Per-session and admin/cross-user revocation are unbuilt.** Device list/remove
  _is_ shipped (`GET /api/auth/devices`, `DELETE /api/auth/devices/{id}`, user-
  scoped, with `sessions.device_id ON DELETE CASCADE` revoking that device's
  tokens), so a leaked 30-day bearer token can be revoked by removing its device
  without DB surgery. What remains: no per-session list/revoke (revoke-others) and
  no admin/cross-user revocation — see `docs/revocation.md`. Per-user device
  growth is bounded (`limits.max_user_devices`, enforced in `issue_session`, now
  inside the login transaction so a failed session insert cannot orphan a device).
- **WebSocket has no `Origin` check** (mitigated by non-cookie bearer tickets a
  cross-origin page cannot obtain).
- **No global aggregate cap on live WebSocket connections.** Per-user concurrent
  connections are bounded (`limits.max_user_ws_connections`, default 32) and
  dead/idle connections are reaped by a server Ping every 30s with a 75s idle
  close that also re-checks `session.expires_at`. Every server-side send is now
  bounded by a write timeout, so a peer that stops reading can no longer wedge the
  send loop and defeat those backstops. Total live connections remain bounded only
  transitively (registered users × per-user cap), with no server-wide ceiling. The
  ping/idle/expiry/send cadences are fixed constants in `ws.rs`, not config. _Fix
  if it matters:_ a global Semaphore in `handle_socket`.
- **Global auth bucket can be drained by a distributed low-rate flood.**
  `rate_limit.rs` · `check_auth` requires the per-client bucket _and_ a single
  process-wide `auth_global` bucket (default 3000/min). A flood spread across
  many under-limit IPs can drain the shared bucket and temporarily lock out all
  authentication. Intentional: `auth_global` is a CPU-capacity ceiling protecting
  OPAQUE/Argon2, and availability is out of scope (same class as the per-username
  lockout below). _Fix if it matters:_ prefer work-admission control (the Argon2
  concurrency semaphore) over a global request-count bucket.
- **No SPKI/certificate pinning** on the HTTP transport (TLS verification is on;
  native builds disable redirects).
- **No whole-request HTTP deadline on the client.**
  `crates/client/src/api_client.rs` (native) sets a 10s `connect_timeout` and a 30s
  per-read-chunk `read_timeout` (deliberately chosen over a whole-request
  `.timeout()` so large downloads stay viable on slow links), but no overall
  deadline; a server that trickles one byte just inside each read window can keep
  an auth/metadata request pending indefinitely. The wasm/browser client is built
  with a bare `Client::new()` and has neither the connect/read-chunk bounds nor a
  whole-request deadline (reqwest's builder timeouts do not apply on wasm), so the
  same stall applies there with even less bounding. Self-confined (the response-
  size cap still bounds memory; the mobile UniFFI boundary is async and
  `AbortSignal`-cancellable, so this stalls the in-flight operation rather than
  freezing the UI). _Fix if it matters:_ a short per-request `.timeout()` for
  auth/metadata (native), and a generous per-request `.timeout()` on wasm.
- **macOS keychain IPC secret uses the default ACL** — another same-user process
  can read it once the login keychain is unlocked.
- **`CLIPPER_SERVER_SECRET_FILE` permissions/ownership are not checked.**
  `crates/server/src/secret.rs` · The pepper file is read with no mode/owner
  inspection, despite `docs/server-secret.md` advising mode 0600. A world/group-
  readable file silently weakens at-rest protection (needs an operator mistake
  _plus_ a separate local user; same class as the keychain default-ACL). _Fix if
  it matters:_ a Unix _warn-only_ check (do not refuse to start, which would break
  systemd `LoadCredential`/KMS/read-only secret mounts).
- **Login challenge has a residual username-existence _timing_ oracle** — the
  real-user path does an extra AEAD unwrap + larger row fetch that the fake-record
  path skips (response _shape_ is equalized, timing is not).
- **Client-supplied `created_at` controls the user's own item TTL** — now
  UTC-normalized and overflow-safe, but still client-chosen.
- **Passphrase bytes can have residual non-zeroizing copies.** Rust-owned IPC,
  Tauri, wasm, mobile, and CLI boundaries move passphrases/invite keys into
  `Zeroizing`, but the lower-level client/OPAQUE APIs still take `&str` and
  browser/foreign-language runtimes keep their own copies outside Rust's control,
  so bytes can remain in freed heap or swap. _Fix if it matters:_ a secret-wrapper
  type for the auth APIs.
- **Per-username auth limiter allows targeted account lockout.** The per-username
  challenge budget caps distributed guessing, but someone who knows a username can
  deliberately exhaust it from many addresses to keep that one account
  rate-limited (blast radius: one named account). _Fix if it matters:_ count only
  failed finalizations, or key by `(client, username)`.
- **User data scoping relies on manual discipline.** Server private-data handlers
  use raw SeaORM entity calls with hand-written `user_id` filters rather than a
  `UserScope`/`UserDb` helper, so a future handler could omit the filter without a
  helper or CI guard catching it. The audit confirmed no current IDOR (mutations
  by PK are all preceded by a user-scoped ownership check on the immutable
  `objects.user_id`); see `user-data-scoping.md`.
- **Linux desktop pulls in unmaintained GTK/glib 0.18 deps** (no fixed line at the
  pinned Tauri version).
- **Dead `rsa` dep in the lockfile** (sqlx MySQL metadata; not compiled for shipped
  targets — keep `cargo tree -i rsa` empty).
