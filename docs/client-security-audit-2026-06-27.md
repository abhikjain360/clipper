# Client-Side Security Audit — 2026-06-27

> **2026-07-19:** An independent, verifier-adjudicated second pass is
> documented in **Part II** at the bottom of this file. It re-checked Part I at
> HEAD `09296c4` (which contains the A1 fix), corrected the A1-resolution
> revocation framing below (see R14/R16), and adds 45 adjudicated findings
> (R1–R45) plus 2 refutations and a fresh dependency scan.

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
> token (logout / device removal) bounds **API access**. Two precision notes
> from the 2026-07-19 second pass (Part II, R14/R16): **(a)** the data key is
> static (HKDF of the OPAQUE export key, never rotated), so revocation cannot
> claw back ciphertext an attacker already recorded — including the persistent
> `localStorage` ciphertext cache, which outlives the tab; **(b)** no code
> removes pre-fix `clipper.session.v1` (passphrase) entries, so a long-lived
> pre-fix tab keeps the full A1 condition until it closes. The residual
> XSS-readability of the data key (the wasm AEAD needs raw key bytes) and the
> `sessionStorage` store itself are documented in
> `docs/local-at-rest-encryption.md`.

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

> **Correction (2026-07-20, `docs/crypto-review-2026-07-20.md` CR5):** the
> "not JS-readable" framing below is optimistic. Wasm linear memory is JS-readable
> whenever the module exports its memory (wasm-bindgen does), so the data key is
> **not** protected from same-origin script by living in wasm memory — and the
> confused-deputy point makes the distinction moot regardless. The honest statement
> is: on web, the data key is exposed to any same-origin script, period. The
> stronger implied mitigation is also unavailable: WebCrypto has no ChaCha20-Poly1305,
> so the key cannot be held as a non-extractable browser key without breaking
> cross-platform E2E (web ciphertext must stay XChaCha20-Poly1305 to sync with
> native). The real controls are therefore XSS / bundle-tamper prevention (A2),
> session-scoped key lifetime (`clipper.session.v2`), and key rotation (CR1).

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

---

# Part II — Independent Re-Audit (2026-07-19)

> **Why a second pass.** Part I was produced by a model the operator does not
> trust on cryptography (upstream capability restrictions; some work was
> silently delegated to a weaker model). Part II is a from-scratch re-audit at
> HEAD `09296c4` — which includes the A1 fix — with the explicit goal of
> catching whatever Part I missed or got wrong. Same scope and threat model as
> Part I. Findings below are **new** unless tagged as re-confirmations.

## Method (Part II)

14 finder agents, each assigned one attack vector (core crypto, OPAQUE, the A1
fix / session resume, sync & event log, at-rest storage, daemon IPC, web DOM
re-audit, wasm boundary, Android, UniFFI/JSI, Tauri shell, transport,
untrusted-data robustness, collab), reading real code at HEAD. Their 83 raw
candidates were deduplicated into 46 claims; each claim was then adjudicated by
an **independent adversarial verifier** instructed to default to _refuted_:
re-read every citation, hunt for missed mitigations, correct the mechanism, and
re-grade severity. Result: **44 confirmed** (many with corrections), **2
refuted**. Verifier corrections mattered — one finder's headline crypto claim
("OPRF blind + server response yields the data key without the passphrase") was
shown to be **wrong** (R22), and the device-signature domain-separation gap was
shown to be blocked by a SHA-256 preimage barrier (R38). The load-bearing
crypto core was separately re-verified and **held** (see _Verified safe,
Part II_). Dependencies were covered by `nix run .#audit` / osv-scanner (R45).

**What happened to Part I at re-examination.** The A1 fix is real and complete
on web — with residual-precision corrections (R14, R16, and the amended note
above). A4 re-confirmed end-to-end (R12). B1 re-confirmed and sharpened (R11).
C1–C5 re-confirmed unchanged at HEAD (R13). The "clean DOM" conclusion was
re-derived; no new injection sink was found. Nothing in Part I proved
fabricated, but its crypto-adjacent analysis was thin; Part II goes
considerably deeper on crypto, sync integrity, and the post-June-27 code.

## Headline (Part II)

- **The crypto core is sound.** AEAD nonce generation, HKDF separation, AAD
  binding, verify-before-release ordering, the OPAQUE suite, and all RNG
  sources were independently re-verified (_Verified safe, Part II_). The
  feared crypto breakage does not exist.
- **The A1 pattern lives on Android**: the mobile app persists the OPAQUE
  passphrase — the revocation-proof E2E root — in the OS keystore for biometric
  resume, and the safer v2 resume material built for the web fix was never
  wired to mobile (R1, Medium).
- **The sync engine trusts server-supplied `seq`s blindly** (R2–R5): one
  far-future seq makes an object permanently delete-immune and sweep-immune;
  and pagination loops, live-event fan-out, and full-refetch-on-reconnect give
  a malicious server extreme, cheap availability attacks.
- **A malicious server can substitute one genuine user file for another at
  download time** (R6) — the client never checks that the returned object id
  matches the request.
- Two **self-inflicted functional bugs** rate Medium on their own: web
  clipboard sync wedges on ordinary large copies (R7), and the first Copy on
  the web client panics the wasm clipboard path (R8).
- Collab remains the soft spot: no reconciliation (R9), unbounded growth DoS by
  any anonymous token holder (R10), awareness channel still unsanitized (R12).
- As in Part I: no anonymous-remote critical was found; everything requires a
  malicious server, script on the origin, an installed app, a share-token
  holder, or a local position.

## Summary table (Part II)

| #   | Sev    | Platform  | Title                                                                                                                                      | vs Part I                |
| --- | ------ | --------- | ------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------ |
| R1  | Medium | android   | Passphrase (E2E root) persisted in OS keystore for biometric resume; v2 resume material never wired                                        | new (A1 variant)         |
| R2  | Medium | sync      | Server-controlled `seq`s poison LWW: far-future seq → permanent delete/sweep immunity                                                      | new                      |
| R3  | Medium | sync      | Unbounded reconciliation pagination — malicious server loops the client forever                                                            | new                      |
| R4  | Medium | sync      | WS `created`-event flood spawns unbounded materialization tasks (no dedup while pending)                                                   | new                      |
| R5  | Medium | sync      | Every reconnect re-downloads all retained clipboard payloads; `invalidate` → continuous flood                                              | new (rel. review 9–10)   |
| R6  | Medium | files     | `download_file_bytes` doesn't pin the returned object id — malicious-server file substitution                                              | new                      |
| R7  | Medium | web       | Clipboard sync wedges on large/aggregate items (localStorage quota; JSON number-array sidecar)                                             | new                      |
| R8  | Medium | web       | `Instant::now()` panics the wasm clipboard path on first Copy                                                                              | new                      |
| R9  | Medium | collab    | No reconciliation path — missed `created`/`deleted` events are unrecoverable                                                               | new                      |
| R10 | Medium | collab    | Unbounded doc growth: any share-token holder permanently bloats a doc for all participants                                                 | new                      |
| R11 | Medium | desktop   | B1 re-confirmed, sharpened: decrypted content via predictable 0644 temp files in shared `/tmp`                                             | re-confirmed + sharpened |
| R12 | Medium | collab    | Awareness channel unsanitized/unrate-limited; malformed cursor permanently kills remote-selection plugin                                   | A4 re-confirmed + new    |
| R13 | Medium | android   | C1–C5 all still present at HEAD; C1 exposure grew with the in-app content viewer                                                           | re-confirmed             |
| R14 | Low    | web       | Post-A1 residual: static data key in `session.v2`; revocation bounds API only; stale v1 never purged                                       | new (A1 follow-up)       |
| R15 | Low    | all       | Revoked-session 401s never acted on: no teardown, dead token retried forever                                                               | new                      |
| R16 | Low    | web       | Session cleanup gaps: blob kept after definitive-401 resume; current-device removal doesn't clear it                                       | new                      |
| R17 | Low    | web       | Engine server-URL binds pre-auth: login DoS / URL fixation until reload                                                                    | new                      |
| R18 | Low    | sync      | File delete tombstones swept after one generation — server resurrects deleted files                                                        | new                      |
| R19 | Low    | collab    | Create/delete mint seqs from the local clock — clock skew silently voids deletes                                                           | new                      |
| R20 | Low    | sync      | Snapshot per-item failure treated as "not seen" — sweep deletes the last good local copy                                                   | new                      |
| R21 | Low    | sync      | Logout doesn't fence in-flight sync writes; failed materialization never retries (doc mismatch)                                            | new                      |
| R22 | Low    | crypto    | Serialized OPAQUE client state unzeroized → KSF-free `H(passphrase)` dictionary oracle                                                     | new                      |
| R23 | Low    | crypto    | Non-zeroized key-material copies (device signing key, data key, export key, JSI)                                                           | new (rel. review resid.) |
| R24 | Low    | desktop   | Legacy plaintext `.payload`/`.txt` clipboard sidecars never swept                                                                          | new                      |
| R25 | Low    | all       | Device-identity record: `device_id` unauthenticated (constant wrap AAD); invalid records re-minted                                         | new                      |
| R26 | Low    | desktop   | Daemon IPC byte fields as JSON number arrays — ≳9 MiB clipboard wedges the Tauri connection                                                | new                      |
| R27 | Low    | desktop   | Slow broadcast consumer evicted as "dead" (`try_send` Full ≡ Closed)                                                                       | new                      |
| R28 | Low    | desktop   | `CLIPPER_DAEMON_SOCKET_PATH` override chmods its parent dir `0700` unconditionally                                                         | new                      |
| R29 | Low    | desktop   | Tauri production CSP blocks collab WS + share-meta fetch — collab dead in packaged builds                                                  | new                      |
| R30 | Low    | web       | `uploadFileBytes` has no size cap; whole-file triple-buffering → tab OOM                                                                   | new                      |
| R31 | Low    | all       | Server error text flows verbatim into UI error banners and daemon logs                                                                     | new                      |
| R32 | Low    | android   | Raw UniFFI JSI module on `globalThis` — in-bundle JS gets native-memory primitives                                                         | new                      |
| R33 | Low    | all       | Duplicate `ws_loop`s from unguarded auth transitions; no mobile engine teardown; racy JS-teardown cb                                       | new                      |
| R34 | Low    | web       | Collab editor destroyed/rebuilt on every parent re-render (inline `collab` prop)                                                           | new                      |
| R35 | Low    | collab    | y-websocket retries forever after 403; ghost rooms; deletion doesn't kick connected peers                                                  | new                      |
| R36 | Low    | linux     | Wayland privacy-marker TOCTOU — no post-read re-check (macOS has one)                                                                      | new                      |
| R37 | Low    | transport | Browser WS inbound uncapped; no heartbeat/read deadline; no HTTP total deadline                                                            | new                      |
| R38 | Info   | crypto    | Device key signs two message types without domain separation (not exploitable; preimage barrier)                                           | new                      |
| R39 | Info   | crypto    | Client signs server-chosen `device_proof_challenge` verbatim                                                                               | new                      |
| R40 | Info   | all       | Session identity fields not cross-checked at login/resume (no token↔device binding)                                                        | new                      |
| R41 | Info   | all       | Unvalidated on-disk record id in deletion paths; i64 overflow in cached blob-size sum                                                      | new                      |
| R42 | Info   | desktop   | Tauri Linux IPC-secret read lacks the daemon's symlink/regular-file guard                                                                  | new                      |
| R43 | Info   | web       | SharePage casts `/meta` JSON without validating `object_id`                                                                                | new                      |
| R44 | Info   | collab    | `materialize_collab` / `get_collab_doc_meta` don't verify the returned object id                                                           | new (capped by A5)       |
| R45 | Info   | deps      | `anyhow` 1.0.102 (RUSTSEC-2026-0190); `quick-xml` 0.39.4 via tauri/plist (RUSTSEC-2026-0194/-0195); `opaque-ke` on pre-release 4.1.0-pre.2 | new                      |

