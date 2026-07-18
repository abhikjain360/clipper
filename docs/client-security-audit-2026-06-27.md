# Client-Side Security Audit — 2026-06-27

Scope: **client side only**, as requested — the web React app (`web/src`), the
Tauri desktop shell (`web/src-tauri`), the Android React Native app
(`mobile/src`, `mobile/android`), the shared Rust client core (`crates/client`,
`crates/core`, `crates/app-types`), the browser wasm binding
(`crates/web-wasm`), the mobile UniFFI bridge (`crates/mobile-uniffi`,
`packages/mobile-bridge`), and the local daemon (`crates/daemon`). Server
handler bugs were explicitly out of scope (some server files are cited only
where they bound a client-side finding).

Two priority themes, per the request: **(A) web XSS / browser-side compromise**
and **(B) a separate malicious app or local process reading Clipper's data**
(on Android each app is a _different uid_ in a sandbox, so the desktop
"same-user is trusted" assumption does **not** apply there).

This pass is independent of, and complements, `docs/security-review.md` (whose
last full audit was server/crypto-weighted and thin on client XSS). Findings
below are **new** unless tagged otherwise.

> **Status: documentation only.** No code was changed. Recommendations are
> prose; nothing here has been applied.

## Method

Multi-agent fan-out (Opus 4.8 at `xhigh`/`max` effort): ~14 focused finders
reading real code and tracing data flows, each finding then re-checked by an
independent adversarial verifier that re-read the code and defaulted to
_refuted_ (web-XSS claims had to reach a real executing sink past React/Tamagui
escaping and the CSP; local claims had to be reachable by a _separate_ uid /
user / sandbox, not an already-trusted same-user process). The DOM-sink and
secret-storage web finders initially failed on oversized structured output and
were re-run free-form; the second pass also uncovered the collaborative-docs
surface, which got its own finders. Across both runs: 54 candidate findings →
**21 confirmed** (after dedup), 7 refuted, and 25 verified-safe negatives.

## Headline

The DOM-injection XSS surface itself is, perhaps surprisingly, **clean** —
every decrypted clipboard/file/collab string renders through React/Tamagui text
nodes (escaped), CodeMirror is highlight-only, and there is no
`dangerouslySetInnerHTML`/`innerHTML` anywhere (see _Verified safe_ below). The
real web exposure is not an injection sink but the **blast radius if script ever
does run** on the origin:

- the **passphrase — the E2E root secret — is persisted in cleartext in
  `sessionStorage`** (HIGH), and
- the **wasm backend is a confused deputy**: injected script can simply _ask_ it
  to decrypt and exfiltrate everything, without ever reading the in-memory keys
  (Medium),

so any same-origin script execution — via a tampered bundle (no SRI, CSP only
via `<meta>`) or any future XSS — is **total, revocation-proof account
compromise**. The newly-found **collaborative-docs feature is not E2E** (the
server holds plaintext and can read/modify/inject it) and carries a small family
of Low issues. On **Android**, the worst is that **decrypted content is exposed
to screen-capture / recents** because no window sets `FLAG_SECURE` (Medium),
alongside several Low hardening gaps (overlay permission, exported activity,
clipboard marker, plaintext cache files).

No anonymous-remote critical was found on the client; every item requires script
execution on the origin, an installed malicious app, a share-token holder, or a
local position.

## Summary table