---

## Part II — Medium findings

### R1 — [Medium] Android persists the OPAQUE passphrase (E2E root) for biometric resume; the v2 resume material was never wired to mobile

- **Files:** `mobile/src/backend.ts:116-135,155-177` · `mobile/src/App.tsx:98,191` ·
  `crates/client/src/engine.rs:308-346,355-369` · `packages/mobile-bridge/src/adapter.ts:96-102` ·
  `crates/mobile-uniffi/src/lib.rs` (no resume export)
- **What:** After every login/register the app saves
  `{ passphrase, username, deviceName, serverUrl }` via
  `SecureStore.setItemAsync(..., { requireAuthentication: true })`; cold-start
  `resumeSession()` reads it behind a biometric prompt and **replays a full
  OPAQUE login with the stored passphrase**. Verified in the expo-secure-store
  56.0.4 source: AES-256-GCM key generated inside AndroidKeyStore, Class-3
  biometric gate only (no PIN/device-credential fallback), uid-sandboxed,
  `allowBackup=false`. Meanwhile the safer material built for the web A1 fix —
  `SyncEngine::resume_with_platform` + `session_resume_material` (token + data
  key + wrapping key, no passphrase) — exists but `crates/mobile-uniffi`
  exports no resume function, and the bridge adapter throws "Session resume is
  handled by the mobile keystore flow."
- **Why it matters:** The passphrase is the revocation-proof root (Part I, A1).
  Server-side revocation is useless against a stolen passphrase:
  `delete_device` (`crates/server/src/routes/auth.rs:568`) removes one device
  row, but the new-device login branch (`auth.rs:791-834`) mints a fresh device
  row for **any** successful OPAQUE login, and no passphrase-change/rotation
  flow exists anywhere in the repo. Versus web A1 the store is materially
  better protected (Keystore-wrapped, biometric-gated, uid-isolated — the
  "separate installed app" actor gets nothing), hence Medium rather than High:
  extraction needs a rooted/forensic device, a Keystore/biometric bypass, or a
  coerced unlock. Versus the web fix it is also **worse** in two ways: the
  store persists indefinitely across reboots (the web blob is tab-lifetime),
  and the passphrase re-enters the RN JS heap on every cold start
  (`backend.ts:160-176`).
- **Recommendation:** Export `resume`/`session_resume_material` equivalents
  over UniFFI and store `{ token, data_key, wrapping_key, … }` in the same
  Keystore-gated slot instead of the passphrase, mirroring
  `clipper.session.v2` — device removal then actually bounds a store
  compromise. (`docs/local-at-rest-encryption.md` updated in this pass.)

### R2 — [Medium] Server-controlled `seq`s are trusted as LWW timestamps: one far-future seq makes an object permanently delete-immune and sweep-immune

- **Files:** `crates/client/src/engine.rs:1204-1221,1275-1281,1515-1518` ·
  `crates/client/src/local_store.rs:318-326,606-611,699-706,738-742,766-772` ·
  `crates/api-types/src/lib.rs:581-588` · `crates/server/src/state.rs:255-270`
- **What:** Seq ingestion has no sanity bound anywhere. Live events and
  snapshot `created_seq` values flow verbatim into `event_seq`/`created_seq`
  (the only guard is a tombstone check; there is no Present-vs-Present guard
  and no range check). Legitimate seqs are server wall-clock µs (~1.75e15
  today), so a poisoned value such as `i64::MAX` is trivially distinguishable —
  yet nothing checks. `hello_ack.server_time` is sent and typed but the client
  never reads it (both connect loops destructure and discard it,
  `engine.rs:1780-1782,1922-1924`).
- **Impact:** A malicious server serves one device a listing or live `created`
  event for real file X with `created_seq = i64::MAX`. The device persists it
  to disk. Thereafter: authentic delete events are dropped
  (`record.event_seq() >= event_seq` at `local_store.rs:738-742`); the sweep
  skips the record forever (`created_seq <= stream_start_seq` fails); even the
  poisoned device's **own** delete silently no-ops locally while the UI
  repaints the item. X is immortal in that device's UI, per-device forkable
  (list/WS responses are per-connection). For clipboard — which has no delete
  path at all — poisoning also defeats retention hygiene: after server-side
  expiry the sweep still skips the item, so "expired" clipboard data persists
  on the client indefinitely.
- **Recommendation:** Validate every ingested seq (WS events and list
  `created_seq`) against `hello_ack.server_time` plus a small grace window;
  reject or clamp out-of-range values. Make the `created`-for-Present arm
  (`local_store.rs:699-706`) a pure duplicate that only refreshes
  `seen_generation` — an object is created exactly once, so that seq-bump arm
  is dead code that exists only as attack surface.

### R3 — [Medium] Unbounded reconciliation pagination — a malicious server can loop the client forever

- **Files:** `crates/client/src/engine.rs:1263-1290,1318-1353` ·
  `crates/api-types/src/lib.rs:497-501` · `crates/client/src/api_client.rs:14,581,591-593`
- **What:** The snapshot loops' entire termination logic is
  `match page.next_after { Some(cursor) => after = Some(cursor), None => break }`
  — no page counter, no monotonicity check on the cursor, no timeout. The
  cursor is fully server-controlled (the honest server only sets `next_after`
  when there is more, but the client relies on server honesty for progress).
  The cheapest variant needs no items at all: `next_after: Some(_)` with an
  empty `items` array loops forever.
- **Impact:** A malicious server answers with the same page plus a repeating
  cursor forever. The reconciliation task — started automatically on every WS
  connect — pages and re-downloads up to 100 × 16 MiB per cycle, 8 downloads in
  flight (`CLIPBOARD_HYDRATION_CONCURRENCY`), indefinitely: unbounded
  metered-data/battery drain (worst on Android), decrypt/CPU/UI churn, and the
  pass never terminates so `sweep_kind` never runs. Per-item integrity still
  holds (hash + AEAD), so this is resource exhaustion, not forgery.
- **Recommendation:** Remember the previous cursor and break with a warning
  unless `next_after` strictly advances in `(created_seq, id)` order; cap total
  pages/items per pass. Cheap hardening: skip the payload download when the
  object id is already persisted with the same envelope (see R5).

### R4 — [Medium] WS `created`-event flood spawns unbounded materialization/download tasks (no dedup while pending)

- **Files:** `crates/client/src/local_store.rs:696-716` ·
  `crates/client/src/engine.rs:1451-1535,2448-2462,2006`
- **What:** `mark_pending_create_inner` returns `Ok(true)` unconditionally on
  the `PendingCreate` arm — dedup exists only for fully materialized `Present`
  records. `handle_created_event` spawns a fresh background task per event (no
  in-flight set, no coalescing, no semaphore); each task does `get_object` +
  `download_object_payload` of up to 16 MiB + decrypt. If materialization keeps
  failing (e.g. the server serves a genuine envelope with a hash-mismatched
  payload), the marker stays `PendingCreate` forever, so **every** duplicate
  event spawns another full-download task — not merely a race window. Duplicate
  events with `created_seq >= record.event_seq` also each cost an fsync'd
  marker rewrite inside the WS read loop. On wasm, the inbound `BrowserWs`
  channel is an unbounded mpsc (`engine.rs:2006`).