| #   | Sev      | Platform      | Title                                                                                                   | New?               |
| --- | -------- | ------------- | ------------------------------------------------------------------------------------------------------- | ------------------ |
| A1  | **High** | web           | Passphrase (E2E root secret) persisted cleartext in `sessionStorage`                                    | new                |
| A2  | Medium   | web           | Web bundle has no SRI and CSP only via `<meta>` → bundle host can inject script                         | new (rel. item 5)  |
| A3  | Medium   | web           | wasm backend is a confused deputy: XSS decrypts/exfiltrates all data                                    | new                |
| A4  | Medium   | web           | Collab awareness `color` → unsanitized inline-CSS injection (UI-redress)                                | new                |
| C1  | Medium   | android       | No `FLAG_SECURE`: decrypted content leaks to screenshots / recents / screen-capture                     | new                |
| A5  | Low      | web/server    | Collaborative-doc content is **not E2E** — server holds plaintext, can read/modify/inject               | new                |
| A6  | Low      | web/shared    | Collab share token = unauthenticated read+write bearer (URL query, no expiry/revoke, plaintext at rest) | new                |
| A7  | Low      | web           | Collab WebSocket + share page are fully anonymous (token-bearer only)                                   | new                |
| A8  | Low      | web           | Owner username broadcast in cleartext awareness to anonymous guests + server                            | new                |
| B1  | Low      | desktop-tauri | `download/upload_file_bytes` route decrypted content through world-readable temp files                  | new                |
| C2  | Low      | android       | Downloaded files written as plaintext to the app cache dir                                              | new                |
| C3  | Low      | android       | Copy writes decrypted secrets to the global clipboard with no `IS_SENSITIVE` marker                     | new (rel. item 15) |
| C4  | Low      | android       | Exported `singleTask` activity + unused browsable `clipper://` scheme                                   | new                |
| C5  | Low      | android       | `SYSTEM_ALERT_WINDOW` declared but unused; no obscured-touch protection                                 | new                |
| —   | Info     | desktop-tauri | (positive) No token/key/IPC-secret is exposed to the Tauri webview                                      | new                |

Known and reconfirmed (already in `security-review.md`): CSP delivered only via
`<meta>` drops `frame-ancestors`, leaving the web deployment clickjackable
(item 5); web/mobile cannot honor clipboard sensitivity markers (item 15).

---

## A. Web & browser

### A1 — [High] Passphrase (E2E root secret) persisted cleartext in `sessionStorage`

> **Status (2026-06-30): Resolved.** The browser client no longer persists the
> passphrase. Session resume now stores only a server-revocable bearer token and
> the two OPAQUE-derived child keys (data key + device-identity wrapping key) in
> `sessionStorage` (`clipper.session.v2`), and re-mounts the session via
> `SyncEngine::resume_with_platform` + `GET /api/auth/validate` with no OPAQUE
> login. The stored material is no longer the revocation-proof root: it cannot
> re-derive the passphrase, re-run login, or enroll a device, and revoking the
> token (logout / device removal) bounds exposure. The residual XSS-readability
> of the data key (the wasm AEAD needs raw key bytes) and the `sessionStorage`
> store itself are documented in `docs/local-at-rest-encryption.md`.

- **Files:** `web/src/backend/index.ts:67,80-87` · `web/src/App.tsx:226-227`
- **What:** The browser "resume across reload" feature serializes the login
  credentials — **including the literal passphrase** — to `sessionStorage`:
  `sessionStorage.setItem("clipper.session.v1", JSON.stringify(creds))` where
  `creds.passphrase` is the user's passphrase (`App.tsx:227`
  `saveSessionCredentials({ passphrase, ... })`). The code comment itself notes
  "sessionStorage is plaintext and same-origin-script readable."
- **Why it matters:** The passphrase is the OPAQUE input from which **both** the
  data key (decrypts all clipboard/file content) **and** the device-identity
  wrapping key derive. Any same-origin script that reads `clipper.session.v1`
  obtains **permanent, revocation-proof** E2E compromise: removing the device or
  rotating the bearer token does not help, because the attacker can re-login and
  re-derive every key from the passphrase. This is strictly worse than stealing
  the bearer token.
- **Reachability:** No live script-XSS sink exists today (the DOM surface is
  clean — see _Verified safe_), so this is currently a **latent amplifier**. It
  becomes **Critical** the instant any script runs on the origin — via the
  integrity-unchecked bundle (A2) or any future XSS. Tauri is unaffected (the
  desktop daemon owns credentials; the store is gated to the browser runtime).
- **Recommendation:** Do not persist the passphrase. Either accept re-login on
  reload, or resume via a **short-lived, server-revocable resume ticket that is
  not the E2E root** (and is not exchangeable for the passphrase). Document the
  `sessionStorage` store in `docs/local-at-rest-encryption.md` either way.

### A2 — [Medium] No Subresource Integrity and CSP only via `<meta>`; the bundle host can inject script

- **Files:** `web/index.html:8,14` · `web/vite.config.ts` (no `headers`) ·
  serving wrappers in `flake.nix` / `scripts/`
- **What:** The standalone web bundle is loaded with no SRI, and its CSP is
  delivered only through an HTML `<meta>` tag (so header-only directives such as
  `frame-ancestors` are silently ignored — see item 5). Whoever serves
  `web/dist` can therefore substitute or augment the JS/wasm and run arbitrary
  script on the origin, which immediately reads the `sessionStorage` passphrase
  (A1) and drives the wasm backend (A3).