- **Impact:** A malicious server streams ~150-byte `created` frames for one
  real object id at arbitrary rate while keeping it permanently pending:
  unbounded concurrent 16 MiB downloads + decrypts on the desktop daemon (OOM),
  bandwidth/CPU burn plus unbounded wasm-heap growth on web/Android — ~10⁵×
  cost asymmetry. Peers cannot inject events (the honest server emits one
  broadcast per committed create), so this needs the malicious-server actor.
- **Recommendation:** Return `Ok(false)` from `mark_pending_create_inner` when
  a `PendingCreate` marker already exists (spawn only on the absent→pending
  transition, still refreshing `seen_generation`/`event_seq`); add a
  `Semaphore` mirroring `CLIPBOARD_HYDRATION_CONCURRENCY`; make the wasm
  inbound channel bounded, closing the socket on overflow.

### R5 — [Medium] Non-incremental reconciliation: every reconnect re-downloads all retained clipboard payloads; the server can force it continuously

- **Files:** `crates/client/src/engine.rs:1240-1247,1318-1347,1469-1478,1696-1698,1806-1808,1939-1941` ·
  `crates/client/src/local_store.rs:554-595` · `crates/server/src/ws.rs:321-341`
- **What:** `snapshot_clipboard` pages through the full retained list and calls
  `decrypt_clipboard_object_item_with_api` for **every** item, which
  unconditionally re-downloads the payload (no local-cache skip) and
  unconditionally re-persists (no unchanged-content short-circuit). There is no
  persisted sync cursor, so reconciliation is never incremental. `Invalidate`
  (or a plain server close) returns `Ok(())`, which resets the reconnect
  backoff to 1 s — so a ~30-byte `{"type":"invalidate"}` frame per connection
  keeps every client in a continuous re-list/re-download/re-decrypt/re-fsync
  cycle (worst case ≈ 100 items × 16 MiB ≈ 1.6 GiB per cycle).
  Overlapping reconnects stack concurrent download loops (superseded loops are
  not aborted; their persists just become no-ops).
- **Impact:** A malicious server pins every connected client's downlink, CPU,
  and disk I/O indefinitely at a cost of a few bytes per cycle, affecting all
  of a user's devices simultaneously; even an honest-but-flaky network makes
  full re-ingest the routine reconnect cost. Availability only; overlaps the
  partially pre-acknowledged class in `docs/security-review.md` items 9–10.
- **Recommendation:** Skip the network download when a local `Present` record
  matches `(id, created_seq)` and instead re-verify the cached ciphertext
  against the envelope's payload hash; don't reset backoff to 1 s on
  server-initiated `Invalidate`; longer-term persist a sync cursor so
  steady-state reconciliation is incremental.

### R6 — [Medium] `download_file_bytes` never verifies the returned object matches the requested id — malicious-server file substitution

- **Files:** `crates/client/src/engine.rs:995-1030` (missing check) vs
  `engine.rs:1593-1597` (`materialize_object` has it) ·
  `crates/client/src/engine.rs:2293,2338-2342` · `crates/core/src/crypto.rs:193-212` ·
  sinks: `web/src/App.tsx:645-646,656-658` · `mobile/src/App.tsx:538-539` ·
  `web/src-tauri/src/lib.rs:373-398`
- **What:** After `api.get_object(file_id)` the code verifies the returned
  envelope's **self-consistency** (`body.object_id != item.id`), kind,
  single-payload, size cap, and payload SHA-256 — but never compares
  `file_item.id` to the requested `file_id`. The AEAD AAD binds the _returned_
  object's id (`ObjectAadV1` is built from `file_item.envelope.body`), so a
  substituted **genuine** object decrypts cleanly under its own AAD; the check
  is missing in exactly this one of the two `get_object` callers.
- **Impact:** The server answers `GET /api/objects/{A}` with the user's genuine
  file B; the client decrypts B and the UI saves/views it **under A's filename
  and MIME type**. Concrete harm: the user clicks "resume.pdf" to export and
  unknowingly ships the decrypted contents of "passwords.kdbx" to a third party
  — server-induced cross-document disclosure with no client-side signal.
  Bounded: the server cannot inject attacker-authored plaintext (no key) and
  can only swap among the user's own stored files.
- **Recommendation:** After `get_object`, reject unless the returned item id
  equals the requested `file_id` (mirroring `engine.rs:1593`). The same check
  is missing for collab metadata — see R44.

### R7 — [Medium] Web clipboard sync wedges on large/aggregate items: JSON number-array sidecar vs. localStorage quota

- **Files:** `crates/client/src/local_store.rs:33,591-594,1324-1331,1345-1348,1871-1879` ·
  `crates/client/src/engine.rs:42,672-701,1184-1192,1344-1366,1472-1478` ·
  `crates/server/src/config.rs:49-52`
- **What:** The wasm local store serializes payload ciphertext with
  `serde_json::to_string(&[u8])` — an array of byte numbers, ~3.6 chars/byte —
  into `localStorage` (5–10 MB/origin quota). A single ~2 MB copy, or the
  aggregate of a week's ordinary image copies (count-based eviction at 1000
  records never relieves byte pressure), throws QuotaExceededError. The write
  order makes it fatal: payload sidecar first, and the persist `?` aborts the
  entire snapshot page loop, skipping the sweep. Every reconnect re-downloads
  the oversized item and fails identically. On upload, the persist error
  propagates to the user **after** the object already committed server-side.
- **Impact:** No attacker needed. The oversized item never appears on web, no
  newer clipboard item created while web was offline ever backfills (pages are
  ascending, abort cuts off the tail), the sweep never runs, and the user
  cannot delete the item (clipboard deletes are unsupported) — a stale web view
  for up to the 7-day server TTL. Each reconnect re-downloads-and-fails
  (compounds R5).
- **Recommendation:** Store the wasm sidecar compactly (single base64 string,
  or IndexedDB/CacheStorage); make per-item persist failures non-fatal in the
  snapshot loop (warn + skip, matching the decrypt-failure arm) so the snapshot
  completes and sweeps; treat the post-commit local persist on upload as
  best-effort.

### R8 — [Medium] `std::time::Instant::now()` panics on wasm32-unknown-unknown, killing clipboard copy in the browser client

- **Files:** `crates/client/src/engine.rs:758-761,792-795` (`Instant::now()`),
  `:570` (`elapsed()`), `:81` (field) · `crates/web-wasm/src/lib.rs:304-313` ·
  `web/src/App.tsx:514` · `crates/client/Cargo.toml:53-66` (no `instant`/`web-time`)
- **What:** The echo-suppression path stores a timestamp with
  `std::time::Instant::now()`, which on wasm32-unknown-unknown is
  `panic!("time not implemented on this platform")` (std `unsupported.rs` —
  verified against the pinned toolchain; no shim crate exists, and none could
  patch std for direct callers). With wasm `panic=abort`, the trap throws into
  the JS microtask: the copy's Promise never settles (silent failure, no error
  banner), and the panic occurs while the `suppressed_payload` write lock is
  held — abort runs no destructors, so **every subsequent clipboard operation
  hangs forever** on the poisoned lock. Non-clipboard features keep working;
  the wasm instance itself survives.
- **Impact:** Any standalone-web user who clicks Copy on a clipboard item
  silently loses the copy and all subsequent clipboard send/copy operations
  until a full page reload + session resume. It also means the 5-second
  echo-suppression mechanism has never functioned on web — this path is
  untested in the browser.
- **Recommendation:** Replace `std::time::Instant` with `web_time::Instant`
  (drop-in, works on native and wasm; already in the dependency tree
  transitively). Add a wasm smoke test that calls `clipboard_payload` so this
  class of bug is caught in CI.

### R9 — [Medium] Collab docs have no reconciliation path — missed `created`/`deleted` events are unrecoverable

- **Files:** `crates/client/src/engine.rs:1173-1193,1294,1357,1524,1637-1658` ·
  `crates/server/src/routes/objects.rs:921-925` (collab excluded from listings) ·
  `crates/server/src/ws.rs:177-207,295-297` (no replay) · `docs/collab-docs-plan.md:140-141`
- **What:** `start_reconciliation` snapshots only File and Clipboard; the
  object list endpoint explicitly excludes collab; there is no collab list
  endpoint at all; the WS never replays; and collab records are never swept.
  So one dropped/lagged `created` event means a device never learns the doc
  exists; one missed `deleted` broadcast leaves a ghost entry forever (opening
  it 404s, but it never disappears from the UI). The code's own comment
  concedes the design (`engine.rs:1088-1095`), and `collab-docs-plan.md:140-141`
  (which says the sync engine should handle collab in the event log and object
  listings) diverges from shipped code — oversight, not intent.
- **Impact:** Honest case: create a collab doc on desktop while the phone is
  offline → the phone never shows it, with no error anywhere. Malicious-server
  case: selectively withhold collab broadcasts per connection for permanent
  per-device view forks — for files/clipboard the server must lie on _every_
  snapshot to sustain an omission; for collab a single withheld event is sticky
  forever. Content itself remains reachable by id/share-token; no
  confidentiality impact.
- **Recommendation:** Add an authenticated `GET /api/collab-docs` list endpoint
  and run a collab snapshot + `sweep_kind(ObjectKind::Collab, …)` in
  `start_reconciliation` (the store already has `persist_snapshot_collab_present`
  and a kind-generic `sweep_kind_inner`). Update `docs/collab-docs-plan.md`
  (done in this pass).

### R10 — [Medium] Unbounded collab-doc growth: any share-token holder can permanently bloat a doc for all participants

- **Files:** `crates/server/src/collab_sync.rs:76,174-180,191-224,243-261` ·
  `crates/server/src/routes/collab.rs:388-411,405-407,453-455` ·
  `crates/server/src/main.rs:380-384` · `web/src/CodeEditor.tsx:95-103,154`