- **Correction to an earlier framing:** the **clipper API server is API-only and
  does not serve the bundle** — so this is _not_ "the partially-untrusted data
  server serves the app." The trusted party is whichever static host serves
  `web/dist`; the gap is the absence of integrity controls on that delivery.
- **Recommendation:** Serve `web/dist` with a **real HTTP CSP header** plus
  `X-Frame-Options: DENY`, and add SRI or a signed/pinned bundle. At minimum
  document the host's header requirements as an operational contract (ties to
  item 5).

### A3 — [Medium] wasm backend is a confused deputy: XSS decrypts and exfiltrates everything

- **Files:** `crates/web-wasm/src/lib.rs:187-271` (`getState`,
  `clipboardPayload`, `downloadFileBytes`) · `crates/client/src/engine.rs:67`
  (data key, memory-only) · `web/src/backend/wasm.ts`
- **What:** The data key and bearer token live **only** in wasm linear memory and
  are not JS-readable — a sound design _against direct theft_. But the module
  singleton exports decrypt-on-demand methods, and `getState` already renders
  decrypted clipboard text into the DOM. Injected same-origin script does not
  need the keys: it calls the backend and exfiltrates plaintext directly.
- **Reachability:** A blast-radius consequence, contingent on script execution
  (A1/A2 or a future XSS) rather than an independent injection vector — hence
  Medium, not High.
- **Recommendation:** Treat the wasm surface as fully attacker-reachable _under_
  XSS and prioritize XSS / bundle-tamper prevention (A2). The memory-only
  key/token design is otherwise correct; do not regress it.

### A4 — [Medium] Collab awareness `color`/`colorLight` → unsanitized inline-CSS injection

- **Files:** `web/src/CodeEditor.tsx:105-109` · `y-codemirror.next`
  `src/y-remote-selections.js:88,194-195,206,236` (via `lib0/dom.js:63`
  `setAttribute('style', …)`) · server relay `crates/server/src/collab_sync.rs:448-456`
- **What:** Remote collaborators' awareness `user.color` / `user.colorLight`
  are interpolated **unvalidated** into a full inline `style` attribute on the
  remote caret/selection decorations. `CodeEditor.tsx` only sanitizes its _own_
  local awareness; the third-party plugin performs no validation, and the server
  relays awareness verbatim — so a malicious peer (any share-token holder) or a
  malicious server controls the entire inline-style block.
- **Impact ceiling:** CSS only — `setAttribute('style', …)` does not parse HTML,
  so there is no script/markup breakout, and the CSP (`script-src` without
  `unsafe-inline`; `img-src 'self' data: blob:`) blocks `url()` exfil. The
  realistic impact is **same-origin UI-redress / clickjacking / spoofing** via a
  `position:fixed` viewport overlay — no token or key theft. (Rated Low by one
  verifier on the bounded impact; Medium here to match the repo's own grading of
  UI-redress in item 5.)
- **Recommendation:** Validate `color`/`colorLight` against a strict
  `#rgb`/`#rrggbb`/`rgb()` grammar (or map remote client ids to a fixed local
  palette and ignore wire-supplied colors) at the `yCollab`/provider boundary
  before the editor reads them. Treat **all** awareness fields as hostile, since
  the channel is non-E2E (A5).

### A8 — [Low] Owner username broadcast in cleartext awareness to anonymous guests

- **Files:** `web/src/App.tsx:433` · `web/src/CodeEditor.tsx:105-108` ·
  `crates/server/src/collab_sync.rs:448-456`
- **What:** `displayName={session.username}` is published into awareness
  `user.name` and relayed verbatim to anonymous guests and the server. The
  account username (a login identifier) leaks to every share-token holder and to
  the server in cleartext. (The name renders as a text node — _not_ an injection
  sink; this is a privacy/identity leak only.)
- **Recommendation:** Let the owner choose a per-doc display name, or default to
  a non-identifying label for shared docs.

---

## Collaborative-docs surface (cross-cutting)

The editor (`web/src/CodeEditor.tsx`) binds a CodeMirror instance to a Yjs
document synced over a raw `y-websocket` `WebsocketProvider` to
`…/api/collab-docs/<objectId>/ws?token=<shareToken>`. This whole feature sits
**outside the E2E envelope** that protects clipboard/file objects.

### A5 — [Low] Collaborative-doc content is not E2E; the server holds plaintext

- **Files:** `crates/server/src/collab_sync.rs:124-261,448-456` ·
  `crates/server/src/routes/collab.rs` · `web/src/CodeEditor.tsx` ·
  `crates/client/src/local_store.rs:116-132`
- **What:** The server keeps an authoritative `yrs::Doc`, decodes/applies/
  re-encodes updates, and persists `encode_state_as_update_v1` **plaintext** to
  `collab_docs.yjs_state`; the client stores content (and token) without AEAD.
  Unlike clipboard/file objects, collab content is server-visible and
  server-modifiable, and a malicious server (or peer) can inject content that
  streams into every collaborator's editor. This is a genuine confidentiality +
  integrity delta from the E2E model — distinct from the accepted "malicious
  server can drop/replay" residual — but it is the _deliberate, code-documented_
  "one server-visible object kind," and it reaches no script sink, so Low under
  a client-XSS threat model.
- **Recommendation:** Document prominently (in `security-review.md` and the
  share UI) that collab content is **not E2E** — server-readable and
  server-modifiable. True confidentiality would require an opaque-blob CRDT under
  a shared doc key.

### A6 — [Low] Share token is an unauthenticated read+write bearer capability

- **Files:** `crates/server/src/routes/collab.rs:65,453` ·
  `web/src/CodeEditor.tsx:102` · `crates/client/src/local_store.rs:126-132,660-665,1478-1485`
- **What:** A 256-bit token grants **full read+write** on a bare string compare,
  with no expiry, no revocation, and no read-only variant. It is transmitted in
  the **WebSocket URL query string** (leakable via proxy logs, history,
  referrers) and persisted **plaintext at rest** in both the browser
  `localStorage` cache and the native `local_store` object cache. Blast radius is
  a single shared doc, and the at-rest read is same-user — hence Low.
- **Recommendation:** Move the token out of the URL (WS subprotocol or
  first-frame handshake); add expiry + rotate/revoke + a read-only viewer token;
  AEAD-wrap the collab record (incl. token) at rest.

### A7 — [Low] Collab WebSocket and share page are fully anonymous

- **Files:** `crates/server/src/main.rs:378-391` ·
  `crates/server/src/routes/collab.rs:388-393` · `web/src/App.tsx`
- **What:** `/collab-docs/{id}/ws` and `/s/{token}/meta` are on the public
  router with only the per-IP limiter (no auth middleware); the WS handler takes
  no `AuthInfo`. This is the intended anonymous-share design, characterized here
  because it underpins A5/A6 — anyone with the token, authenticated or not, is a
  full participant.
- **Recommendation:** Accept-and-document anonymous sharing, bounded by the
  expiry/revocation/read-only controls from A6.

---

## C. Android (malicious-app / device-access)

Threat actor: a _separate installed app_ (different uid) or someone with brief
physical/screen access. `allowBackup=false` is set and correctly closes the adb
backup channel.

### C1 — [Medium] No `FLAG_SECURE`: decrypted content leaks to screenshots, recents, and screen-capture

- **Files:** `mobile/android/app/src/main/AndroidManifest.xml:20` ·
  `mobile/android/app/.../MainActivity.kt:13-20` · `mobile/src/App.tsx:474`
  (`<Text>{item.text}</Text>`), `:446`/`:549` (`setViewing({content: …})`),
  `:932-947` (content viewer)
- **What:** Decrypted clipboard items and file bodies are painted to the screen,
  but no window sets `WindowManager.LayoutParams.FLAG_SECURE` (no
  `setRecentsScreenshotEnabled`, no `expo-screen-capture`). So every such surface
  is screenshot-able, is captured into the OS **recents/task-switcher
  thumbnail**, and is recorded verbatim by any **MediaProjection** screen-
  recorder / mirror / remote-support app the user installs and grants capture to
  (`FLAG_SECURE` windows render black under MediaProjection). None of this
  requires Clipper to be compromised — it crosses a uid boundary, defeating the
  E2E guarantee for an on-device adversary.