- **What:** The only size control is a 4 MiB **per-message** cap (inbound only);
  there is no cumulative doc-size, update-count, or per-connection rate limit
  anywhere, and the per-IP HTTP limiter gates only the upgrade request. The
  server integrates any well-formed update into the authoritative `yrs::Doc`,
  persists the full state, and serves the entire state to every future joiner.
  Yjs/yrs tombstones make growth monotone — even "deleting the junk" never
  reclaims space. The WS is anonymous: the `?token=` share token is the sole
  credential (A6/A7).
- **Impact:** Any share-link recipient streams junk updates indefinitely; the
  doc swells to hundreds of MB; every participant who opens it afterward —
  owner on any device, any guest — downloads, decodes, and loads the entire
  state into a `Y.Doc` + CodeMirror (plus a full-string copy per editor
  rebuild, see R34): the tab hangs or OOMs. Damage survives the attacker
  disconnecting (persisted `yjs_state`); recovery is deleting the whole doc.
- **Recommendation:** Enforce a cumulative ceiling in `apply_remote_update`
  (re-encode after integrating; reject/close past a limit like 10–50 MB) plus a
  per-connection update rate/byte budget; optionally cap `yjs_state` at persist
  time and guard client-side before handing oversized text to CodeMirror.

### R11 — [Medium] B1 re-confirmed, sharpened: decrypted file content staged through predictable, default-permission temp files in shared `/tmp`

- **Files:** `web/src-tauri/src/lib.rs:358-368,405-420,530-542` ·
  `crates/client/src/engine.rs:1044-1045` · `crates/daemon-types/src/protocol.rs:158-166` ·
  `crates/daemon/src/handler.rs:405-409,632-653`
- **What:** The desktop download path stages decrypted plaintext via
  `tokio::fs::write` to `std::env::temp_dir().join("clipper-{pid}-download-{file_id}")`
  — `File::create` semantics: mode `0666 & ~umask` (typically 0644), follows
  symlinks, no `O_EXCL`/`O_NOFOLLOW`, deterministic per (pid, suffix) path
  (suffix sanitized, no traversal). The prior B1 mechanism is unchanged; the
  sharpened facts: (a) the **live desktop trigger is narrower than claimed** —
  uploads go through the native dialog with the real path (no temp), so the
  exposure is the text-file **viewer** (`App.tsx:727-732` →
  `download_file_bytes` unconditionally under Tauri); (b) the symlink
  pre-plant → cross-uid overwrite primitive is largely neutralized by Linux
  `fs.protected_symlinks=1`; the reliable primitive is the plain **read
  window** (pid via `ps`, `clipper-*` watchable via inotify); (c) macOS is
  unaffected (per-user 0700 `$TMPDIR`); (d) two concurrent viewer opens of the
  same file race one path and can return torn/mixed plaintext; (e) the engine
  already exposes temp-free `upload_file_bytes`/`download_file_bytes`
  (bytes-over-IPC is proven by the clipboard commands) — but a naive bytes-IPC
  switch hits the 32 MiB line cap because of R26's JSON encoding.
- **Impact:** On multi-user Linux, another local user (explicitly in scope)
  watches `/tmp` and reads the decrypted plaintext of any file the victim opens
  in the desktop text viewer — defeating E2E confidentiality for that file. A
  crash leaves the plaintext in `/tmp` indefinitely; nothing sweeps it.
- **Recommendation:** Add byte-carrying `UploadFileBytes`/`DownloadFileBytes`
  IPC variants (with base64/binary bulk framing per R26) and delete the temp
  staging. Interim: `OpenOptions::new().create_new(true).mode(0o600)` plus a
  random name component inside an app-private 0700 temp dir.

### R12 — [Medium] Collab awareness channel unsanitized and unrate-limited (A4 re-confirmed; malformed cursor permanently kills the remote-selection plugin)

- **Files:** `web/src/CodeEditor.tsx:104-109,157` ·
  `web/node_modules/y-codemirror.next/src/y-remote-selections.js:88,186-195,206,217,227,236` ·
  `web/node_modules/yjs/dist/yjs.mjs:2518-2519,2554` ·
  `@codemirror/view` plugin-crash path (`dom.style.cssText`, `PluginInstance.update`) ·
  `crates/server/src/collab_sync.rs:448-456,67,71,76` · `web/index.html:8`
- **What:** (a) **A4 re-confirmed end-to-end:** remote `user.color`/`colorLight`
  flow unsanitized into full inline `style` attributes on remote caret/selection
  decorations; only the _local_ color is palette-pinned. Impact ceiling
  unchanged: CSS-only UI-redress (no script path under the CSP; `img-src`
  blocks external `url()` exfil but allows `data:` images). (b) **New:** a
  malformed awareness `cursor` (`{anchor:{},head:{}}`) throws inside
  `Y.createAbsolutePositionFromRelativePosition`; CodeMirror catches the plugin
  crash and **permanently deactivates the remote-selections plugin** for that
  editor instance — remote carets _and_ local cursor broadcast stay dead until
  the EditorView is rebuilt, and the bad state lingers up to the 30 s awareness
  timeout (refreshable for a sustained room-wide grief). A finder's
  "VERIFIED-SAFE: crafted cursors fail closed" claim was verified **wrong**.
  (c) No awareness validation or rate/size budget beyond the 4 MiB frame cap;
  invalid-JSON states throw uncaught per frame and `awareness.states` is
  unbounded. Attacker set: any anonymous share-token holder, or the server.
- **Recommendation:** Add an awareness `change`/`update` listener that rewrites
  remote states before `yCollab` consumes them: coerce colors to
  `^#[0-9a-fA-F]{3,8}$` (or derive from `clientid` via `CURSOR_COLORS` and
  ignore wire values) and drop `cursor` unless anchor/head are well-formed
  `{client, clock}` items. Server-side: validate awareness frame shape and
  apply a per-connection rate/size budget before relaying.

### R13 — [Medium] Android C1–C5 re-confirmed at HEAD — none fixed; C1's exposure grew with the in-app content viewer

- **Files:** `mobile/android/app/src/main/java/com/clipper/mobile/MainActivity.kt:13-20` ·
  `mobile/android/app/src/main/AndroidManifest.xml:5,15,20,25-30` ·
  `mobile/src/backend.ts:47-49,73-85` · `mobile/src/App.tsx:432,474,538-539,908,914-950`
- **What:** All five Part I Android items verified still present at HEAD
  `09296c4`: **C1** no `FLAG_SECURE` anywhere (grep: zero hits), no
  `expo-screen-capture` — and the new in-app viewer (`App.tsx:914-950`) renders
  full decrypted content in a `TextInput`, extending the screenshot/recents/
  MediaProjection surface; **C2** downloads written decrypted to the app cache
  dir and never deleted (no `finally`); **C3** copy paths use
  `Clipboard.setStringAsync` — verified in expo-clipboard 56.0.4's Kotlin: no
  `IS_SENSITIVE` anywhere; **C4** exported `singleTask` activity +
  `clipper://` BROWSABLE filter, still with no handler anywhere (latent surface
  only); **C5** `SYSTEM_ALERT_WINDOW` in the release manifest, unused; no
  `filterTouchesWhenObscured`.
- **Recommendation:** Unchanged from Part I, now with a concrete C1 path:
  `window.setFlags(FLAG_SECURE, FLAG_SECURE)` in `MainActivity.onCreate` plus
  `setRecentsScreenshotEnabled(false)` on API 33+; delete the temp file in a
  `finally` after `shareAsync`; attach `EXTRA_IS_SENSITIVE` via a tiny native
  module; drop the `clipper://` filter and `SYSTEM_ALERT_WINDOW`.

---

## Part II — Low findings

### Session & auth

**R14 — Post-A1 residual precision: `session.v2` = token + static data key + wrapping key; revocation bounds API access only; stale v1 never purged.**
`web/src/backend/index.ts:74-105` · `crates/web-wasm/src/lib.rs:212-239` ·
`crates/client/src/engine.rs:93-97,355-369` · `crates/client/src/api_client.rs:273-275,394-396` ·
`crates/client/src/local_store.rs:1443-1478,1706-1746,1872-1879`.
The blob definitively holds `{token, dataKey, wrappingKey, …}` — the wrapping
key additionally unwraps the device's Ed25519 identity from `localStorage`
(envelope-signing as the device while the token lives). Both keys are HKDF of
the OPAQUE export key, stable per passphrase, and **no rotation flow exists**.
So a one-shot same-origin script that reads the blob once gets: pre-revocation,
full device impersonation via the API plus offline decryption of everything
fetched and of the persistent `localStorage` ciphertext cache (which outlives
the tab); post-revocation, every ciphertext byte already exfiltrated — or
archived by a malicious server, past _and future_ — stays decryptable forever.
The pre-fix/post-fix delta is real but narrower than the original note implied:
the attacker loses OPAQUE re-login, session minting after revocation, and
device enrollment — session control improved; content confidentiality vs. a
malicious server is equally and permanently lost until a passphrase-rotation
feature exists. The A1-fix completeness itself was verified (no passphrase or
OPAQUE-replayable secret in any web storage) — except that **no code removes
stale `clipper.session.v1`** entries, so a long-lived pre-fix tab retains the
passphrase until it closes (plus session-restore tail). The A1-resolution note
above and `docs/local-at-rest-encryption.md` were corrected in this pass.
_Fix:_ one startup line `sessionStorage.removeItem("clipper.session.v1")`;
consider per-object/epoch-rotated data keys; return resume material once at
login rather than via an always-callable export.