- **Recommendation:** Set `FLAG_SECURE` on the `MainActivity` window (a tiny
  native module / Expo config plugin) — one flag blanks screenshots, the recents
  thumbnail, and MediaProjection at once; on Android 13+ also
  `setRecentsScreenshotEnabled(false)`. Optionally clear `viewing`/blank panels
  on an `AppState` background transition for defense-in-depth on the thumbnail.

### C2 — [Low] Downloaded files written as plaintext to the app cache directory

- **Files:** `mobile/src/backend.ts:73-85` · `mobile/src/App.tsx:535-543`
- **What:** File downloads are written decrypted to the app cache dir. The dir is
  app-private (uid-protected, `allowBackup=false`), so it is not readable by
  other apps today — but it leaves decrypted E2E content at rest with no
  encryption and an unbounded lifetime, and is reachable on a rooted/backup/
  forensic path. Defense-in-depth, hence Low.
- **Recommendation:** Write to a path that is cleaned up promptly after use, or
  keep decrypted bytes in memory; if persisted, encrypt at rest.

### C3 — [Low] Copy writes decrypted secrets to the global clipboard with no `IS_SENSITIVE` marker

- **Files:** `mobile/src/backend.ts:47-49` · `mobile/src/App.tsx:432,745-752,905-912`
- **What:** Copy actions place decrypted content on the global Android clipboard
  without `ClipDescription` `EXTRA_IS_SENSITIVE`, so it appears in clipboard
  previews / clipboard-history surfaces and is readable per platform clipboard
  rules. This is the _write-side_ analogue of item 15 (which only discussed the
  read side); note item 15's "trusted same-user process" framing does not hold on
  Android where other apps are separate uids.
- **Recommendation:** Set `EXTRA_IS_SENSITIVE` on clipboard writes of decrypted
  content; consider an auto-clear timeout.

### C4 — [Low] Exported `singleTask` activity + unused browsable `clipper://` scheme

- **Files:** `mobile/android/app/src/main/AndroidManifest.xml:20,25` ·
  `mobile/src/App.tsx:64`
- **What:** `MainActivity` is `exported=true`, `launchMode="singleTask"` with
  default task affinity, and registers a `VIEW`/`BROWSABLE` `clipper://`
  intent-filter — task-affinity/StrandHogg-style hijack surface plus a
  deep-link entry point. **No `clipper://` handler exists** in JS (`mobile/src`)
  or native, so nothing currently parses injected deep-link data (no token/action
  injection found), but the unused exported surface is needless attack surface.
- **Recommendation:** Drop the unused `VIEW`/`BROWSABLE` intent-filter, or set
  a dedicated `taskAffinity=""` and handle/validate deep links explicitly if the
  scheme is intended.

### C5 — [Low] `SYSTEM_ALERT_WINDOW` declared but unused; no obscured-touch protection

- **Files:** `mobile/android/app/src/main/AndroidManifest.xml:5` (and debug
  variants) · `mobile/app.json:12`
- **What:** The shipped release manifest requests `SYSTEM_ALERT_WINDOW`
  (draw-over-other-apps) but nothing in the app uses it — leftover from a merged
  library manifest. It is an over-broad, scary permission for a clipboard app,
  and the app sets no `filterTouchesWhenObscured` / obscured-touch protection,
  so it does not defend against _other_ apps' overlay/tapjacking either.
- **Recommendation:** Remove `SYSTEM_ALERT_WINDOW` from the production manifest
  (keep only in debug if a tool needs it); add `filterTouchesWhenObscured` on
  sensitive controls (confirm/delete/logout).

---

## B. Desktop / Tauri / local

### B1 — [Low] `download_file_bytes` / `upload_file_bytes` route decrypted content through world-readable temp files

- **Files:** `web/src-tauri/src/lib.rs:352,400,530` · `crates/client/src/engine.rs:943`
- **What:** The desktop file-IPC commands stage decrypted E2E file content
  through temp files that are not created with restrictive modes, leaving a
  window in which another **local user** (different uid — not the accepted
  same-user case) could read plaintext on a multi-user machine. (Distinct from
  the client _cache_ temp files, which were verified to be `0600` at open — see
  _Verified safe_.)
- **Recommendation:** Use the bytes-returning daemon IPC path the engine already
  exposes (`download_file_bytes`/`upload_file_bytes` return/accept `Vec<u8>`) so
  decrypted bytes never touch a shared-filesystem temp, or create temps `0600`
  in an app-private dir.

### (positive) No token/key/IPC-secret is exposed to the Tauri webview

- **Files:** `web/src-tauri/src/lib.rs:108,210` · `daemon_client.rs:199` ·
  `ipc_secret.rs:35`
- Verified good: no Tauri command returns the bearer token, export/derived keys,
  or the IPC HMAC secret to the webview JS context. **Preserve this invariant** —
  do not add a command that surfaces those.

---

## Verified safe (negative results)

Independently checked and found _not_ vulnerable — recorded so the surface does
not get re-litigated:

**Web XSS / DOM:**

- All decrypted clipboard/file/device/collab-meta strings render through
  React/Tamagui **text nodes** → escaped. No `dangerouslySetInnerHTML`,
  `innerHTML`, `outerHTML`, or `insertAdjacentHTML` anywhere in `web/src`.
- CodeMirror (`@codemirror/lang-html`, `@codemirror/lang-markdown`, file viewer,
  collab editor) is **highlight-only** — no Markdown/HTML rendering or preview
  anywhere, so untrusted document text never becomes markup.
- Remote collaborator **display name** reaches the DOM only via `createTextNode`
  (a text node) — cursor-label spoofing at most, not HTML/JS injection.
- Shared `Y.Doc` `meta.language` resolves through a **fixed allowlist** lookup —
  it cannot drive an attacker-controlled dynamic `import()` or `eval`.
- The SharePage URL/query token is **never reflected** into a markup/script sink.
- The blob download path uses attacker-influenced `mime_type`/`filename` but
  forces a download (no anchor-href/script sink); the share link/token is
  rendered as text and copied, never as an `href`.
- wasm/server-returned strings reach only React text nodes or URL construction,
  not script/markup sinks.

**At-rest / keys (desktop + shared client):**

- Data key, wrapping key, and OPAQUE export key are **memory-only**; only the
  **AEAD-wrapped** device signing key touches disk.
- The device-key wrapping key = `HKDF(OPAQUE export key)` — **not derivable
  on-device without the passphrase**.
- Client at-rest temp files are `0600` at open (no world-readable
  create-then-chmod window); `ensure_private_dir` uses umask-then-chmod.
- Daemon IPC secret + cache files/dirs are `0600`/`0700` with uid + symlink
  guards — **cross-uid read is not possible** on the daemon path.

---

## Coverage, residual gaps, and what to fold in

- **Run reliability:** the two heaviest web finders first failed on oversized
  structured output; re-running them free-form recovered full coverage and, as a
  bonus, surfaced the collab surface. Net coverage of the requested themes is
  complete; the daemon/IPC and mobile-bridge memory-safety surfaces were
  examined and produced only verified-safe results plus the Low items above.
- **Not deeply pursued (lower priority, candidates for a follow-up):** the
  mobile JSI/UniFFI C++ marshalling under crafted/oversized inputs (memory
  safety) was reviewed at the boundary but not fuzzed; a per-deployment
  `connect-src` allowlist for the web CSP (item 12) remains a policy choice.
- **Recommended folding into `docs/security-review.md`:** A1 (High) and the
  Medium set (A2, A3, A4, C1) at minimum; A1 in particular deserves a decision
  before any web deployment, since it converts any script execution into
  permanent, revocation-proof compromise.

## Prioritized recommendations

1. **A1 — stop persisting the passphrase** in `sessionStorage` (re-login on
   reload, or a non-root, server-revocable resume ticket). Highest leverage.
2. **A2 — serve `web/dist` with a real CSP header + `X-Frame-Options: DENY` and
   SRI / a signed bundle**, closing the script-execution delivery vector that
   makes A1/A3 critical.
3. **C1 — set `FLAG_SECURE`** on Android to stop screen-capture/recents leakage
   of decrypted content.
4. **A4 — validate awareness colors** at the collab provider boundary; treat all
   awareness fields as hostile.
5. **Document A5/A6/A7** — make the non-E2E, anonymous, bearer-token nature of
   collaborative docs explicit, and harden the share token (out of URL, expiry,
   revoke, read-only, AEAD at rest).
6. Android Low hardening: remove `SYSTEM_ALERT_WINDOW` (C5), set
   `IS_SENSITIVE`/auto-clear on clipboard writes (C3), drop the unused
   `clipper://`/exported surface (C4), and clean up/encrypt cached downloads
   (C2). Desktop: route file IPC through bytes, not shared-fs temps (B1).