**R15 — Revoked-session 401s are never acted on: dead token retried forever, no session teardown.**
`crates/client/src/api_client.rs:1028` (status carried but never matched — repo
grep finds only the 404 match at `engine.rs:2444-2446`) ·
`crates/client/src/engine.rs:1696-1720,1877-1900` (ws*loop treats every error
identically) · `engine.rs:1757-1759` (native handshake 401 stringified, status
erased) · `web/src/backend/index.ts:150-154` (resume catch keeps the blob on a
definitive 401). After device removal, the revoked client keeps the data key,
wrapping key, signing identity, and all cached decrypted content in memory
indefinitely; keeps serving `getState`/`clipboardPayload`/`sessionResumeMaterial`
to any same-origin script; shows only a "Disconnected" badge; and hammers the
server with 401s at ≤60 s intervals forever. The reload path does fail closed
(resume validates the token first), so this applies to the live,
never-reloaded session. \_Fix:* match `ClientError::Api { status: 401, .. }` in
the sync paths (and re-check ambiguous WS failures via `validate_session`) and
run the full `logout()` teardown on a confirmed 401; clear the web resume blob
on definitive auth failure.

**R16 — Session cleanup gaps: revoked-token blob kept after failed resume; current-device removal doesn't clear it; engine-side asymmetric token cleanup.**
`web/src/backend/index.ts:135,150-154` (only JSON-parse failure clears the
blob) · `web/src/App.tsx:1152-1161,1208` (`removeDevice` never calls
`clearSessionResume()`; the current-device button is UI-gated only) ·
`crates/client/src/engine.rs:316-328` (validate-failure clears the token, but
the identity-load-failure branch propagates with the live token installed —
resident-but-unusable, dies with the tab). No privilege escalation; everything
is bounded by sessionStorage/tab lifetime, but after a definitive 401 the raw
data key lingers in the blob for the tab's life, readable by a one-shot script
even though the session is authoritatively dead. _Fix:_ distinguish 401 from
transient errors in `resumeSession` and clear the blob; clear the installed
token on the identity-load failure branch; call `clearSessionResume()` when
`removeDevice` targets the current device.

**R17 — Web engine server-URL binds on first auth attempt (pre-auth): login DoS / URL fixation until reload.**
`crates/web-wasm/src/lib.rs:49-67,138,163,194,406-418` ·
`crates/client/src/engine.rs:104-123`. `get_or_build` installs the engine
singleton before any auth runs and there is no removal path; a later differing
URL is rejected with "Server URL is fixed for this session" (which echoes both
URLs). No crafted-link vector exists (nothing sources the URL from the page
URL); the realistic triggers are a same-origin script binding the engine to an
attacker URL on a fresh page (post-fixation logins are rejected **before** any
network call — a login DoS/phishing-setup nuisance), and — more likely — an
honest user typo on the first attempt wedging the page until reload.
OPAQUE keeps the passphrase from an evil server regardless. _Fix:_ only commit
the engine to the slot after the first _successful_ auth (or allow rebinding
while no token/session is held); drop the URLs from the error message.

### Sync & local store

**R18 — File delete tombstones are swept after one generation: a malicious server resurrects deleted files on the client's second reconnect.**
`crates/client/src/local_store.rs:606-611,745-752,766-785` ·
`crates/client/src/engine.rs:1292-1303` · `docs/ws-sync-flow.md:132-133,214-215,315`
(doc states the invariant unconditionally; code implements single-generation).
The tombstone is written with the current generation; on reconnect #1 it blocks
the malicious re-listing **without** refreshing its `seen_generation`, and the
same snapshot's sweep unconditionally deletes every stale-generation tombstone
(the `Deleted` arm has no `created_seq` guard); on reconnect #2 the re-listed
item persists as Present and reappears in the UI. Overlaps the accepted
"server can drop/omit/replay" residual but is worse for clients that verifiably
received the delete: the defense the code built self-destructs in the same pass
that relies on it. Combined with R2 the block fails on the _first_ reconnect.
_Fix:_ never sweep `Deleted` markers (they're ~50 bytes), or sweep only after a
server-attested retention horizon; at minimum refresh the tombstone's
`seen_generation` when it blocks a persist. Note `ws-sync-flow.md`'s
future-work item (clipboard delete events) will inherit this flaw.
(`docs/ws-sync-flow.md` annotated in this pass.)

**R19 — Collab create/delete mint seqs from the local clock and compare them against server-clock seqs — skew silently voids deletes.**
`crates/client/src/engine.rs:1100,1122` (local `Utc::now().timestamp_micros()`)
vs `crates/server/src/state.rs:255-270` (server seqs are server wall-clock) ·
`crates/client/src/local_store.rs:738-742` · `crates/server/src/routes/collab.rs:98,166-172,268,305`
(endpoints compute but don't return the seq) · `crates/server/src/ws.rs:440-442`
(self-originated events suppressed). A device whose clock lags the server by
more than the doc's age deletes a doc: the server delete succeeds, but the
local tombstone seq loses to the stored server create-seq and the delete
silently no-ops; retries then 404, so the doc is stuck in the local UI with no
in-app recovery. Symmetrically a creator with a fast clock ignores others'
authentic delete broadcasts. No attacker needed. _Fix:_ return the committed
`seq` from `POST/DELETE /api/collab-docs` and use it (mirroring
`delete_file`'s use of `deleted_seq`); never mint seqs from the local clock.

**R20 — Snapshot per-item failure treated as "not seen": the sweep deletes the client's last good local copy.**
`crates/client/src/engine.rs:1283,1330-1339,1355-1366` ·
`crates/client/src/local_store.rs:766-772,1175-1188` · `docs/ws-sync-flow.md:265-267`.
Any per-item error in a snapshot — hash/AEAD failure **or a transient
payload-download error or unsupported MIME under a perfectly honest server** —
skips the item via `filter_map`, so it fails the `seen_generation` predicate
and the end-of-snapshot sweep deletes its previously verified-good local
ciphertext and memory copy. A malicious server that tampers one stored
ciphertext makes every client destroy its redundant copy on the next reconnect
(self-healing only while the server's copy is intact). Meaningful surface is
clipboard items (file blobs aren't cached). _Fix:_ on per-item failure, check
for an existing local `Present` record and mark it seen for the current
generation (plus a tamper/sync warning) instead of leaving it sweep-eligible —
sweep only on confirmed absence from the list.

**R21 — Logout/device-removal doesn't fence in-flight sync writes; failed materialization never retries (doc mismatch).**
`crates/client/src/engine.rs:459-477,510-525` (logout/remove*device: no
generation bump, no task abort) · `crates/client/src/local_store.rs:222-224,226-250`
(own-upload persist has **no** generation guard at all) ·
`crates/client/src/engine.rs:1163-1171` (`publish_visible_state` has no session
check) · `engine.rs:1520-1535` (spawn-once, `warn!` on error, no retry —
contradicting `docs/ws-sync-flow.md:198-204` "The client retries while the
generation is still current"). (a) Post-logout, in-flight persists still pass
the generation guard and repopulate `AppState`; on web the UI gates on
`state.session` so nothing renders, but `getState()` (same-origin script /
daemon IPC client) reads display texts, file metadata, and collab share tokens
after teardown. No new capability vs. pre-logout access. (b) One transient
error on a live `created` event hides the item until the next reconnect —
permanently for collab (R9). \_Fix:* allocate a new local generation inside the
logout/remove_device teardown; no-op `publish_visible_state` when
`state.session` is `None`; implement bounded in-generation retry per the doc
(or fix the doc — annotated in this pass).

**R22 — Serialized OPAQUE client state is unzeroized — a memory artifact yields a KSF-free `H(passphrase)` dictionary oracle (not the data key directly).**
`crates/core/src/crypto.rs:371-374,470-473` (`start.state.serialize().to_vec()`)
· `crates/client/src/api_client.rs:252-272,373-393` (held across the HTTP
round-trip, dropped unwiped). Verifier **corrected the finder's crypto claim**:
`r + N` does **not** yield `rwd`/the data key — the OPRF finalize hash requires
the passphrase (`oprf_output = SHA-512(len(pwd)‖pwd‖len(Y)‖Y‖"Finalize")`), and
the doc comments at `crypto.rs:380,480` that omit this are misleading. The real
exposure: the serialized state contains both the OPRF blind `r` and
`M = r·H(pwd)`, so the artifact **alone** yields `H(pwd) = M·r⁻¹` — an offline
dictionary oracle at full speed (hash-to-curve per guess, **no Argon2**),
unlike the server-side oracle which is Argon2-limited. Exploitation needs a
post-hoc memory artifact (swap/hibernation/core dump/post-compromise forensics)
plus a guessable passphrase. The in-struct copies inside opaque-ke are
`ZeroizeOnDrop`; only Clipper's serialized Vec is unwiped. _Fix:_ return
`Zeroizing<Vec<u8>>` (call sites treat the value opaquely) or keep the typed
structs in-crate; fix the misleading doc comments.

**R23 — Non-zeroized key-material copies: device signing key unwrap, data-key stack copy across hydration, export-key stack remnant, JSI/React passphrase copies.**
`crates/core/src/crypto.rs:324-330,627-656` (`unwrap_with_key`→`decrypt`
returns plain `Vec<u8>`) · `crates/client/src/local_store.rs:1735-1754` (32-byte
secret copied out; source Vec dropped unwiped — `decode_resume_key` at
`crates/web-wasm/src/lib.rs:427-437` shows the codebase knows the pattern) ·
`crates/client/src/engine.rs:380-428` (`cache_key = *encryption_key` held
across `.await`, contradicting `docs/local-at-rest-encryption.md:41-43`) ·
opaque-ke 4.1.0-pre.2 `ClientLoginFinishResult` derives `Clone` only — the
**export key** (root of both child keys) remains as a stack remnant
(`crypto.rs:511-515`) · `UniffiString.h:43-64`/`Bridging.h:21-30` (JSI crossing
copies never wiped) · `mobile/src/App.tsx:169,184-198` (passphrase kept in
React state, never cleared post-login). All require a pre-existing
memory-disclosure capability (root, swap/dump, renderer exploit); the
passphrase-at-boundary variants partially overlap the accepted residual in
`docs/security-review.md:293-298`. _Fix:_ `Zeroizing` returns from
`decrypt`/`unwrap_with_key`; wrap `cache_key`; clear the RN passphrase state in
the login `finally`.

### Desktop daemon & Tauri

**R24 — Legacy plaintext `.payload`/`.txt` clipboard sidecars are never swept.**
`crates/client/src/local_store.rs:1059-1085,1108-1119,1166-1170` (current code
writes only `.payload.ciphertext`; the hydrate orphan-sweep is suffix-scoped to
`.payload.ciphertext`; legacy names are only removed on object-delete paths).
Machines that ran pre-`31491b2` dev builds (June 2026) retain plaintext
clipboard contents for every item not since deleted — same-uid/disk-access
exposure, already `0600` under `0700` dirs, undeployed-app scope. _Fix:_
wholesale unlink `*.payload`/`*.txt` in the hydrate sweep.

**R25 — Device-identity record: cleartext `device_id` unauthenticated (constant wrap AAD); invalid records silently re-minted.**
`crates/client/src/local_store.rs:1487-1493,1713-1717,1731-1739` (wrap AAD is a
constant, `crates/core/src/crypto.rs:685`; `device_id` is cleartext and only
UUID-validated) · `local_store.rs:982-996,1241-1249` (errors other than
decrypt/version failure hit "Replacing invalid local device identity" and
re-mint) · `crates/server/src/routes/auth.rs:729-835` (a swapped-in fresh UUID
is **not** rejected — the server mints a new device row with the original
public key). A same-origin script (localStorage write) or same-uid process that
rewrites `device_id` causes silent, undetected device-identity migration on the
next login; each flip burns a device slot until the `max_devices` cap rejects
logins (persistent login DoS); a non-UUID value discards the registered
identity entirely. The attacker never gets the key — integrity/availability
tamper only. `docs/local-at-rest-encryption.md` flagged the cleartext field as
a _confidentiality_ note but missed the _integrity_ consequence (corrected in
this pass). _Fix:_ bind `device_id ‖ profile_id` into the wrap AAD (bump the
record version) and fail closed on `InvalidDeviceId` instead of re-minting.

**R26 — Daemon IPC byte fields serialize as JSON number arrays; clipboard payloads ≳9 MiB fatally wedge the Tauri↔daemon connection.**
`crates/daemon-types/src/protocol.rs:141-145,198-203` (plain `Vec<u8>` derives)
· `crates/daemon/src/handler.rs:35,97-108,351-353` (32 MiB line cap; on
overflow the daemon emits an uncorrelated error and **drops the connection**) ·
`web/src-tauri/src/daemon_client.rs:70,131-156,317-318` ·
`crates/client/src/engine.rs:42,47`. serde*json emits `Vec<u8>` as
`[104,101,…]` (~2–4 chars/byte), so engine-legal payloads (≤16 MiB) above ~9
MiB of typical binary content exceed the line cap in both directions; each
attempt force-reconnects the client. Note for any R11 fix: `serde_bytes` alone
does **not** change serde_json output — base64 or binary framing is required.
Self-inflicted (a malicious server can't forge the triggering ciphertext).
\_Fix:* base64-encode bulk fields (or length-prefixed binary frames), align the
caps, and have the daemon reject oversized lines without killing the
connection.

**R27 — Slow broadcast consumer evicted as "dead": `try_send` Full treated identically to Closed.**
`crates/daemon/src/clients.rs:28,44-56` · `crates/daemon/src/handler.rs:130-141,56-66` ·
`crates/daemon/src/main.rs:220-225,306-317` (every state change broadcasts the
entire `AppState` JSON). During initial bulk sync or a clipboard storm, a
64-deep per-client queue overflows and the healthy Tauri client is
force-disconnected; recovery is automatic (backoff ≤5 s, fresh full-state
snapshot on reconnect), so impact is a transient outage + in-flight command
failures, worst exactly when sync is busiest. _Fix:_ reap only on `Closed`; on
`Full` drop the oldest queued event (state events are latest-wins snapshots) or
use a `watch` channel.

**R28 — `CLIPPER_DAEMON_SOCKET_PATH` override chmods its parent dir `0700` unconditionally.**
`crates/daemon-types/src/ipc_path.rs:13-19,40-71,134-138` ·
`crates/daemon/src/main.rs:253-255` · `web/src-tauri/src/daemon_spawn.rs:11-17`
(no `env_clear`). `ensure_private_socket_dir` runs on whatever the override's
parent is — e.g. `CLIPPER_DAEMON_SOCKET_PATH=$HOME/daemon.sock` chmods `$HOME`
to 0700 on every start, breaking group-shared dirs. The euid check blocks
cross-user chmod; self-config footgun only. _Fix:_ only `set_permissions` when
the daemon created the leaf dir itself (or require an override parent to
already be 0700 and fail closed).

**R29 — Tauri production CSP blocks the collab WebSocket and share-page fetch — collab dead in packaged desktop builds.**
`web/src-tauri/tauri.conf.json:30` (`connect-src: 'self' ipc: http://ipc.localhost`)
vs `:44-50` (devCsp adds `127.0.0.1` — which is why dev works and release
breaks) · `web/src/CodeEditor.tsx:98-103` · `web/src/App.tsx:1073` ·
`web/src-tauri/src/lib.rs:22,157-159` (even the default `ws://127.0.0.1:8787`
is blocked, since the webview origin is `tauri://localhost`). Verified against
the vendored tauri 2.11.3 CSP-injection mechanism; policies intersect, so the
Tauri `connect-src` wins. Not exploitable — a security control silently
breaking a shipped feature (and the tempting wrong fix, `connect-src *`, would
over-open the webview). _Fix:_ add the deployment API origin (today
`http://127.0.0.1:*` + `ws://127.0.0.1:*`) to `security.csp.connect-src`;
longer-term route collab traffic through the daemon like everything else.

### Web & wasm

**R30 — `uploadFileBytes` has no client-side size cap; the wasm upload path triple-buffers whole files → tab OOM.**
`web/src/App.tsx:596-602` (whole `file.arrayBuffer()`, no `file.size` check) ·
`crates/web-wasm/src/lib.rs:315-324` (`bytes.to_vec()`, second copy) ·
`crates/client/src/engine.rs:879-993` (`encrypt_file_blob_bytes` → third copy;
no ceiling — clipboard is capped at 16 MiB with the exact rationale comment at
`engine.rs:552-562`, file _download_ is capped at 512 MiB, only upload is not).
Peak ≈ 3× file size; a multi-GB pick crashes the renderer or traps the wasm
instance (panic=abort) mid-upload, wedging the memoized backend until reload
and leaving a half-initialized object server-side. Native/mobile also
whole-buffer (uncapped), so the missing cap is cross-platform. User-triggered
only. _Fix:_ `MAX_FILE_UPLOAD_BYTES` check at the top of
`upload_file_bytes`; check `file.size` before `arrayBuffer()`; long-term,
stream-encrypt.

**R31 — Server-controlled error text flows verbatim into client UI banners and daemon logs.**
`crates/client/src/api_client.rs:857-869,1027-1028,1157-1159` (the JSON error
path is verbatim; only the non-JSON fallback is escaped — the codebase
half-addressed this; 256 KiB bound) · sinks: `web/src/App.tsx:426`,
`mobile/src/App.tsx:257,373` (React/RN text nodes — escaped, no injection),
daemon `daemon.log` (`crates/daemon/src/main.rs:234-248`). A malicious server
can render arbitrary persuasive text in the trusted red error banner
("Session expired — re-register at …") on all three clients, and inject
newlines/ANSI escapes into the 0600 log file (log forging, terminal escapes on
`cat`). Correction to the finders: the text does **not** flow into
`AppState.error` (never assigned; only JS-side `useState`). _Fix:_ sanitize the
server `message` at `api_error_from_response` (strip control chars); prefer
mapping `ApiErrorCode` to fixed client-side strings.

**R32 — Raw UniFFI JSI module on `globalThis` gives in-bundle JS native-memory primitives.**
`packages/mobile-bridge/cpp/generated/clipper_mobile_uniffi.cpp:2904-2931,3026,3042,3222-3236` ·
`UniffiString.h:21-29,95-98` (no `offset+length <= size` check → OOB native
heap read into JS strings) · `rustbuffer_free` trusts a JS-settable
`__ubrnRustCapacity` (wild/double free; Rust really deallocs by capacity —
`uniffi_core` rustbuffer.rs:171-179; the logic is project-side codegen, not
upstream) · forged bigint handles → `Arc` refcount at attacker-chosen addresses
· missing `count` guards (`args[count-1]` = `args[-1]` when `count == 0`). The
only way third-party JS reaches the RN runtime is a malicious npm dependency in
the bundle — verified no WebView, no OTA (`expo-updates` disabled), no
eval/remote-bundle loading in the app. Such a dependency gets an in-process
escalation reaching the Rust-side data key that by design never crosses the
bridge. _Fix:_ bounds-check the string reads; track buffer provenance instead
of trusting JS-writable capacity; add `count` guards; keep the raw module off
`globalThis`.

**R33 — Lifecycle: duplicate `ws_loop`s from unguarded auth transitions; no mobile engine teardown; racy JS-teardown callback.**
`crates/client/src/engine.rs:413-418` (`finish_auth` spawns `ws_loop`
unconditionally; no already-logged-in guard; loops exit only on logout, so a
logout→login inside the ≤60 s backoff window resurrects the old loop into the
new session — production-reachable) · web: `web/src/main.tsx:16-22` (StrictMode
double-resume is dev-only) · mobile: `packages/mobile-bridge/src/adapter.ts:63-70`
(client swap with no shutdown — but verified the abandoned pre-login engine is
provably inert), `crates/mobile-uniffi/src/lib.rs:48-221` (no shutdown export;
a dev JS reload leaves the old engine Arc-pinned over the same data dir) ·
bridge: `clipper_mobile_uniffi.cpp:582,612-629,650,669-673` (global
`std::function` written on the JS thread, called from tokio threads with an
unsynchronized check-then-call; `invokeNonBlocking` captures `&rt` by
reference; `cleanupRustCrate` is a no-op stub — a Metro reload/runtime recreate
can crash the app natively). Integrity is genuinely bounded (generation guards

- 32-conn server cap); availability-only. _Fix:_ auth-epoch-guarded `ws_loop`s;
  an engine `shutdown()` wired into client swap and RN `invalidate()`;
  mutex/atomic-guard the callback and implement `cleanupRustCrate` for real.

**R34 — Collab editor destroyed and rebuilt on every parent re-render (inline `collab` prop defeats effect deps).**
`web/src/App.tsx:1037-1044,1107-1114` (inline object literals) ·
`web/src/CodeEditor.tsx:154,177-182` (deps compare by identity; rebuild does a
full `ytext.toString()` and resets the `Y.UndoManager`). Any background sync
event (or clicking "Copy link") destroys the focused CodeMirror view
mid-typing: focus/selection/scroll lost, undo history silently reset, whole
document re-stringified (compounds R10's cost). Y.Doc and the provider survive
(their effect deps primitives). _Fix:_ memoize the config at both call sites,
or drop `collab` from the editor effect deps in favor of its primitive fields.

**R35 — y-websocket retries the collab WS forever after a 403; deleted docs linger as ghost rooms; deletion doesn't kick connected peers.**
`web/node_modules/y-websocket/src/y-websocket.js:135-168,277,105-106` (reconnect
on any close, no close-code inspection, ≤2.5 s cap) ·
`crates/server/src/routes/collab.rs:223-306,398-403` (delete removes rows but
never evicts `state.collab_rooms`; invalid token/doc → 403 at upgrade) ·
`crates/server/src/collab_sync.rs:252-260,466-470` (post-delete persists are
silent 0-row no-ops) · `web/src/CodeEditor.tsx:92-128,150-158` (no
status/connection listeners; an unsynced doc is an editable editor
indistinguishable from a live one). After a doc is deleted, every guest's
client retries the WS forever per tab (re-sending the token in the query each
time — compounding A6), shows a normal editor, and silently discards anything
typed; peers still connected at deletion time keep editing the in-memory ghost
room and receiving each other's updates — **deletion does not actually revoke
access for connected parties**. _Fix:_ client-side, re-validate the doc via the
meta endpoint after a few failed reconnects and render "document unavailable";
server-side, evict the room and close its connections on delete.

**R36 — Wayland privacy-marker TOCTOU: no post-read re-check lets a marked secret slip into sync history.**
`crates/client/src/clipboard_watcher_linux.rs:206-278,295-344` (marker check,
hint-value read, and payload read are **three** independent roundtrips, each a
fresh Wayland connection in wl-clipboard-rs — no offer-identity pinning is
possible at the crate level) · contrast `clipboard_watcher_macos.rs:121-130`
(explicit post-read re-validation; the macOS manual read lacks it too). The
realistic scenario is accidental: a password copied from KeePassXC lands in the
~1–10 ms gap between the marker and payload roundtrips within one 500 ms poll
tick (~0.2–2% per copy), and the daemon then syncs the "clean" secret to all
the user's devices — exactly what the `x-kde-passwordManagerHint: secret`
contract exists to prevent. Content stays E2E-encrypted; no external party
gains anything. _Fix:_ after the payload read, re-fetch the MIME list and
re-run the marker check, dropping the capture if it changed (the macOS
pattern).

**R37 — Transport robustness: browser WS inbound uncapped; no heartbeat or read deadline anywhere; no HTTP total deadline; `Host` header lacks the port.**
`crates/client/src/engine.rs:2006,2016-2025` (wasm: every inbound frame copied
into an unbounded mpsc — vs native's tungstenite default 64 MiB cap; the 64 KiB
server cap is client→server only) · `engine.rs:1757,1771-1853,1916-1972,2058-2070`
(no handshake timeout, no client ping, no read deadline — a server that accepts
TCP and goes silent leaves the client stuck "Connecting"/"Connected" forever;
no TCP keepalive either) · `crates/client/src/api_client.rs:763-776`
(`connect_timeout` + per-read `read_timeout` only; a ≥1-byte-per-29 s dribble
holds any request forever on native; the wasm client has **no timeouts at
all**) · `engine.rs:1739,1746` (`Host` built without the port — RFC 7230 nit,
breaks nothing in practice). All availability-only, malicious-server gated.
_Fix:_ close the browser socket on frames above a few KB (all legit messages
are sub-KB); handshake timeout + client-initiated ping with a read deadline;
total deadline on non-payload requests; build `Host` as `host:port` when
non-default.

---

## Part II — Info findings

**R38 — Device Ed25519 key signs two message types without domain separation (not exploitable).**
`crates/core/src/crypto.rs:122-139` signs both `ObjectEnvelopeBodyV1` and
`DeviceLoginProofBodyV1` over raw postcard bytes with no domain tag. The
finder's collision argument was wrong (byte 0 _can_ coincide), but the verifier
proved confusion infeasible for a sharper reason: the signed login proof's
postcard tail is fixed `0x20 ‖ device_id(17B) ‖ 0x20 ‖ pk_D(32B)` while the
envelope's tail is `0x20 ‖ SHA256(ciphertext)` — a byte-collision forces a
SHA-256 preimage; and even a hypothetical collision gains nothing, since
sibling clients AEAD-verify under the data key the server lacks. Worth fixing
as hygiene **now** (pre-deployment, no compat burden): prepend
`b"clipper:device-login-proof:v1"` / `b"clipper:object-envelope:v1"` to the
signed bytes, matching the AAD discipline already used at `crypto.rs:199-200`.

**R39 — Client signs the server-chosen `device_proof_challenge` verbatim (bounded signing oracle, no exploitable confusion).**
`crates/client/src/api_client.rs:296-309` signs `challenge_id`/`challenge`
verbatim from the server with no length/format check (garde runs only
server-side). A malicious server gets a signature over a body whose two
challenge fields it fully chose (up to ~64 MiB — a harmless self-DoS); every
cross-protocol abuse path is independently blocked by the server's hash
commitments (`api-types/lib.rs:805-829`, `objects.rs:671-696,1610-1611`) and
context binding (`objects.rs:1592-1597`). _Fix:_ reject unless
`challenge.len() == 32` and `challenge_id.len() <= 64` before signing.

**R40 — Session identity fields not cross-checked at login/resume.**
`crates/client/src/engine.rs:190,209,220-221,252,270,281-282` (adopts
server-returned `device_id`/`username` unconditionally) ·
`engine.rs:316-336,390-394` + `crates/server/src/routes/auth.rs:520-523`
(`validate` discards `AuthInfo` and returns bare `Ok` — no token↔account↔device
correspondence check; resume mounts whatever identity the JS blob names). A
malicious server can silently fragment the device list / spoof the displayed
username; a same-origin script can mount a foreign account's token into a
wedged session. No key/plaintext/privilege gain — strictly weaker than what
those actors already have. _Fix:_ error on `requested_device_id`/username
mismatch at login; have `/api/auth/validate` return `{user_id, device_id,
username}` and reject mismatches at resume.

**R41 — Unvalidated on-disk record id used in deletion paths; i64 overflow in cached blob-size sum.**
`crates/client/src/local_store.rs:143-149,1132-1158,1175-1209` (record body
`id` — not the filename — feeds path builders; `Path::join` accepts `/`, `..`,
and absolute paths) and `local_store.rs:1542-1548` (plain `i64::sum()`; debug
panics, release wraps — no `overflow-checks` override exists). All public write
paths UUID-validate ids (`validate_item_id`, tested), so no in-model attacker
can plant such a record; this is a latent primitive for any future less-trusted
writer. The summed outer `payloads[].ciphertext_size` is _not_ AEAD-bound.
_Fix:_ validate the id (or fall back to the filename) in
`remove_stored_object_record_and_payloads`; use a saturating fold.

**R42 — Tauri Linux IPC-secret read lacks the daemon's symlink/regular-file guard.**
`web/src-tauri/src/ipc_secret.rs:44-56` (bare `std::fs::read`) vs
`crates/daemon/src/keychain.rs:255-262,289-301` (`symlink_metadata` guard). The
only attacker who could plant the symlink (same uid) can already read the 0600
secret directly, so this grants no new capability — a desync/DoS nuance inside
the accepted same-user boundary. _Fix:_ share `reject_non_regular_existing_file`
via `crates/daemon-types`.

**R43 — SharePage casts the `/s/{token}/meta` JSON without validating `object_id`.**
`web/src/App.tsx:1081-1085` (unchecked cast; failure modes are caught and
rendered as escaped text) · `web/node_modules/y-websocket/src/y-websocket.js:403-407`
(room name concatenated unencoded — can only mangle the path on the
already-configured server). Server-controlled value only; no exploitable sink.
_Fix:_ one-line type check before `setInfo`.

**R44 — `materialize_collab` / `get_collab_doc_meta` don't verify the returned object id.**
`crates/client/src/engine.rs:1139-1154,1637-1658,2386-2393` — same missing
identity check as R6, for collab metadata (a per-doc view can display a
different doc's share token/`updated_at`). Marginal impact is capped by A5
(collab content is server-visible/server-mutable by design). _Fix:_ same
one-line id equality check.

**R45 — Dependency findings (osv-scanner, `nix run .#audit`, this pass).**
`anyhow` 1.0.102 → **RUSTSEC-2026-0190** (fixed in 1.0.103; trivial `cargo
update -p anyhow`). `quick-xml` 0.39.4 → **RUSTSEC-2026-0194** and
**RUSTSEC-2026-0195** (CVSS 7.5; fixed in 0.41.0) — reaches the client only via
`plist` → `tauri 2.11.3` (desktop shell); exposure is limited to local
plist/entitlements parsing, so real-world risk is low, but track a Tauri bump.
Also noted: the core PAKE is pinned to a **pre-release** crate,
`opaque-ke 4.1.0-pre.2` (`Cargo.lock`) — fine while pre-deployment, but pin to
a stable release before shipping. (20 further advisories were already filtered
by `osv-scanner.toml`, mostly the known unmaintained GTK3 transitive stack.)

---

## Part II — Refuted claims

Independently verified as **not** true — recorded so they are not re-raised:

- **"The production web bundle ships a dev-profile (debug) wasm build"** —
  REFUTED. `wasm-pack build` defaults to `--release` (no `--dev` flag is ever
  passed in `web/package.json:6`); the on-disk artifacts confirm it
  (`web/src/generated/wasm/*.wasm` matches the release target output, ~1.4 MB
  wasm-opt-optimized, no overflow-check strings, no debug sections). Panic
  messages with file paths in a release binary are normal Rust behavior, not
  evidence of a debug build. (Hygiene only: add an explicit `--release` for
  self-documentation.)
- **"Mobile `AppState` clipboard text is unbounded (16 MiB × 100) and crosses
  JSI on every state bump"** — REFUTED. The persist layer re-derives display
  text from the decrypted payload via `bounded_text_preview`
  (`CLIPBOARD_TEXT_PREVIEW_MAX_CHARS = 512`, `local_store.rs:28,1613-1676`) on
  **every** path — WS live, snapshot, local echo, hydration — with a dedicated
  regression test (`derives_bounded_preview_without_trusting_caller_text`,
  `local_store.rs:2280-2324`). Worst case over JSI ≈ 51 KB, not ~1.6 GB. The
  engine's full-text `clipboard_display_text` is dead code no consumer reads.

---

## Part II — Verified safe (negative results)

Re-verified this pass and found **sound** (the crypto core was the primary
distrust target of this re-audit; all claims below were re-read against code,
docs, and vendored dependency sources):

- **AEAD:** XChaCha20-Poly1305 with a fresh random 24-byte nonce generated
  _inside_ `encrypt` per call (`crates/core/src/crypto.rs:603-624`); random
  192-bit nonces make per-key reuse a ~2^96 birthday non-issue; `decrypt`
  rejects wrong-length nonces before touching the cipher and returns `Err` on
  tag failure before yielding any plaintext.
- **HKDF separation:** distinct labels
  (`clipper:opaque-export:data-key:v1` / `…:device-identity-wrap-key:v1`,
  `crypto.rs:26-28,293-299`), matching `docs/opaque.md` and
  `docs/object-envelopes.md`; test asserts the keys differ.
- **AAD binding:** `ObjectAadV1` (`crypto.rs:193-225`) binds the role domain,
  `object_id`, `object_type`, `object_version`, `source_device_id`,
  `created_at`, `operation`, and payload ids; all eight client meta/payload
  encrypt/decrypt helpers route through these builders — no call site forgets
  the AAD. Nonce/size/hash exclusion is deliberate and sound (pinned by the
  envelope signature + explicit SHA-256 re-checks + the AEAD tag).
- **Verify-before-release ordering:** envelope cross-checks + signature run
  before any decrypt at every entry point (clipboard `engine.rs:1457-1463`,
  file meta `engine.rs:2366-2372` with a payload-count DoS guard, file blob
  `engine.rs:999-1011`); the local-cache path re-checks size+SHA-256 before
  AEAD (`local_store.rs:1636-1651`).
- **OPAQUE:** `ClipperOpaqueCipherSuite` = Ristretto255 + TripleDH + SHA-512 +
  Argon2id (`crypto.rs:36-42`), matching `docs/opaque.md`; fake-record
  anti-enumeration present (`crypto.rs:546-552`); `server_mac` verified inside
  opaque-ke `finish`; export key never transmitted; the opaque-ke client state
  structs are `ZeroizeOnDrop` (only Clipper's _serialized_ copies are unwiped —
  R22).
- **RNG:** all key/nonce/salt/token generation funnels through CSPRNGs
  (`crypto.rs:66-86`; `rand` 0.10 ThreadRng; OPAQUE internals on `OsRng`;
  browser pinned to `getrandom 0.4.3` with `wasm_js`). No
  `thread_rng`/`SmallRng`/`from_entropy` anywhere in `crates/`.
- **A1 fix completeness (web):** no passphrase or OPAQUE-replayable secret
  remains in any web storage post-fix (only `clipper.session.v2` token + leaf
  keys in sessionStorage; ciphertext + wrapped identity in localStorage; a vim
  flag). Residuals documented in R14/R16.
- **Login-proof ↔ envelope signature confusion:** blocked by a required
  SHA-256 preimage and, independently, by AEAD under a key the attacker lacks
  (R38/R39).
- **Documented residual (unchanged):** envelope signatures verify against
  server-supplied device public keys with no client-side pinning — the real E2E
  authenticity mechanism is AEAD+AAD under the export-key-derived data key.
  This is explicitly documented in `docs/object-envelopes.md:194-244`, so it is
  a design statement, not a hidden flaw.
- **DOM/XSS surface:** re-derived Part I's conclusion — no new injection sink
  found; A4 (R12) remains the only markup-adjacent issue.
- **Tauri webview secrets invariant:** no Tauri command returns the bearer
  token, keys, or the IPC HMAC secret to webview JS (Part I positive, still
  true). Note the desktop analogue of A3 applies as a design property: webview
  JS can drive every Tauri command including decrypt-on-demand — the same
  confused-deputy model as A3, so webview XSS prevention (A2/CSP) is the
  control that matters on desktop too.

---

## Part II — Coverage notes

- **Finder→verifier corrections were frequent and material** (mechanism
  corrections on R18, R22, R26, R38 and both refuted claims, plus severity
  re-grades both up and down) — the two-stage design caught errors a
  single-pass audit would have published, including one wrong crypto claim
  produced by _this_ pass's finders.
- **Not deeply pursued (follow-ups):** fuzzing the postcard/serde decoders and
  the UniFFI JSI marshalling under crafted inputs (boundary-reviewed only);
  server-side rate-limit/capacity review (out of scope); a packaged-build smoke
  test of the Tauri CSP (R29 was verified from config + vendored tauri source,
  not by running a release bundle).
- **Raw finder/verifier artifacts** (83 candidates, 46 verdicts with full
  evidence trails) were produced in-session; the adjudicated content is fully
  reflected above.

## Part II — Prioritized recommendations

1. **R1 — wire the v2 resume material to mobile** (UniFFI `resume` export) and
   stop persisting the passphrase in the OS keystore. Highest leverage:
   removes the last revocation-proof root at rest.
2. **R2 — validate ingested seqs against `hello_ack.server_time`** (+grace) and
   make the `created`-for-Present arm a pure duplicate. One small check kills
   the delete-immunity primitive.
3. **R3/R4/R5 — reconciliation bounds:** cursor-progress check + page cap;
   dedup pending creates (absent→pending transition only) + a materialization
   semaphore; skip payload downloads for unchanged `(id, created_seq)` records
   and stop resetting backoff on `Invalidate`.
4. **R6 — pin the requested object id** in `download_file_bytes` (and R44's
   collab equivalents). One line each.
5. **R7/R8 — web robustness:** compact wasm sidecar encoding (or IndexedDB) +
   non-fatal per-item snapshot persists; `web_time::Instant` + a wasm smoke
   test. Both are user-facing breakages today.
6. **R15/R16 — act on 401** (session teardown + blob cleanup) so device
   revocation has a local effect; purge `clipper.session.v1` at startup.
7. **R9/R10/R12/R35 — collab hardening:** list endpoint + reconciliation;
   cumulative doc-size + rate limits; awareness validation at the provider
   boundary; room eviction + client-visible failure on delete/403.
8. **R11 — remove the desktop temp-file staging** (bytes-over-IPC with proper
   framing, which also fixes R26).
9. **R13 — Android:** `FLAG_SECURE` (+ recents flag on API 33+), download-cache
   cleanup, `IS_SENSITIVE`, drop `clipper://` and `SYSTEM_ALERT_WINDOW`.
10. **Hygiene batch (cheap, pre-deployment):** R18 tombstone retention, R19
    server-returned seqs, R20 sweep-on-absence-only, R21 generation fencing on
    logout, R22/R23 `Zeroizing` returns, R25 AAD-bind `device_id`, R29 Tauri
    CSP `connect-src`, R30 upload cap, R31 error-text sanitization, R32 JSI
    bounds checks, R33 auth-epoch + engine shutdown, R34 memoized collab prop,
    R36 post-read marker re-check, R37 WS caps/timeouts, R38/R39 domain prefix
    - challenge length checks, R45 `cargo update -p anyhow` (+ track the Tauri
      bump for quick-xml, and pin opaque-ke to a stable release).
