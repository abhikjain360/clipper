# Known issues and open decisions

Every known bug, security issue and undecided product question, one entry
each. An entry stays here until it has a decision: fix, accept, or drop. Fixed
entries keep their commit so the fix can be checked.

Each entry has:

- **Status:** open, fixing, fixed (with the commit), or decided.
- **Severity:** high, medium or low, judged by how likely it is and what it
  costs the user. "On main" means `main` had the same defect before this
  branch.
- **Decision:** what to do about it. "Fix (Claude)" means Claude chose to fix
  it without asking; reverse it if you disagree.

## Security

### 1. A malicious server can swap the revision a device accepted, through its own delete reply

- **Status:** fixed in `f6c55b2`
- **Severity:** high (needs a malicious server). Not on main.
- **Where:** `crates/client/src/local_store.rs`, `apply_delete_inner`.
- **What happens:** when this device deletes an object, it waits for the
  server's reply. If an unsigned "deleted" event with the same sequence number
  arrived first, a shortcut upgrades the stored anchor to the device's own
  tombstone after comparing only revision numbers, not body hashes. A server
  that first delivered another device's competing revision 2 can replace that
  accepted revision 2 with the tombstone's, and then get revision 3 built on
  it accepted. The server cannot forge content, but it can choose which of the
  user's genuine versions this device follows.
- **Decision:** fix (Claude).

### 2. Two downloads running at once can accept two different bodies for one revision

- **Status:** fixed in `f6c55b2`
- **Severity:** high (needs a malicious server). Not on main.
- **Where:** `crates/client/src/engine.rs`, `retain_downloaded_file`.
- **What happens:** when a download finishes, retention skips the anchor check
  if the device already holds that revision number or a newer one. It never
  compares hashes, so a second download of a different revision 2 is
  accepted, and an older revision is accepted after a concurrent advance.
- **Decision:** fix (Claude).

### 3. A download that finishes after logout still returns the old account's file

- **Status:** fixed in `f6c55b2`
- **Severity:** high. Not on main (this return path is new).
- **Where:** `crates/client/src/engine.rs`, `retain_downloaded_file` and
  `download_file_bytes`.
- **What happens:** account A starts a large download, logs out, and B logs
  in. Retention notices the session changed and skips saving the record, but
  reports success, so the decrypted bytes reach the caller. On desktop they are
  written to the folder A chose.
- **Decision:** acceptable on desktop, where A chose the folder (owner). The fix
  stays for now because on the web the file is saved into the next user's
  browser session; revert on request.

### 4. A clipboard push spanning a login uses two accounts' credentials

- **Status:** fixed in `f6c55b2`
- **Severity:** medium. On main.
- **Where:** `crates/client/src/engine.rs`, `send_clipboard_payload`.
- **What happens:** the push reads A's data key, then after a logout and login
  signs with B's device key and sends B's token. B's account gets an object
  that only A's key opens.
- **Decision:** fix (Claude): read every credential at once and refuse to
  send if the session changed.

### 5. A screen update built for one account can be shown to the next

- **Status:** fixed in `f6c55b2`
- **Severity:** low in practice. On main.
- **Where:** `crates/client/src/engine.rs`, `publish_visible_state`.
- **What happens:** a sync task builds the list of visible items for account
  A. If it stalls before showing it, across a logout and B's whole login, it
  shows A's items on B's screen until B's data finishes loading. Two reviewers
  reproduced it by holding the task by hand; ordinary scheduling does not
  stall a task for the length of a login.
- **Decision:** ignore for now (owner). The fix landed with the other session
  fixes in `f6c55b2` and is kept.

### 6. Logging in installs the new token before the old session is stopped

- **Status:** fixed in `f6c55b2`
- **Severity:** low in practice. On main.
- **Where:** `crates/client/src/engine.rs`, `resume_with_platform`;
  `crates/client/src/api_client.rs`, `login_finish`.
- **What happens:** an in-flight sync from account A sends B's token and
  stores B's collab doc titles and share links in A's local store. It needs a
  login or resume while A is still signed in, which the web and mobile apps do
  not offer today.
- **Decision:** fix (Claude): stop the old session before installing the new
  token.

### 7. A file listed as clipboard is installed as a clipboard item

- **Status:** fixed in `3ccc245`
- **Severity:** low (needs a malicious server). On main.
- **Where:** `crates/client/src/engine.rs`, `snapshot_clipboard` and the other
  snapshot loops.
- **What happens:** the snapshot asks the server for clipboard items but never
  checks each item's kind. A server can list the user's `notes.txt` there; the
  file leaves Files and appears in clipboard history.
- **Decision:** fix (Claude): check the kind in every snapshot loop.

### 8. A crafted RRULE name still gets past the numeric check

- **Status:** fixed in `21f1cc5`
- **Severity:** medium. Not on main.
- **Where:** `crates/schedule/src/ingest.rs`, `validate_rrule_numbers`.
- **What happens:** the check ends a property name at `;` or `:`, but the
  parser also ends it at `,` or `=`. `RRULE,X:FREQ=DAILY;INTERVAL=65538`
  imports as every 2 days, and `COUNT=4294967296` as a series that never ends.
- **Decision:** fix (Claude).

### 9. A feed of empty comma-separated values costs about 470 MB to parse

- **Status:** fixed in `21f1cc5`
- **Severity:** medium. Not on main.
- **Where:** `crates/schedule/src/ingest.rs`, the pre-scan in
  `parse_calendar`.
- **What happens:** an 8 MiB feed of lines like `CATEGORIES:,,,,` allocates
  one value per comma. The import is accepted, so every client, including the
  browser, re-parses it.
- **Decision:** fix (Claude): cap the number of values before parsing.

### 10. A synced item with an extreme date crashes the calendar

- **Status:** fixed in `21f1cc5`
- **Severity:** low (only a record signed by one of the user's own devices).
  Not on main.
- **Where:** `crates/schedule/src/time.rs`, `resolve_local`;
  `crates/schedule/src/recurrence.rs`, `until_scan_bound`.
- **What happens:** a date in year -262,143 or +262,142 makes expansion
  panic. On the web a panic kills the app, on every load.
- **Decision:** fix (Claude): refuse dates outside years 1 to 9999 and use
  checked arithmetic.

### 11. The Android alarm receiver logs the alarm's title in plaintext

- **Status:** decided
- **Severity:** low. Not on main.
- **Where:** `mobile/modules/clipper-alarm/.../AlarmReceiver.kt`, `onReceive`.
- **What happens:** each fired alarm writes its title to logcat. Logout
  cannot remove it; it stays until the log rotates or the phone reboots.
- **Decision:** accepted as a known issue for now.

### 30. The server has no header timeout, request deadline or connection cap

- **Status:** open; checked in code
- **Severity:** medium. On main.
- **Where:** `crates/server/src/main.rs`, the bare `axum::serve` call; the
  streamed payload upload in `crates/server/src/routes/objects.rs`.
- **What happens:** hyper waits for request headers with no deadline and Tokio
  accepts any number of connections. The rate limiters run only after headers
  are parsed, so a client that sends headers one byte at a time is never
  counted. This works without an account against the auth routes, and
  `/api/health` has no rate limit at all. The upload stream has no per-chunk
  read timeout.
- **Recommendation:** add a header-read timeout, a connection or concurrency
  cap, and either a short whole-request deadline on auth and health or a
  per-chunk upload timeout. If a reverse proxy is to do this instead, state it
  as a deployment requirement in `server-resource-limits.md`.
- **Decision:**

### 31. Authenticated routes rely on axum's implicit 2 MiB body limit

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `crates/server/src/main.rs`, the authenticated router (only the
  public auth router sets `DefaultBodyLimit`); the `Postcard` extractor in
  `crates/server/src/routes/mod.rs`; `init_object`.
- **What happens:** the whole inline object body is read into memory before any
  size check. An inline payload above about 2 MiB gets a generic 400 instead of
  `PayloadTooLarge`, and setting `max_object_meta_ciphertext_bytes` above 2 MiB
  has no effect.
- **Recommendation:** decide whether inline payloads above 2 MiB are supported,
  then set an explicit limit on the authenticated router that matches the
  configuration, so clients get `PayloadTooLarge`.
- **Decision:**

### 32. The per-user API limit counts requests, not bytes

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `crates/server/src/rate_limit.rs`, `check_api_user`; the payload
  routes in `crates/server/src/routes/objects.rs`.
- **What happens:** a 512 MiB download costs the same as a one-byte request.
  Stored bytes are capped by the storage quota, but repeated downloads and
  upload-then-delete churn are limited only by request count.
- **Recommendation:** charge payload routes by bytes, or state that bandwidth
  limits belong to a reverse proxy.
- **Decision:**

### 33. Native clients have no cap on stored objects

- **Status:** open; checked in code
- **Severity:** low (needs a malicious server). On main.
- **Where:** `crates/client/src/local_store/sqlite.rs` (the browser store caps
  its index at 1,000; the native store has no cap).
- **What happens:** a server can announce or list any number of objects, and
  each one becomes a row on the device.
- **Recommendation:** decide a cap and what happens at it: drop the oldest or
  refuse new objects.
- **Decision:**

### 34. Reconciliation has no total page cap and does not stop a replaced pass

- **Status:** open; checked in code
- **Severity:** low (availability against a malicious server). On main.
- **Where:** `crates/client/src/engine.rs`, the snapshot loops.
- **What happens:** each page must now advance the cursor, but a server can
  return an endless run of valid pages, downloading payloads on each. After a
  reconnect the earlier pass keeps running; generation checks only stop its
  writes.
- **Recommendation:** cap pages or items per pass and cancel a pass when a new
  generation starts, if availability against a malicious server is in scope.
- **Decision:**

### 35. Sequence numbers from the server are accepted without a range check

- **Status:** open; checked in code (the `server_time` in `hello_ack` is never
  read). The effect on the current anchor-based store is not re-checked.
- **Severity:** medium (needs a malicious server). On main.
- **Where:** `crates/client/src/engine.rs`, live event and snapshot paths;
  ordering in `crates/client/src/local_store.rs`.
- **What happens:** event and listing sequence numbers are compared as
  given. A far-future value can win every later ordering comparison for that
  object; the audit showed it making an object immune to deletes and sweeps.
- **Recommendation:** reject or clamp any sequence number far beyond
  `hello_ack.server_time` plus a small grace window.
- **Decision:**

### 36. Reconnects downloaded held payloads and invalidations reset the backoff

- **Status:** payload downloads reduced in `1d519d3`; cache eligibility fixed
  in the working tree; not committed.
  Invalidation backoff remains open.
- **Severity:** medium (also costs an honest server). On main.
- **Where:** `crates/client/src/engine.rs`, clipboard and schedule snapshots
  and live materialization.
- **What happened:** each pass downloaded and decrypted every listed clipboard
  or schedule item even when the device already held that revision, up to 100
  clipboard items of 16 MiB. A server `invalidate` ends the connection cleanly, which resets the
  reconnect backoff to one second, so a server can repeat this continuously.
- **Fix:** verify the envelope and retain rollback and continuity checks, then
  skip payload downloads only when this session's in-memory revision and
  envelope body hash match and verified payload ciphertext is available.
  Mark unchanged objects seen so sweeps keep them. Request-count tests
  cover schedule and clipboard reconnects, changed payload verification, and
  rejected heads. List pages now request the default server maximum of 500.
- **Remaining:** do not reset the backoff on a server `invalidate`.

### 37. Browser clipboard sync stops on large items

- **Status:** open; checked in code
- **Severity:** medium. On main.
- **Where:** `crates/client/src/local_store.rs`, the browser
  `write_stored_object_record_with_payload`; `snapshot_clipboard` in
  `crates/client/src/engine.rs`.
- **What happens:** the browser store writes payload ciphertext with
  `serde_json::to_string(&[u8])`, a JSON array of numbers at about 3.6
  characters per byte, into `localStorage` (5 to 10 MB per origin). A copy of
  about 2 MB fails with a quota error, the error aborts the snapshot pass and
  its sweep, and every reconnect repeats it.
- **Recommendation:** store ciphertext as base64 (or in IndexedDB), and make one
  item's write failure skip that item instead of ending the pass.
- **Decision:**

### 38. The browser WebSocket has no inbound size cap and no deadlines

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `crates/client/src/engine.rs`, `BrowserWs` and the browser
  `ws_connect`.
- **What happens:** every inbound frame goes into an unbounded channel, and
  there is no connect, `hello_ack` or read timeout, so a silent server leaves
  the browser showing Connected forever. Native clients have 30 s, 10 s and
  75 s deadlines.
- **Recommendation:** close the socket on frames above a few KB and add the same
  deadlines native has.
- **Decision:**

### 39. Daemon IPC sends bytes as JSON number arrays

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `crates/daemon-types/src/protocol.rs` (plain `Vec<u8>` fields such
  as `SendClipboardPayloadParams.bytes`); the 32 MiB line cap in
  `crates/daemon/src/handler.rs`.
- **What happens:** a clipboard payload above about 9 MiB serializes past the
  32 MiB line cap, and the daemon drops the desktop app's connection.
- **Recommendation:** send bulk bytes as base64 or length-prefixed binary, and
  reject an oversized line without dropping the connection.
- **Decision:**

### 40. Login and resume do not check the identity the server returns

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `crates/client/src/engine.rs`, login and resume;
  `crates/server/src/routes/auth.rs`, `validate`.
- **What happens:** the client adopts whatever device id and username the
  server returns, and `GET /api/auth/validate` returns an empty success, so a
  resume mounts whatever identity the stored blob names.
- **Recommendation:** fail on a device id or username mismatch at login; have
  `validate` return user id, device id and username and compare them on resume.
- **Decision:**

### 41. The web resume blob survives a refused resume and removal of this device

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `web/src/backend/index.ts`, `resumeSession`; `web/src/App.tsx`,
  `removeDevice`.
- **What happens:** `resumeSession` keeps the stored token and keys after any
  failure, including a definitive 401, and removing the current device does
  not clear them. The data key stays in `sessionStorage` until the tab closes.
- **Recommendation:** clear the blob on a 401 at resume and when the removed
  device is this one.
- **Decision:**

### 42. Keys and passphrases are not wiped from memory on every path

- **Status:** open; checked in code
- **Severity:** low (needs a memory dump or root). On main.
- **Where:** `crates/core/src/crypto.rs`, `decrypt` and `unwrap_with_key`
  (return plain `Vec<u8>`); `crates/client/src/api_client.rs`, `login_prepare`
  (takes `&str`); the cache key copy in `crates/client/src/engine.rs`;
  passphrase state in `mobile/src/App.tsx`.
- **What happens:** copies of the device signing key, data key, export key and
  passphrase can stay in freed memory, swap or crash dumps.
- **Recommendation:** return `Zeroizing` from `decrypt` and `unwrap_with_key`,
  add a secret wrapper type for the auth APIs, and clear the mobile passphrase
  state after login.
- **Decision:**

### 43. Collab cursor data from other participants is not validated

- **Status:** open; checked in code
- **Severity:** medium (anyone with the share link, or the server). On main.
- **Where:** `web/src/CodeEditor.tsx` (sets only the local colour);
  `y-codemirror.next` remote selections; the server relays awareness unchanged
  in `crates/server/src/collab_sync.rs`.
- **What happens:** a remote `color` value is placed into an inline `style`
  attribute, which allows a page-covering overlay (CSS only, no script). A
  malformed remote cursor permanently disables remote cursors in that editor
  until it is rebuilt. Awareness has no size or rate limit beyond the 4 MiB
  frame cap.
- **Recommendation:** rewrite remote awareness states before the editor reads
  them: colours from a fixed palette keyed by client id, malformed cursors
  dropped. Validate frame shape and budget on the server too.
- **Decision:**

### 44. Collab docs have no size budget and do not count toward quota

- **Status:** open; checked in code
- **Severity:** medium. On main.
- **Where:** `crates/server/src/collab_sync.rs`, `crates/server/src/routes/collab.rs`.
- **What happens:** the only limit is 4 MiB per message. Anyone with the share
  link can grow a doc without bound, CRDT tombstones make the growth permanent,
  and every later opener downloads the whole state. `collab_docs.yjs_state` is
  not counted in `users.storage_bytes`.
- **Recommendation:** a cumulative size ceiling per doc and a byte and rate
  budget per connection; decide whether collab state counts toward quota.
- **Decision:**

### 45. Deleting a collab doc does not disconnect people editing it

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `crates/server/src/routes/collab.rs`, `delete_collab_doc` (does not
  touch the room map in `state.rs`); the `y-websocket` client.
- **What happens:** connected peers keep editing the in-memory room after the
  delete. A guest's editor retries the socket forever after the 403 and looks
  normal while discarding everything typed.
- **Recommendation:** evict the room and close its connections on delete; show
  "document unavailable" after failed reconnects.
- **Decision:**

### 46. Collab share tokens never expire and always allow editing

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `crates/server/src/routes/collab.rs`; `web/src/CodeEditor.tsx`;
  the browser and native local stores.
- **What happens:** the token is a full read and write credential with no
  expiry, revocation or read-only variant. It travels in the WebSocket URL query
  string (proxy logs, history), is stored in plaintext in both local caches, and
  is the one server credential that reaches the Tauri webview's JavaScript.
- **Recommendation:** move it out of the URL, add expiry, rotation and a
  read-only token, and encrypt the stored collab record.
- **Decision:**

### 47. Shared collab docs show the owner's account username

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `web/src/App.tsx`, `displayName={state.session?.username ?? "You"}`.
- **What happens:** the login username goes to every share-link holder and the
  server in cursor data.
- **Recommendation:** a per-doc display name, or a neutral label on shared docs.
- **Decision:**

### 48. Android shows decrypted content to screenshots, recents and recorders

- **Status:** open; checked in code
- **Severity:** medium. On main.
- **Where:** the Android main activity (no `FLAG_SECURE` in `mobile/src`,
  `mobile/app.json` or `mobile/modules`); the viewers in `mobile/src/App.tsx`.
- **What happens:** clipboard and file content appears in the recents
  thumbnail, in screenshots, and in any screen-recording app the user allowed.
  `FLAG_SECURE` would also block the user's own screenshots.
- **Recommendation:** set `FLAG_SECURE` on the main window and, on API 33 and
  above, `setRecentsScreenshotEnabled(false)`.
- **Decision:**

### 49. Android clipboard copies are not marked sensitive

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `mobile/src/backend.ts` copy paths (`expo-clipboard`).
- **What happens:** copied secrets show in Android's clipboard preview and
  clipboard history.
- **Recommendation:** set `EXTRA_IS_SENSITIVE` through a small native module;
  consider clearing the clipboard after a delay.
- **Decision:**

### 50. The Android app requests an overlay permission it does not use

- **Status:** open; checked in code (`SYSTEM_ALERT_WINDOW` is in the generated
  manifest; `mobile/app.json` does not block it)
- **Severity:** low. On main.
- **Where:** `mobile/app.json`.
- **What happens:** the app asks for draw-over-other-apps, and no sensitive
  button filters touches while obscured.
- **Recommendation:** block the permission in `app.json` and add
  `filterTouchesWhenObscured` to confirm, delete and logout.
- **Decision:**

### 51. Password-manager clipboard markers are not honoured everywhere

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `crates/client/src/clipboard_watcher_linux.rs`, `read_clipboard`
  and `clipboard_has_password_manager_marker`.
- **What happens:** on Linux the marker is read before the payload and never
  re-checked after it, and a stalled marker read counts as "no marker", so a
  password copied from a manager can sync. macOS re-checks after reading. The
  browser and mobile apps cannot see the markers at all.
- **Recommendation:** re-check the marker after the payload read and treat a
  stalled read as marked; tell web and mobile users that markers are not
  honoured there.
- **Decision:**

### 52. The Tauri window has every `core:default` permission

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `web/src-tauri/capabilities/default.json`.
- **What happens:** the webview can call unused core commands such as image
  loading and devtools toggling. No injection sink is known.
- **Recommendation:** an explicit allowlist, after a desktop run confirms what
  the app needs.
- **Decision:**

### 53. The daemon trusts every process of the same user

- **Status:** open; checked in code
- **Severity:** low (accepted so far). On main.
- **Where:** the `upload_file` and `download_file` daemon commands;
  `crates/daemon/src/keychain.rs` (IPC secret and session credentials);
  `web/src-tauri/src/ipc_secret.rs` (Linux read with plain `std::fs::read`).
- **What happens:** another process of the same user can request the IPC secret
  from the login keychain; macOS prompts unless that program is trusted. With
  the secret, it can make the daemon read or write any path. The daemon checks
  only the peer uid, not its code signature. The Tauri
  Linux secret read lacks the daemon's symlink and regular-file check (no new
  capability, since the same user can read the file anyway).
- **Recommendation:** keep same-user trust and say so, or harden: byte-based
  file IPC instead of paths, an app-bound keychain ACL, a code-signature check
  of the peer on macOS, and the shared regular-file check on the Tauri side.
- **Linux options:** an AppArmor or SELinux profile, the Flatpak or Snap
  sandbox identity of the peer, and failing closed when a required peer label
  cannot be verified.
- **Decision:** retain same-user IPC trust for now. Desktop resume stores the
  revocable bearer token and two derived keys, so credential-store access can
  decrypt the cache and unwrap the existing signing identity without another
  OPAQUE login. The passphrase and OPAQUE export key remain unpersisted.
  First try the macOS data protection keychain with when-unlocked,
  this-device-only protection and no iCloud synchronization. For missing
  entitlement or code-signing errors, use the regular login keychain with its
  default access rule, the same storage and trust as the IPC secret. That rule
  trusts the creating binary; other programs get a macOS prompt. The login
  keychain is local to this Mac and normally unlocked while the user is logged
  in. It follows login-keychain locking and backup behaviour, without the data
  protection store's explicit this-device-only guarantee. Accept this fallback
  so local ad-hoc builds can resume. Neither case requires user presence for
  the trusted daemon, which starts unattended. Reads check both stores but
  resume only the session ID, store, and account recorded in the non-secret
  profile;
  logout attempts deletion in both. Logs identify the chosen store. Locked or
  denied keychain access does not trigger a storage downgrade. Linux has no
  session secret store and requires login at each start. The broader IPC
  peer-trust issue remains open.
  Keychain items and IPC secret caches are now scoped to the daemon's data
  directory. The default directory keeps its existing names; independent QA
  directories use a canonical-path hash, preventing accidental access to the
  owner's items (entry 165). This does not change same-user IPC trust.

### 54. Whether a username exists can still be learned

- **Status:** open; registration checked in code, timing not re-checked
- **Severity:** low. On main.
- **Where:** `crates/server/src/routes/auth.rs`, `register_finish` and
  `challenge`.
- **What happens:** a holder of an unused invite gets 409 from
  `register_finish` for a taken username, without spending the invite. Login
  challenge responses have the same shape for real and unknown users, but the
  real path does an extra unwrap, so timing differs.
- **Recommendation:** accept both (the first needs a valid invite), or make the
  finish result and the challenge timing indistinguishable.
- **Decision:**

### 55. Auth rate limits allow a targeted or a global login lockout

- **Status:** open; not re-checked
- **Severity:** low (availability). On main.
- **Where:** `crates/server/src/rate_limit.rs`.
- **What happens:** anyone who knows a username can use up its per-username
  challenge budget from many addresses. A slow flood spread over many
  addresses can drain the global auth bucket and block every login.
- **Recommendation:** count only failed logins, or key by client and username;
  limit Argon2 work by concurrency instead of a global request bucket.
- **Decision:**

### 56. Sessions cannot be listed or revoked one at a time

- **Status:** open; checked in code (only the `/api/auth/devices` routes exist)
- **Severity:** low. On main.
- **Where:** `crates/server/src/routes/auth.rs`; `crates/server/src/main.rs`.
- **What happens:** a leaked 30-day token can be revoked only by removing its
  whole device. There is no "log out other sessions" and no admin revocation.
  Device removal deletes the row, so the same device id can register again.
- **Recommendation:** `GET /api/auth/sessions`, `DELETE /api/auth/sessions/{id}`
  and `POST /api/auth/sessions/revoke-others`; optionally a revoked state on
  devices instead of a hard delete.
- **Decision:**

### 57. The data key cannot be rotated and the passphrase cannot be changed

- **Status:** open; checked in code (no passphrase-change route)
- **Severity:** low under the threat model, but it decides how bad every key
  leak is. On main.
- **Where:** `crates/core/src/crypto.rs`, `derive_data_key_from_opaque_export_key`.
- **What happens:** the data key is fixed for the life of the account. If it or
  the passphrase leaks, all past and future ciphertext stays readable; revoking
  sessions only stops API access.
- **Recommendation:** passphrase change through OPAQUE re-registration, plus a
  client pass that re-encrypts retained objects under the new key.
- **Decision:**

### 58. OPAQUE passphrase stretching is at the OWASP minimum

- **Status:** open; checked in code (19 MiB, 2 passes, 1 lane, pinned)
- **Severity:** low. On main.
- **Where:** `crates/core/src/crypto.rs`, `OPAQUE_KSF_*`.
- **What happens:** the cost is pinned so a dependency bump cannot change it,
  but it was not raised. Raising it later forces every account to re-register.
- **Recommendation:** decide before deployment, for example 64 MiB and 3
  passes, weighed against login time on mobile and in the browser.
- **Decision:**

### 59. Server user scoping relies on hand-written filters

- **Status:** open; checked in code (no `UserDb` or `UserScope` helper)
- **Severity:** low (no current leak found). On main.
- **Where:** private handlers in `crates/server/src/routes/`.
- **What happens:** each handler adds its own `user_id` filter, so a new handler
  that forgets one would read across users unnoticed. The database does not stop
  a session or revision from naming another user's device (composite foreign
  keys not re-checked after migration 5). Not every route has a two-user
  isolation test.
- **Recommendation:** a scoped database helper or a CI check for raw entity
  calls, composite device foreign keys, and a two-user test per private route.
- **Decision:**

### 60. Web client security depends on the host that serves it

- **Status:** open; CSP checked in code
- **Severity:** medium. On main.
- **Where:** `web/index.html` (CSP in a meta tag); the production static host;
  `crates/web-wasm`.
- **What happens:** any script running on the web origin can ask the wasm
  backend to decrypt everything and can read the session blob and mint
  WebSocket tickets; the server does not check WebSocket `Origin`. The bundle
  has no subresource integrity. `frame-ancestors` and `X-Frame-Options` must come
  from the production host as headers, which is not chosen yet. The CSP allows
  connections to any HTTPS or WSS server, since the user picks the server.
- **Recommendation:** accept "script on the origin is total compromise" and
  make the production host send a real CSP header, the framing headers and
  SRI when a host is chosen; decide whether a build-time server allowlist is
  worth losing the free choice of server.
- **Decision:**

### 61. No certificate pinning

- **Status:** open; checked in code
- **Severity:** info. On main.
- **Where:** `crates/client/src/api_client.rs`.
- **What happens:** the client trusts any certificate the system trusts.
- **Recommendation:** accept, or pin the server's public key.
- **Decision:**

### 62. The client picks the creation time that sets clipboard expiry

- **Status:** open; checked in code (bounded to a one-hour window)
- **Severity:** info. On main.
- **Where:** `crates/server/src/routes/objects.rs`, `expires_at` from the
  envelope's `created_at`.
- **What happens:** a client can shift its own clipboard items' expiry by up to
  the accepted `created_at` window.
- **Recommendation:** accept, or derive expiry from server time.
- **Decision:**

### 63. Collab docs are readable by the server and open to anyone with the link

- **Status:** open (confirm the design)
- **Severity:** low. On main.
- **Where:** `crates/server/src/collab_sync.rs`, `crates/server/src/routes/collab.rs`.
- **What happens:** collab content is plaintext on the server, which can read,
  change or inject it, and the socket and share page accept anyone holding the
  token with no account.
- **Recommendation:** confirm as designed and say so in the share UI.
- **Decision:**

### 64. Rollback and provenance checks have deliberate limits

- **Status:** open (confirm the design)
- **Severity:** low. On main.
- **Where:** `crates/client/src/local_store.rs` anchors; envelope signature
  checks in `crates/client/src/engine.rs`.
- **What happens:** a fresh install has no anchor; a jump of more than one
  revision cannot check the missing links. Browser anchors live in an evictable store. Envelope
  signatures use device keys the server supplies, so they prove server-checked
  provenance only; the AEAD under the data key is the real authenticity check.
- **Recommendation:** confirm these limits; decide whether browser anchors need
  durable storage.
- **Decision:**

### 65. Native revision anchors are kept forever and lost on a schema change

- **Status:** open; not re-checked
- **Severity:** low. Not on main.
- **Where:** `crates/client/src/local_store/sqlite.rs`, `object_anchors`.
- **What happens:** one anchor row per object ever accepted, never pruned, and
  hydration still loads every held object (lists do not use the indexes). A
  schema version change recreates the database and drops every anchor.
- **Recommendation:** measure anchor growth before choosing expiry (likely per
  kind); carry `object_anchors` across schema changes once anyone runs the
  client long-term; add bounded queries for the clipboard list and calendar
  range.
- **Decision:**

### 66. Historical revisions have no retention policy

- **Status:** open; not re-checked
- **Severity:** low. Not on main.
- **Where:** `crates/server/src/routes/objects.rs`, `purge_object`; schedule
  revision references.
- **What happens:** a recording's reference to a plan revision does not stop
  that revision from being purged, and nothing defines how long history is kept.
- **Recommendation:** decide retention and purge rules for referenced revisions.
- **Decision:**

### 67. The local SQLite file itself is not encrypted

- **Status:** open; not re-checked
- **Severity:** low. Not on main.
- **Where:** `crates/client/src/local_store/sqlite.rs`.
- **What happens:** object content is encrypted by the app, but the database's
  own metadata (ids, kinds, sequence numbers) is plaintext in a `0600` file.
- **Recommendation:** keep the current boundary unless a threat model needs
  SQLCipher.
- **Decision:**

### 68. The server pepper cannot be rotated

- **Status:** open (out of scope for this branch)
- **Severity:** low. On main.
- **Where:** `crates/server/src/secret.rs`, `crates/server/src/secret_storage.rs`.
- **What happens:** wrapped blobs carry no key id; changing the pepper means a
  new database.
- **Recommendation:** a key-id byte in wrapped blobs and an admin re-wrap
  command.
- **Decision:**

### 69. The generated React Native bridge lacks input bounds checks

- **Status:** open (out of scope for this branch); not re-checked
- **Severity:** low (needs a malicious npm dependency). On main.
- **Where:** generated UniFFI JSI code under `packages/mobile-bridge/cpp`.
- **What happens:** string reads trust offsets, buffer frees trust a
  JavaScript-writable capacity, and the raw module sits on `globalThis`.
- **Recommendation:** bounds checks and buffer provenance in the generator
  patch; keep the module off `globalThis`.
- **Decision:**

### 70. Some dependency advisories remain

- **Status:** open; re-checked during the 2026-10-06 Rust dependency upgrade
- **Severity:** low. On main.
- **Where:** `Cargo.lock`, `osv-scanner.toml`.
- **What happens:** the Linux desktop still pulls GTK and glib 0.18 through
  Tauri. The audit filters the glib, proc-macro-error and smallstr advisories,
  plus existing JavaScript tooling advisories. SQLx 0.9 removed `rsa` from
  the lockfile; the upgrades also removed rkyv 0.7 and the unic crates.
- **Recommendation:** track upstream fixes for the remaining advisories.
- **Resolved:** obsolete rsa, rkyv and unic audit ignores were removed.
- **Decision:**

### 116. A live delete event is not signed, so the device keeps a weaker anchor

- **Status:** fixed in `d2841d9`. The WebSocket does not replay the event log (a reconnect takes a snapshot), so only the live broadcast needed the tombstone
- **Severity:** medium. Not on main.
- **Where:** `crates/api-types` (the event shape), `crates/server/src/ws.rs`
  and the event replay, `crates/client/src/local_store.rs`
  (`apply_delete_inner`, the observed-delete anchor).
- **What happens:** every revision, including a tombstone, is signed by the
  device that made it, but a live "deleted" event only names the object and a
  sequence number. The client cannot check it, so it keeps the previous head
  as a weaker "seen deleted" anchor and asks any later revision to be at least
  two steps newer. That weaker anchor is what made entry 1 possible. For an
  encrypted object every delete event has a signed tombstone on the server
  behind it.
- **Decision:** build in this PR (owner): the event carries the signed
  tombstone, the client verifies it and anchors on it exactly, and the
  "seen deleted" anchor and the "two steps newer" rule go.

### 117. Logout lets the old session's work keep running

- **Status:** fixed in `d8372c0`
- **Severity:** high as a class. Five review passes found bugs of this shape
  (entries 3 to 6 among them); each was patched with a session check.
- **Where:** `crates/client/src/engine.rs` and every shell: the daemon, wasm,
  UniFFI, Tauri, and the web and mobile logout buttons.
- **What happens:** logout clears keys and memory while uploads, downloads,
  timer writes, calendar syncs and background sync from the old session are
  still running, so each of them needs its own check that the session it
  started under is still current.
- **Decision:** build in this PR (owner). Logout lists the user-started work
  still running and offers Wait or Cancel them. Wait keeps the user signed in;
  the app never signs out on its own when the work ends. Cancel them stops the
  work, waits until it has stopped, then signs out. Background sync is always
  cancelled without asking. Quitting the app is not a logout.

### 124. The calendar value cap misses semicolon-separated values

- **Status:** fixed in `90e4a4a`
- **Severity:** medium. Entry 9 was incomplete.
- **Where:** `crates/schedule/src/ingest.rs`, `validate_value_count`.
- **What happens:** the parser also splits values on `;` for `X-` properties,
  `GEO` and `REQUEST-STATUS`, and each `;NAME` in the parameters becomes a
  parameter. An 8 MiB feed of semicolons peaks near 700 MB and is accepted.
- **Decision:** fix (Claude): count every separator the parser splits on.

## Bugs

### 167. Clipboard reconciliation could deadlock the shared store

- **Status:** fixed in the working tree; not committed.
- **Severity:** high. Found in `7597c27` on a desktop with imported calendars.
- **Where:** the clipboard snapshot's buffered futures in the client engine.
- **What happened:** buffered cache and revision checks acquired the store's
  sync lock and waited for its database lock. Another buffered future queued
  for the sync lock. When a result became ready, the consumer waited for that
  lock to mark or persist the item, stopping polling of the buffered futures
  that had to release it. Schedule snapshots, expansion and writes also needed
  the sync lock and stopped. The copied production store had complete payload
  metadata; database contention was enough to trigger the deadlock.
- **Fix:** finish cache and revision checks and mark held items before buffering
  downloads. Buffered futures only download, verify and decrypt; they take no
  store locks. Persistence still validates revisions at the storage boundary.
  A regression holds the database lock during a reconnect, then checks that
  reconciliation, schedule expansion and writes finish. It covers both complete
  and missing payload metadata, requiring verified downloads in the latter case.

### 166. Reconnect skip checks read and hashed every held payload

- **Status:** fixed in the working tree; not committed.
- **Severity:** medium. Found in `7e0a7cc`.
- **Where:** local store cache eligibility and calendar source reuse.
- **What happened:** every reconnect loaded and hashed held payload ciphertext
  to confirm its presence. Large clipboard histories could read gigabytes.
  Source reads ran the same cache check twice.
- **Fix:** compare recorded payload length and hash with the envelope and check
  presence, keeping the exact in-memory head and all revision checks. Native
  storage queries the blob length; browser storage checks the key without
  loading its value. Actual payload reads still verify bytes. Source reads check
  the cache once. Tests count payload reads for large held clipboard objects and
  cache checks for source reads, and reject missing or mismatched cache entries.

### 165. Extra macOS daemons shared the owner's keychain items

- **Status:** fixed in `551eff8`.
- **Severity:** high.
- **Where:** daemon session credentials and IPC secret; shared daemon client.
- **What happened:** every data directory used the same keychain service and
  accounts. A QA daemon read the owner's IPC secret and could overwrite or
  delete the owner's saved session. The process cache also held one IPC secret
  regardless of directory.
- **Fix:** keep today's names for the default directory. Other directories use
  a service suffixed with the SHA-256 hash of their canonical path. Protected
  and login session stores and the IPC secret all use that namespace. Clients
  share the naming code and cache secrets per canonical directory. The daemon
  and CLI accept `--data-dir`; all desktop components accept `CLIPPER_DATA_DIR`.
  Fake-store tests cover unchanged default names, distinct directories, aliases,
  cached reconnects, fallback session saves, restarts and logout isolation.
  QA must use its own directory and socket and disable clipboard watching.
  Byte assertions confirm that the default service and both account names
  exactly match the daemon and client constants in `b417e0d`'s parent.

### 164. Shared persisted heads could hide missing content or misidentify a source revision

- **Status:** fixed in `7e0a7cc`; performance follow-up in entry 166.
- **Severity:** medium. Found in `1d519d3`.
- **Where:** client head reuse and local schedule source reads.
- **What happened:** another browser tab could advance localStorage while this
  tab still held older content or no content. A Present record could also
  outlive its payload. Comparing only persisted heads skipped needed downloads
  and could pair an old source payload with a newer head when preparing a write.
- **Fix:** bind each in-memory record to its own revision and envelope body
  hash. Snapshot, live materialization and source reuse require that record to
  match the current head and its verified payload ciphertext to be present.
  Missing or older content is downloaded and verified. Schedule writes use the
  content's own head. Browser tests use two stores sharing real localStorage;
  native tests remove payloads beneath Present records and count downloads.

### 162. Large calendar imports spent requests on unchanged and newly staged objects

- **Status:** fixed in `1d519d3`; cache eligibility follow-up in entry 164.
- **Severity:** medium.
- **Where:** client snapshots and `calendar_import.rs`.
- **What happened:** reconnecting with 1,246 held work events needed roughly
  1,264 requests and could hit the 1,200/minute user limit. New imports also
  checked every fresh deterministic ID with a GET. A refresh repeatedly
  downloaded the same source payload.
- **Fix:** unchanged held heads need only 500-item list pages. New batches
  create event IDs directly, while conflicts and resumed batches retain full
  verification. Source reads reuse a verified held payload only after checking
  current metadata and its exact head. Behaviour tests count list, event and
  source payload requests and verify changed or rejected heads.

### 163. The recipe page shadowed its load error in a catch block

- **Status:** fixed in `1d519d3`.
- **Severity:** low.
- **Where:** `web/src/kitchen/RecipePage.tsx`.
- **Fix:** use `cause` for the session-change failure; preserve error handling.

### 158. A stale keychain copy could restore a different account

- **Status:** fixed in the working tree; not committed.
- **Severity:** medium. Found in `39a94d2`.
- **Where:** daemon session store and non-secret profile.
- **What happened:** protected credentials for account A could survive failed
  deletion while a newer account B was saved in the login keychain. Startup
  preferred the protected copy and could restore A.
- **Fix:** record a random non-secret session ID and chosen store in the profile.
  Resume only matching session and account metadata. Ignore and try to delete
  other copies. If no copy matches, keep login prefilled. Fake-store tests cover
  logout A, failed deletion, fallback login B, and restart.

### 159. A failed cache hydration could hide step timers from the first alarm plan

- **Status:** fixed in the working tree; not committed.
- **Severity:** low. Found in `39a94d2`.
- **Where:** client authentication and cached document publication.
- **What happened:** the state notification preceded loading held recipes. If
  cache hydration failed, no later publication notified the alarm planner.
- **Fix:** load held documents before the state notification, including the
  hydration-error path. Test that a blocked document load sends no notification
  and that the first resulting plan includes the step timer.

### 160. A transient keychain write failure disabled an otherwise valid saved session

- **Status:** fixed in the working tree; not committed.
- **Severity:** low. Found in `39a94d2`.
- **Where:** daemon session confirmation persistence.
- **What happened:** a locked store or denied prompt at the six-hour write set
  the signed-out marker even though the previous credential copy remained valid.
- **Fix:** keep the previous copy and current-session record, log the failed
  write, and retry later. Only logout or server rejection sets signed-out.
  Tests cover retry and offline resume after a failed confirmation write.

### 161. The clipboard QA option rejected numeric environment values

- **Status:** fixed in the working tree; not committed.
- **Severity:** low. Found in `39a94d2`.
- **Where:** daemon option parsing and `AGENTS.md`.
- **What happened:** `CLIPPER_DISABLE_CLIPBOARD_WATCHING=1` failed parsing.
- **Fix:** accept `1`/`0`, `true`/`false`, and `yes`/`no`, case-insensitively.
  Test each form and invalid input through the environment. QA must use an
  enabled value; update the agent rule.

### 155. Desktop resume credentials had unsafe storage and excessive writes

- **Status:** fixed in `39a94d2`; follow-up fixes are uncommitted.
- **Severity:** high. On main.
- **Where:** daemon credential store and session persistence.
- **What happened:** Linux wrote the token and derived keys to plaintext JSON.
  macOS used the default login-keychain access rule. Minute confirmations
  rewrote the entire credential item every minute.
- **Fix:** Linux saves only the non-secret profile. Tests inject a memory store.
  macOS prefers unlocked-only, device-only data protection without user presence,
  with the default login-keychain rule for entitlement/signing failures so
  local ad-hoc builds can resume. Both stores keep secrets out of plaintext files.
  Unchanged sessions persist a new confirmation only every six hours; token
  and key changes are immediate. Update the local encryption documentation.

### 156. Failed credential deletion could undo logout after restart

- **Status:** fixed in `39a94d2`; current-session matching is an uncommitted follow-up.
- **Severity:** high. On main.
- **Where:** daemon logout and startup.
- **What happened:** logout reported success even if deleting stored credentials
  failed, so an offline restart could restore the old session.
- **Fix:** durably save a signed-out marker in the non-secret profile before
  reporting success. Startup checks it before credential reads and retries
  deletion. Reject logout success if the marker cannot be saved.

### 157. A QA daemon captured the owner's clipboard into test accounts

- **Status:** fixed in `39a94d2`; numeric option parsing is an uncommitted follow-up.
- **Severity:** high. On main.
- **Where:** daemon options and client clipboard watcher startup.
- **What happened:** a second daemon signed into a test account automatically
  watched the same macOS clipboard and uploaded the owner's content.
- **Fix:** add `--disable-clipboard-watching` and
  `CLIPPER_DISABLE_CLIPBOARD_WATCHING=true`. Apply the setting before login or
  resume. Require it for every extra QA daemon in `AGENTS.md`.

### 154. Desktop loses its session and server URL after the daemon restarts

- **Status:** fixed in `7411747`; hardened in `39a94d2`; follow-up fixes are uncommitted.
- **Severity:** medium. On main.
- **Where:** daemon credential storage and startup, saved profiles, desktop login form.
- **What happened:** the daemon stored only profile metadata and waited for a
  passphrase at startup. It never saved or resumed the token and derived keys.
  Saved app profiles omitted the server URL, and the desktop form used the build default.
- **Fix:** keep the token and derived keys in the macOS keychain and resume the
  existing device session at startup. Keep the non-secret profile in a private
  file and use its server URL and username for sign-in, including when a changed
  ad-hoc code signature denies keychain access. Log credential read and resume failures.
  Logout removes the saved session and keeps the login prefill.

### 143. Android startup hangs when the fingerprint prompt cannot start

- **Status:** fixed; device QA pending.
- **Where:** `mobile/src/backend.ts`, the resume read of the stored
  credentials through `expo-secure-store` with `requireAuthentication`.
- **What happens:** opening the app while the phone is locked, for example
  with `adb shell monkey`, makes Android refuse the fingerprint prompt
  ("Unable to start authentication. Called after onSaveInstanceState()"). The
  credential read then never settles, and the app stays on "Starting Clipper"
  until it is removed from recent apps and opened again.
- **Decision:** refuse the prompt when the activity is not resumed or its
  state is saved. Cancel a prompt after 25 seconds and bound the credential
  read at 30 seconds. Show the startup error with retry and sign-in actions.

### 145. A refused native session kept its encrypted local data

- **Status:** fixed.
- **Where:** `crates/client/src/engine.rs`, session rejection;
  `crates/client/src/local_store.rs`, native profile storage.
- **What happened:** rejection cleared keys and display state, but left
  cached objects and app-data rows on disk, including pending changes.
- **Fix:** erase cached objects, payloads, app-data rows, pending changes and
  sync cursors before clearing the rejected session. Keep revision anchors
  and the wrapped device identity for rollback checks and later sign-in.

### 146. Offline session confirmation accepted unrelated successful replies

- **Status:** fixed.
- **Where:** `crates/client/src/api_client.rs`, session validation;
  `crates/server/src/routes/auth.rs`, validate response.
- **What happened:** any successful HTTP status refreshed the offline limit,
  including a captive portal's HTML page.
- **Fix:** parse the validation response and match its username and device
  against the saved session before recording a confirmation.

### 147. Session rejection and HTTP recovery depended on the WebSocket

- **Status:** fixed.
- **Where:** `crates/client/src/engine.rs`, confirmation and reconnect;
  `crates/client/src/app_data_sync.rs`, sync availability.
- **What happened:** 403 did not erase local data, and HTTP recovery could
  not enable writes or pending pushes until the WebSocket connected.
- **Fix:** treat 401 and 403 as rejection everywhere. Track HTTP availability
  separately and enable HTTP writes and app-data sync after confirmation.

### 148. Concurrent calendar import cleanup can fail intermittently

- **Status:** fixed.
- **Where:** `crates/client/src/schedule_integration_tests.rs`,
  `live_calendar_unchanged_feeds_and_newer_fetches`;
  `crates/client/src/calendar_import.rs`, import reads and cleanup;
  `crates/client/src/local_store.rs`, tombstone validation.
- **What happened:** concurrent imports could purge a payload between reads,
  replace a source head before its payload was read, or deliver a tombstone
  before cleanup checked the old head. Refresh failed or left cleanup pending.
- **Fix:** retry replaced source reads against a verified newer head. Treat
  missing retired payloads and verified competing tombstones as completed
  cleanup. Accept an identical signed tombstone while retaining rollback,
  body identity and parent-link checks.
- **Validation:** controlled replacement and cleanup tests pass.
  `live_calendar_unchanged_feeds_and_newer_fetches` passes 15 consecutive runs
  with imported recurrence rules.

### 151. Foreground reconnect waited for a stalled WebSocket attempt

- **Status:** fixed.
- **Where:** `crates/client/src/engine.rs`, native WebSocket loop.
- **What happened:** `reconnect_now` woke backoff but could not interrupt
  connection or hello waits, delaying reconnection by up to 30 or 10 seconds.
- **Fix:** observe the restart signal throughout confirmation, connection,
  hello and the connected socket. Drop the current attempt before starting
  another one, and restart immediately without backoff. Pass the outer
  receiver's seen version into the attempt so a restart during the handoff
  from confirmation to connection cannot be missed.

### 12. Migration 5 broke collab docs on servers upgraded from main

- **Status:** fixed in `3b57f4c`
- **Severity:** high. Not on main.
- **What happened:** collab docs carried through the migration got their id
  as 36 bytes of text instead of a 16-byte UUID, so listing them failed and
  renaming or deleting them returned 404.
- **Decision:** fix (Claude).

### 13. The desktop app froze while a calendar synced

- **Status:** fixed in `6a3ec21`
- **Severity:** medium. The serial loop was on main; this branch added the
  first long command.
- **What happened:** the daemon handled one connection's requests one at a
  time, and the desktop app uses one connection, so stopping a timer waited
  for a sync of up to a minute or more.
- **Decision:** fix (Claude): each request runs on its own task, at most 8 at
  once per connection.

### 14. The desktop app could lose a reply from the daemon and wait forever

- **Status:** fixed in `6a3ec21`
- **Severity:** medium. On main.
- **What happened:** sending a command while a large state update was arriving
  threw away the part of the line already read. If the lost line was a reply,
  the command never returned.
- **Decision:** fix (Claude).

### 15. A state change made while no screen is waiting is lost

- **Status:** fixed in `f6c55b2`
- **Severity:** medium. On main.
- **Where:** `crates/client/src/engine.rs`, `bump_version`.
- **What happens:** the change notice is dropped when nothing is listening at
  that moment, so the web or mobile screen stays stale until the next change.
- **Decision:** fix (Claude).

### 16. A lost reply to a timer write leaves the timer stuck or duplicated

- **Status:** fixed in `3ccc245`
- **Severity:** medium. Not on main.
- **Where:** `crates/client/src/engine.rs`, `start_actual`,
  `stop_actual_inner`, `write_schedule_record_for_session`.
- **What happens:** the server never echoes a device's own events, and the
  client does not re-read an object after a 409 or a dropped connection. If a
  stop's reply is lost, the timer shows as running and every start or stop
  fails with 409 until Refresh. If a start's reply is lost and the user
  retries, two timers run on the server and this device knows one.
- **Decision:** fix (Claude): after a 409 or a dropped reply on its own write,
  fetch that object's current head.

### 17. Each retry of a failed calendar sync uploads another copy of the feed

- **Status:** fixed in `3ccc245`
- **Severity:** low. Not on main.
- **Where:** `crates/client/src/calendar_import.rs`, `sync_calendar_source`.
- **What happens:** after a lost reply, each Sync uploads a new raw feed file
  (up to 8 MiB, shown in Files, charged to quota), then fails with 409.
  Nothing removes these files.
- **Decision:** fix (Claude): fixed by 16, plus removing the file uploaded for
  an attempt that got a 409.

### 18. One damaged anchor row stops cleanup for that kind on every pass

- **Status:** fixed in `3ccc245`
- **Severity:** medium. Not on main.
- **Where:** `crates/client/src/local_store/sqlite.rs`, `forget_object`.
- **What happens:** reads skip a damaged anchor, but cleanup refuses to
  forget a row that has one, and that error aborts the sweep for the whole
  kind. Objects of that kind deleted on other devices are never removed here.
- **Decision:** fix (Claude).

### 19. A listed file that cannot be decrypted is hidden by cleanup

- **Status:** fixed in `3ccc245`
- **Severity:** low. On main.
- **Where:** `crates/client/src/engine.rs`, `snapshot_files`.
- **What happens:** clipboard and schedule mark such an item as seen and keep
  the copy this device holds; files do not, so cleanup hides a readable older
  copy.
- **Decision:** fix (Claude).

### 20. Events after a second calendar block in a feed are dropped silently

- **Status:** fixed in `21f1cc5`
- **Severity:** medium. Not on main.
- **Where:** `crates/schedule/src/ingest.rs`, `parse_calendar`.
- **What happens:** the parser stops at the first closing `END`, so a feed
  with two `VCALENDAR` blocks, or a stray `END:VEVENT`, imports only what came
  before it and reports nothing skipped.
- **Decision:** fix (Claude).

### 21. Two occurrences share one identity inside a daylight-saving gap

- **Status:** fixed in `21f1cc5`
- **Severity:** low. Not on main.
- **Where:** `crates/schedule/src/engine.rs`, `rule_spans`.
- **What happens:** an imported hourly rule across a gap produces two
  occurrences at the same instant, so cancelling one cancels both.
- **Decision:** fix (Claude): keep the first, as RFC 5545 says.

### 22. Windows fixed-offset zone names are refused

- **Status:** fixed in `21f1cc5`
- **Severity:** low. Not on main.
- **Where:** `crates/schedule/src/ingest.rs`, `feed_time_from_partial`.
- **What happens:** Outlook's `TZID=UTC-11` and similar exact ids are refused
  with the guessed "(UTC+05:30)" names, so the whole import is rejected.
- **Decision:** fix (Claude).

### 23. A revise that arrives after a purge and re-create skips revision numbers

- **Status:** fixed in `09b037a`
- **Severity:** low (only the same account can cause it). Not on main.
- **Where:** `crates/server/src/routes/objects.rs`, `revise_object`,
  `advance_object_head`.
- **What happens:** the server checks the head outside the transaction, so a
  delayed revision 4 can land on a freshly re-created object whose head is 1.
- **Decision:** fix (Claude): the head may only move from n-1 to n.

### 24. The web grid gets one day wrong where daylight saving skips midnight

- **Status:** decided
- **Severity:** low. Not on main.
- **Where:** `web/src/schedule-layout.ts`, `dayBounds`;
  `web/src/calendar-view.ts`.
- **What happens:** in zones such as Santiago and Cairo, on that day the
  column runs from 01:00 to 01:00 the next day, so an early block shows on two
  days.
- **Decision:** accepted as a known issue for now.

### 71. A user action that gets a 401 does not sign the session out

- **Status:** open; checked in code
- **Severity:** low.
- **Where:** `crates/client/src/engine.rs`; only the snapshot and WebSocket
  paths call `end_refused_session_for`.
- **What happens:** an upload or delete refused with 401 shows the error, but
  the session stays signed in until a snapshot or the socket hits a 401.
- **Recommendation:** route user actions through the same helper.
- **Decision:**

### 72. A failed live fetch is never retried

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `crates/client/src/engine.rs`, `handle_created_event` (spawns one
  `materialize_object` and only logs a failure).
- **What happens:** one transient error on a live create or update hides the
  object until the next reconnect's snapshot. `ws-sync-flow.md` says the client
  retries.
- **Recommendation:** a bounded retry within the generation, or correct the
  spec.
- **Decision:**

### 73. A 404 during a live fetch drops the copy this device holds

- **Status:** open; checked in code
- **Severity:** low.
- **Where:** `crates/client/src/engine.rs`, `materialize_object`.
- **What happens:** any 404, including a transient one on an `updated` event,
  removes the local copy until the next reconnect.
- **Recommendation:** treat a 404 as absence only for a `created` event; for an
  update keep the content and let the next snapshot decide.
- **Decision:**

### 74. Logout waits for an in-flight calendar sync

- **Status:** fixed in `d8372c0`: a calendar sync is listed as running work, and Cancel stops it
- **Severity:** low. Not on main.
- **Where:** `crates/client/src/engine.rs`, `calendar_write` held across the
  feed fetch.
- **What happens:** logout can take up to the 60 s fetch timeout.
- **Recommendation:** cancel the fetch on logout.
- **Decision:**

### 75. Collab changes use the device clock as their ordering key

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `crates/client/src/engine.rs`, `rename_collab_doc` and
  `delete_collab_doc` (`Utc::now().timestamp_micros()`); the collab routes do
  not return the committed `seq`.
- **What happens:** overlapping renames settle by response order, so a rename
  can revert locally until the next snapshot. A device clock ahead of the
  server can make a later remote delete look stale until the next sweep.
- **Recommendation:** return the committed `seq` from the collab create,
  rename and delete routes and use it.
- **Decision:**

### 76. A streamed revise that fails midway blocks the object for about two hours

- **Status:** open; not re-checked
- **Where:** `crates/server/src/routes/objects.rs`, pending revisions and the
  orphan sweep.
- **What happens:** revision n+1 stays pending and no other revision of that
  object is accepted until the sweep removes it. A calendar source of a few
  thousand events takes the streamed path, so one dropped connection blocks its
  refresh and deletion.
- **Recommendation:** let the same device replace its own pending revision.
- **Decision:**

### 77. Floating and all-day Android alarms ring at the old zone's time after travel

- **Status:** open; checked in code (`BootReceiver` re-arms the stored instants on
  `TIMEZONE_CHANGED`)
- **Severity:** medium for travellers. Not on main.
- **Where:** `mobile/modules/clipper-alarm/.../BootReceiver.kt`; the alarm plan
  export.
- **What happens:** the plan holds instants computed in the last zone the app
  saw, so a floating 07:00 alarm rings at the old zone's 07:00 until the app is
  opened.
- **Recommendation:** carry the wall-clock time and a floating flag to Kotlin
  and convert on `TIMEZONE_CHANGED`.
- **Decision:**

### 78. SQLite commits on macOS may not survive a power loss

- **Status:** open; checked in code
- **Severity:** low. Not on main.
- **Where:** `crates/client/src/local_store/sqlite.rs` (`synchronous = FULL`, no
  `fullfsync`; the comment claims parity with the old file store).
- **What happens:** a commit survives an app crash but not necessarily a power
  cut, unlike the file store it replaced.
- **Recommendation:** `PRAGMA fullfsync = ON`, or correct the comment.
- **Decision:**

### 79. A calendar of about 3,300 events can no longer be refreshed

- **Status:** open; the 256 KiB cap is checked, the event count is not
- **Where:** `crates/client/src/engine.rs`,
  `MAX_SCHEDULE_PAYLOAD_CIPHERTEXT_BYTES`; the calendar source record.
- **What happens:** the source record lists the event ids of its active,
  pending and retired batches, about 39 bytes each. A refresh holds two
  batches, so a large calendar exceeds the cap.
- **Recommendation:** raise the cap for source records, or move each batch's
  member list into its own object.
- **Decision:**

### 80. The web week grid re-expands on every state change

- **Status:** open; checked in code (`AppState` has no schedule key)
- **Severity:** low. Not on main.
- **Where:** `web/src/SchedulePanel.tsx`; `crates/app-types/src/lib.rs`.
- **What happens:** every clipboard or file event re-runs the week expansion,
  and the whole `AppState` is re-serialized across the wasm boundary on each
  change.
- **Recommendation:** add a schedule content key to `AppState` that changes when
  a plan, recording or override arrives, and key the grid on it.
- **Decision:**

### 81. Servers upgraded through migration 5 keep orphaned payload files

- **Status:** open; not re-checked
- **Severity:** low. Not on main.
- **Where:** the server's objects directory.
- **What happens:** old `{object}.{payload}.bin` files have no row pointing at
  them and are never removed.
- **Recommendation:** delete them by hand on any upgraded server.
- **Decision:**

### 82. An alarm due right after a reboot can fail to ring

- **Status:** open; checked in code (a refused foreground-service start is only
  logged)
- **Severity:** medium (a missed alarm). Not on main.
- **Where:** `mobile/modules/clipper-alarm/.../RingService.kt`, `start`.
- **What happens:** Android 15 and later refuse a `mediaPlayback` foreground
  service whose start is attributed to `BOOT_COMPLETED`, including an exact
  alarm delivered in the short window after boot (reported in
  gdelataillade/alarm#424). Whether `LOCKED_BOOT_COMPLETED` carries the same
  attribution is untested. A later alarm after reboot works.
- **Recommendation:** catch the refusal and ring through the notification's
  full-screen intent and sound; test locked and unlocked reboots on a device.
- **Decision:**

### 83. Android lint crashes

- **Status:** open; not re-checked
- **Severity:** low. Not on main.
- **Where:** the Android lint run.
- **What happens:** lint's Kotlin analysis crashes inside
  `react-native-worklets` ("Cannot find a KaModule for the VirtualFile").
- **Decision:**

### 84. Sparse imported recurrence rules can expand incompletely

- **Status:** open; not re-checked
- **Severity:** low. Not on main.
- **Where:** `crates/schedule/src/engine.rs`, expansion of imported rules.
- **What happens:** the `rrule` crate stops on long empty stretches without
  always saying so, so an unusual sparse provider rule can show fewer
  occurrences or none. The tested daily, weekly, monthly and yearly patterns
  are not affected.
- **Decision:**

### 85. The mobile app never shuts its engine down

- **Status:** open; checked in code
- **Severity:** low. On main.
- **Where:** `crates/mobile-uniffi/src/lib.rs` (no shutdown export); the
  generated `cleanupRustCrate` returns without doing anything.
- **What happens:** a JavaScript reload leaves the old engine running over the
  same data directory, and the bridge's callback is not protected against a
  runtime being recreated.
- **Recommendation:** an engine `shutdown()` called on client swap and on React
  Native invalidation.
- **Decision:**

### 86. A missing cached payload hides a schedule or clipboard row

- **Status:** open; not re-checked
- **Severity:** low. Not on main.
- **Where:** `crates/client/src/local_store.rs`, preview decryption at hydration.
- **What happens:** the object drops out of the list until it is fetched again,
  with nothing shown to say it is loading.
- **Recommendation:** show the row as "not loaded" instead of hiding it.
- **Decision:**

### 87. The browser logs logout and validate as failed requests

- **Status:** open; checked in code
- **Severity:** low.
- **Where:** `crates/client/src/api_client.rs`, `logout` and `validate_session`.
- **What happens:** the response body is dropped unread, so Chrome logs
  `net::ERR_ABORTED` although the server handled both.
- **Recommendation:** read the body, or use `keepalive`.
- **Decision:**

### 119. A revise fails with a 500 while another connection writes

- **Status:** fixed in `90e4a4a`
- **Severity:** medium. Introduced by the head check in `0c7d68c`.
- **Where:** `crates/server/src/routes/objects.rs`, `revise_object`.
- **What happens:** the revise transaction reads before it writes, so SQLite
  refuses its first write at once when another connection holds the write
  lock, instead of waiting. A timer stop, edit or delete fails whenever another
  device writes at the same moment.
- **Decision:** fix (Claude): take the write lock before the first read.
  The shared route helper, registration, login and cleanup now take the lock
  before reading. Completion and purge already write first.

### 120. The desktop IPC connection can hang for good

- **Status:** fixed in `90e4a4a`
- **Severity:** medium-low. Partly on main (the serial loop had the same hang
  with one slot).
- **Where:** `crates/daemon/src/handler.rs`; `web/src-tauri/src/daemon_client.rs`.
- **What happens:** with all eight request slots busy, the daemon stops
  reading; the desktop app stops reading while it writes a large request; the
  daemon's state broadcast then blocks, the running requests cannot reply, and
  nothing frees. A Logout also waits behind eight slow requests. When the
  daemon's read loop exits, the socket stays half open.
- **Decision:** fix (Claude): the desktop app reads while it writes, Logout
  skips the slot limit, and the daemon closes the socket when its read loop
  ends. A request beyond the eighth waits for a slot in its own task, so the
  read loop always keeps reading, and a waiting request is dropped when its
  connection ends, so a retry after reconnecting cannot run twice.

### 121. Lost replies to deletes, and gateway errors, skip recovery

- **Status:** fixed in `90e4a4a`
- **Severity:** low-medium. Not on main.
- **Where:** `crates/client/src/engine.rs`, `write_schedule_record_for_session`
  recovery and `write_tombstone`.
- **What happens:** recovery from a lost reply ran only on a dropped
  connection or a 409. A reply lost behind the Cloudflare tunnel arrives as a
  502, 503, 504 or 520-527, and a retried timer start then made a second timer.
  A lost delete reply left every retry failing with 409 until Refresh.
- **Decision:** fix (Claude): treat gateway errors as ambiguous and give
  deletes the same re-read recovery. The authenticated head getter includes
  tombstones. Recovery accepts an exact matching write as this device's write;
  a competing head follows normal verification and anchor rules.

### 122. Calendar feed copies left behind

- **Status:** open
- **Severity:** low. Not on main.
- **Where:** `crates/client/src/calendar_import.rs`, `sync_calendar_source`.
- **What happens:** the raw feed upload stays in Files and counts toward quota
  when the source save does not commit. It is removed only when the save gets
  a 409, which proves the save can never commit. After a dropped connection or
  a gateway error the save may still be running on the server, and reading the
  old head back does not prove it will not commit later; removing the upload
  then would leave the source pointing at a purged file, stuck for good. A
  sync cancelled by logout between the upload and the save leaves one too.
- **Decision:**

### 123. A series ending on 9999-12-31 never expands

- **Status:** fixed in `90e4a4a`
- **Severity:** low. Not on main.
- **Where:** `crates/schedule/src/recurrence.rs`, `until_scan_bound`.
- **What happens:** the scan bound adds a day and reaches year 10000, which the
  recurrence library refuses.
- **Decision:** fix (Claude): clamp the bound to the end of year 9999.

### 125. The revision-head doc comment describes the wrong function

- **Status:** fixed in `90e4a4a`
- **Severity:** low.
- **Where:** `crates/server/src/routes/objects.rs`.
- **What happens:** the comment describing the returned kind, tombstone flag
  and head revision was attached to the function that checks a proposed head.
- **Decision:** move the existing comment to `head_revision_for_write`.

### 126. The live schedule test overflowed the default test-thread stack

- **Status:** fixed in `90e4a4a`
- **Severity:** low in tests; the same large futures sat inside every
  operation's caller in the apps.
- **Where:** `crates/client/src/engine.rs`, `run_work`.
- **What happened:** `run_work` took each operation's future as a parameter of
  an `async fn`, so the whole operation was stored inline in its caller. The
  live test's future outgrew the 2 MiB default stack and aborted.
- **Decision:** fix (Claude): `run_work` boxes the operation as soon as it is
  called, so a caller holds only a pointer.

### 135. The mobile header's icon buttons have no accessibility labels

- **Status:** open
- **Severity:** low. On main.
- **Where:** `mobile/src/App.tsx`, the two icon buttons beside the title.
- **What happens:** screen readers and UI automation see two unnamed buttons;
  one of them is Logout.
- **Decision:**

## Product decisions

### 25. A changed occurrence without its series rejects the whole import

- **Status:** open
- **Where:** `crates/schedule/src/ingest.rs`, orphaned `RECURRENCE-ID`
  events.
- **What happens:** when a user is invited to one occurrence of someone else's
  series, the provider's feed holds that occurrence without its series. Today
  the import of that whole calendar is rejected.
- **Recommendation:** import it as a one-off event.
- **Decision:**

### 88. What happens to overrides when a series' timing or recurrence changes

- **Status:** open
- **Where:** `crates/client/src/engine.rs`, `update_schedule_item`;
  `crates/client/src/schedule_context.rs`, `effective_overrides`.
- **What happens:** a timing or recurrence edit is refused while local
  overrides exist, with an error that says "resolve them" though there is no UI
  to do so. If such an edit arrives by sync, or the pinned base cannot be read,
  the series and its alarms are skipped with a warning.
- **Recommendation:** choose what the user is offered (discard, map to the new
  definition, or keep them with the old series), then build that UI and its
  write.
- **Decision:**

### 89. Editing one occurrence, or this and later occurrences

- **Status:** open
- **Where:** schedule domain and web composer.
- **What happens:** single-occurrence overrides exist in the model but have no
  editing UI. They can now be created, moved, cancelled and removed through
  the daemon and CLI. "This and future" needs series-split rules that are not
  defined. Expansion always uses the latest definition.
- **Decision:** single-occurrence changes use standalone signed and synced
  override objects with their own revisions. The editing UI and "this and
  later" remain open; no series splitting is implemented.

### 152. Concurrent occurrence overrides hid the whole series

- **Status:** fixed in the working tree.
- **Severity:** high (P1). On main in `607f56a`.
- **Where:** `crates/client/src/schedule_changes.rs`,
  `crates/client/src/schedule_context.rs`, `crates/client/src/local_store.rs`.
- **What happened:** two devices changing the same occurrence before syncing
  created separate override objects. Duplicate recurrence identities made
  expansion and alarm planning skip the entire series and blocked later edits.
- **Fix:** derive the override object ID from the series and recurrence
  identity with UUIDv5. Retry revision conflicts against the recovered head.
  Existing duplicates use the newest signed revision write time, with a stable
  object-ID tie-break, and log a warning. Restore clears all known duplicates.
- **Validation:** a local-server workflow covers two devices writing before
  syncing, one surviving override, restore and reuse of its revision chain,
  and older duplicate objects without hiding the series or its alarms.

### 153. Deleting a series left its occurrence overrides live

- **Status:** fixed in the working tree.
- **Severity:** medium. On main in `607f56a`.
- **Where:** `crates/client/src/engine.rs`, `delete_schedule_object_inner`;
  `crates/client/src/schedule_changes.rs`, restore and override cleanup.
- **What happened:** deletion tombstoned only the series. Its standalone
  overrides stayed live and unlisted; restore failed because the series was
  missing.
- **Fix:** tombstone the series' known overrides before the series in the
  same client operation. Restore on a deleted series cleans up matching
  overrides without requiring a live definition. Cleanup retries conflicts
  against the recovered head and preserves historical revisions.
- **Validation:** a local-server workflow cancels occurrences, deletes the
  series, checks that no live overrides remain, and verifies the second device
  agrees after sync. It also cleans up orphan overrides from an earlier
  deletion one occurrence at a time, checks repeated restore succeeds, and
  restores from a device that has not yet received the override tombstones.

### 90. Editing recorded time

- **Status:** open
- **Where:** schedule recordings (`ActualRecord`).
- **What happens:** manual entry, correcting start and end, linking a recording
  to a plan afterwards, and viewing a recording against its historical plan are
  not designed. Linking afterwards needs a rule for which plan revision to pin.
  The historical-plan read exists in Rust with no UI.
- **Decision:**

### 91. Timers started on two devices

- **Status:** open
- **Where:** `crates/client/src/engine.rs`, `start_actual`.
- **What happens:** starting a timer stops every running timer this device has
  synced, but two devices that have not synced can each run one until the next
  start.
- **Decision:**

### 92. Recovering a calendar import that cannot finish

- **Status:** open
- **Where:** `crates/client/src/calendar_import.rs`.
- **What happens:** a pending batch resumes on every refresh and cannot be
  cancelled, and it blocks removing its source or raw file. A crash between the
  raw-file upload and the pending manifest, including logout cancellation,
  leaves an unreferenced file in Files that nothing removes. Overrides that
  pointed at a purged import cannot be
  reattached.
- **Recommendation:** a cancel action for a pending batch, cleanup of raw files
  no manifest references, and a reattach flow for overrides.
- **Decision:** finish saved pending batches before fetching a new response.
  Cancellation, unreferenced raw-file cleanup and override reattachment remain
  open. Irrecoverable pending data is retired before a fresh fetch. Transient
  recovery failures still stop that refresh so the batch can resume.

### 93. Calendar connector questions

- **Status:** open
- **Where:** future Google and Zoho connectors.
- **What happens:** to settle before building them: which binding is primary
  when the same meeting arrives from two sources; whether replying to an
  invite from Clipper is supported; how provider recurrence and moved instances
  map onto Clipper's overrides; and whether the work Google Workspace account
  allows a personal OAuth app (if not, it is ICS-only). Unchanged normalized feeds
  write nothing; separate sources still duplicate the same meeting.
- **Decision:**

### 94. An override for a position the rule never produces adds an occurrence

- **Status:** open
- **Where:** `crates/schedule/src/engine.rs`, `occurrences`.
- **What happens:** this is how provider `RDATE` additions work, but a local
  override written against a wrong identity would also add one.
- **Recommendation:** keep, and have the future override UI check identities
  against rule positions before writing.
- **Decision:**

### 95. A cancelled occurrence still uses a COUNT slot

- **Status:** open
- **Where:** `crates/schedule/src/engine.rs`.
- **What happens:** this is RFC 5545 behaviour; users often expect the
  opposite.
- **Recommendation:** keep, and say so in the composer where `COUNT` is shown.
- **Decision:**

### 96. An imported start that is not on the rule's weekdays

- **Status:** open
- **Where:** `crates/schedule/src/engine.rs`.
- **What happens:** the `rrule` crate drops a `DTSTART` that is not on `BYDAY`;
  Google includes it, so the event's own summary names a day the calendar never
  shows.
- **Recommendation:** include the `DTSTART` instance for imported events.
- **Decision:**

### 97. COUNT and UNTIL together in an imported rule

- **Status:** open
- **Where:** `crates/schedule/src/recurrence/imported_rule.rs`.
- **What happens:** RFC 5545 forbids it; today the rule stays imported and
  whichever clause the library honours wins.
- **Recommendation:** reject at import, like the other strict checks.
- **Decision:**

### 98. A device clock more than an hour behind cannot edit

- **Status:** open; checked in code (the error is `InvalidObjectEnvelope`)
- **Where:** `crates/server/src/routes/objects.rs`, `validate_object_created_at`.
- **What happens:** the one-hour `created_at` window now applies to revisions,
  and the refusal has no distinct error code.
- **Recommendation:** keep the window but return a distinct code so a client
  can re-sign with server time.
- **Decision:**

### 99. A snapshot item older than the held revision is skipped

- **Status:** open
- **Where:** `crates/client/src/engine.rs`, snapshot loops.
- **What happens:** the item is skipped with a warning and the held head is
  kept; aborting the pass instead was an earlier bug.
- **Recommendation:** keep.
- **Decision:**

### 100. One skipped event rejects the whole calendar refresh

- **Status:** open
- **Where:** `crates/schedule/src/ingest.rs`; `crates/client/src/calendar_import.rs`.
- **What happens:** a single provider quirk (for example an all-day event whose
  `DTEND` equals `DTSTART`) rejects the refresh and keeps the old calendar.
  Entry 25 is one case of this.
- **Recommendation:** keep strict for now; revisit with real feeds.
- **Decision:**

### 101. Deleting a file frees no storage

- **Status:** open; checked in code (`delete_file` only writes a tombstone)
- **Where:** `crates/client/src/engine.rs`, `delete_file`; server purge route.
- **What happens:** the tombstone is charged and nothing purges ordinary files,
  so an account at its limit cannot recover space from the UI. At the byte
  limit a running timer also cannot be stopped, because the stop is a charged
  revision.
- **Recommendation:** choose between purging once the server acknowledges the
  tombstone (delete becomes final at once), purging after every device has seen
  it (needs a signal the server lacks), or an explicit "empty trash".
- **Decision:**

### 102. Revisions can be stored for free

- **Status:** open
- **Where:** `crates/server/src/storage_quota.rs`, `revision_cost_bytes`.
- **What happens:** a revision is charged only its payload and metadata
  ciphertext, both of which can be zero bytes, and revisions are not counted as
  objects. The signed envelope bytes are never charged. An account can add
  revision, payload and event rows and empty payload files without limit.
- **Recommendation:** charge the envelope length plus a fixed amount per payload.
- **Decision:**

### 103. Clipboard items cannot be deleted

- **Status:** open; checked in code (`ObjectDeleteUnsupported` for clipboard)
- **Where:** `crates/server/src/routes/objects.rs`.
- **What happens:** clipboard items leave only by the 7-day expiry or the
  100-item cap, so a secret copied by mistake stays synced until then.
- **Decision:**

### 104. Peer-to-peer sync and a downloaded-file cache

- **Status:** open
- **Where:** none yet.
- **What happens:** explicit-pairing LAN sync and keeping downloaded file bytes
  locally were planned and not built.
- **Decision:**

### 118. Clipper as a data store for vibe-coded apps and agents

- **Status:** decided; design doc to write after the scheduler PR merges.
- **What happens:** the cooking, gym and similar tools keep their data in
  their own backends, and LLM agents reach them through generic note or
  markdown tools that are not shaped to the data.
- **Decision (owner, 2026-10-06):**
  - Core tools (clipboard, files, schedule, alarms) stay built into Clipper.
    Vibe-coded apps are a frontend plus a schema over shared collections, so
    apps can read each other's data. learn-german stays out.
  - Each device keeps real SQLite tables for app collections. The server
    stores encrypted row changes and syncs them; it does not store queryable
    blobs for private data. Conflicts resolve per row.
  - The server can hold triggers (a fire time in plaintext, an encrypted
    payload) and wake a device to run the job, which gives apps a "remind me"
    tool without the server reading anything.
  - Desktop agents driven by Claude Code or Codex reach every collection,
    encrypted ones included, through the local daemon, using the owner's
    subscription.
  - A collection can be marked server-visible. Visible collections are
    plaintext on the server and reachable through a hosted MCP endpoint for
    other agent tools (claude.ai, ChatGPT). Agents may read and write them
    under per-collection read or read-write scopes, with schema checks on
    every write, version checks on updates, soft deletes, a change log with
    undo, and a separate rate limit. Making a collection visible or private
    again shows a warning: the server keeps what it has seen.
  - No in-app chat. Agents work through desktop Claude Code or Codex, or
    through the hosted MCP endpoint on visible collections.

### 150. The gym starter library is written by the first device that opens the gym

- **Status:** fixed in the working tree.
- **Where:** `crates/gym/src/starter.rs`,
  `crates/mobile-uniffi/src/gym.rs` (`gym_seed_starter_library`),
  `crates/client/src/app_data_sync.rs` (`app_data_downloaded`).
- **What happened:** a newly signed-in device that opened the gym before its
  first app-data download saw empty tables and wrote the starter library
  again, and last write wins then replaced the user's edits to starter
  exercises and workouts.
- **Decision:** the app writes the starter library only after the device has
  finished one app-data download since it unlocked, and only when it has no
  exercises, workouts, sessions or sets. Until then the gym asks again every
  two seconds.

## Code and features

### 26. `UnparseableRule` carried its reason as a string

- **Status:** fixed in `a3d0ee4`
- **Decision:** fix (owner): one typed variant per reason.

### 27. `RecurrenceEngine` was a trait with one implementation

- **Status:** fixed in `d404d58`
- **Decision:** fix (owner): inherent methods on a `RecurrenceEngine` struct.

### 28. The web form cannot set "every N days, weeks or months"

- **Status:** fixed in `a850982`; checked in Chrome: every 3 days, every 2 weeks on chosen weekdays, every 2 months, the number kept across units, 0 and 70000 refused
- **Decision:** build (owner).

### 105. The composer cannot set yearly or nth-weekday rules or an end

- **Status:** open; checked in code
- **Where:** `web/src/schedule-recurrence.ts`, `buildRecurrence`.
- **What happens:** the form offers once, daily, weekly, weekdays and monthly by
  day of month (plus the interval from entry 28). Yearly rules, "second
  Tuesday", and an end by count or date exist in the domain but have no
  control; an existing end is carried over unchanged.
- **Decision:**

### 106. Undo and history browsing are not built

- **Status:** open
- **Where:** schedule UI.
- **What happens:** retained revisions make undo possible; there is no UI for
  undo or for browsing history.
- **Decision:**

### 107. Mobile has no schedule management UI

- **Status:** open
- **Where:** `mobile/src`.
- **What happens:** the Android app shows the schedule and records actual time
  but cannot create or edit plans; the browser's responsive layout does not
  cover the React Native app.
- **Decision:**

### 108. Android calendar refresh needs the app to be opened

- **Status:** open
- **Where:** `crates/daemon/src/calendar_refresh.rs`; `mobile/src/App.tsx`;
  the Android alarm plan.
- **What happens:** desktop refreshes enabled sources hourly while logged in,
  starting shortly after login. Android refreshes enabled sources on launch and
  foreground. Both use the later of the device's local successful check time
  and the shared active fetch time. A missing time, one older than an hour, or
  one more than five minutes in the future is due. They refresh sequentially
  and continue after failures. Manual Sync always
  fetches immediately. Android registers a seven-day alarm plan; a phone whose
  app is never opened can still run out of alarms.
- **Decision:** keep desktop hourly and Android foreground refresh. Refresh and
  alarm-window renewal while the Android app stays closed remain open.

### 140. Desktop break reminders need an app that stays running

- **Status:** decided; implemented in the working tree.
- **Where:** `web/src-tauri/src/lib.rs`, `web/src-tauri/src/notifications.rs`.
- **What happens:** closing the last desktop window previously exited Tauri.
  The daemon is a plain helper executable, including in development, and its
  existing build signature uses its own linker-generated identifier without a
  bound Info.plist. Its Tokio entrypoint has no Cocoa run loop. Its notification
  delivery has not been verified. Tauri's notification plugin
  reports permission as granted on desktop without asking macOS.
- **Decision:** use Apple's UserNotifications API in the packaged Mac app.
  Closing its window hides it; Dock activation reopens it. Quit stops reminders.
  Ask permission on the first marked timer or upcoming alarm targeted at this
  Mac and disable delivery for that app run
  after denial. Unbundled development binaries do not deliver notifications.
  Sign the complete bundle after inserting the daemon. Local builds use ad-hoc
  signing with the app's bundle identifier unless a signing identity is supplied.
  Verify permission and banners in the installed build after review.

### 141. Alarm targets use registered device IDs

- **Status:** decided; implemented in the working tree.
- **Where:** `crates/schedule/src/alarm.rs`, `crates/client/src/engine.rs`,
  `web/src-tauri/src/alarms.rs`, `web/src/SchedulePanel.tsx`, `crates/cli`.
- **Decision:** an absent target keeps alarms on all Android phones. A target
  restricts delivery to that registered device. Mac targets use notifications
  with sound in the running Tauri app, including with its window closed.
  Calendar sources now have the same target choice in the web/desktop and
  Android calendar lists. An absent source target keeps imported alarms on
  all phones; a selected phone or Mac receives that source's alarms alone.
  The source's alarm switch still silences it without clearing the target.
  A removed device does not cause fallback delivery; its saved target remains
  visible. `clipper devices` lists IDs, names, platforms and the current device.
  `clipper calendar list` exposes source IDs and targets;
  `clipper calendar ring-on <source-id> <device-id|phones>` changes the target.
  Stopping an alarm from another device remains out of scope. Verify sound,
  permission and delivery with the window closed in the installed build.

### 142. Desktop notification startup and delivery had regressions

- **Status:** fixed in the working tree.
- **Where:** `web/src-tauri/src/lib.rs`, `web/src-tauri/src/alarms.rs`,
  `crates/daemon-types/src/protocol.rs`, `crates/app-types/src/lib.rs`.
- **What happened:** desktop startup read managed state before Tauri's Ready
  event ran setup, causing a launch panic. Failed alarm queries advanced the
  delivery cursor past due alarms. The IPC version did not reject daemons that
  lacked the new alarm and reminder fields. Omitting a false reminder flag
  prevented ActualView binary decoding.
- **Fix:** start notification loops in setup after managing the daemon client
  and retain the delegate until the app exits. Keep the cursor on query failure
  and retry after five seconds. IPC version 5 rejects older daemons. Always
  serialize the view's reminder flag. Behaviour tests cover query recovery and
  binary decoding with reminders enabled or disabled.

### 136. Calendar fetch ordering depends on device clocks

- **Status:** open
- **Where:** `crates/client/src/calendar_import.rs`.
- **What happens:** batches record fetch completion using each device's UTC
  clock. Incorrect clocks can order responses differently from their real fetch
  times. Equal recorded times use the raw snapshot UUID as a stable tie-break.
- **Decision:** use recorded UTC fetch times. A time more than five minutes
  ahead of the current device cannot beat its fresh fetch and makes the source
  due for refresh. Smaller clock differences and a shared time source remain open.

### 139. An unchanged check cannot prevent a slow older feed from activating

- **Status:** open
- **Where:** `crates/client/src/calendar_import.rs`.
- **What happens:** a device can fetch an older feed state and upload it slowly.
  Another device's later unchanged check writes nothing to the server, so that
  check cannot participate in shared activation ordering. The older state can
  activate until the next refresh replaces it.
- **Decision:** preserve zero server writes for unchanged feeds. A shared check
  time or another way to reject these older states remains open.

### 137. Imported alarm controls and sync status need platform integration

- **Status:** fixed
- **Where:** desktop daemon, Android and web UI, mobile bindings.
- **What happens:** web, desktop and Android show the latest check time and an
  alarm switch. Native apps offer manual Sync. Android supports adding,
  refreshing and removing calendars through UniFFI, and recalculates registered
  alarms after engine state changes, including imported batches and source
  alarm settings. Imported calendars and user-authored alarms can target a
  Mac and use notifications with sound. Untargeted calendars ring on phones
  only.
- **Decision:** desktop refreshes hourly while logged in and Android refreshes
  on launch and foreground. Browser feeds refresh through a native app; the
  browser shows the desktop refresh note and no Sync button.

### 138. Concurrent calendar cleanup and late uploads can leave old events

- **Status:** fixed
- **Where:** `crates/client/src/calendar_import.rs`.
- **What happens:** two devices cleaning the same retired batch can both read a
  live object, then compete to tombstone it. A revision conflict leaves cleanup
  unfinished. A slow uploader can also write an event after another device
  finishes and purges that batch.
- **Decision:** retry against the current head; accept a verified concurrent
  tombstone or an already-purged object. Retain source entries added by other
  refreshes, including added event IDs. A superseded uploader restores its
  retired entry; refresh also finds hydrated imported events absent from the
  current source, so interrupted retirement tracking can recover. Live workflows
  cover concurrent cleanup, a paused late upload and recovery after restarting.

### 109. Remaining alarm features from abnormalarm

- **Status:** open
- **Where:** `mobile/modules/clipper-alarm`.
- **What happens:** custom sounds and volume ramp, flashlight, skip-next,
  configurable snooze length, configurable auto-silence and a missed-alarm
  notification are not ported; ringing stops after a fixed ten minutes.
- **Decision:**

### 110. Owner QA on installed builds

- **Status:** open. The owner's Rust review is done (2026-10-06).
- **What happens:** the checks below need the installed apps against a real
  server, and have not all been run on this branch. The biometric resume round
  trip needs a device with an enrolled fingerprint, and logged-in desktop
  flows need the macOS keychain prompt answered once.
- **Decision:**

- [ ] **Schedule tab (web or desktop).**
  - Create a floating morning routine, a zoned meeting and a three-day all-day
    block.
  - Move between weeks. Check titles, times and how overlapping blocks are
    drawn.
  - Rename each block. Then change its time or recurrence and check the result.
  - Set "Every N" days, weeks and months, and switch between units. The number
    stays as typed, and 0 or more than 65,535 is refused.
  - Open an existing series that has a custom rule or an end. Check that the
    composer shows Custom when the form cannot express the rule, that clicking
    the already selected repeat option changes nothing, and that editing
    weekdays keeps the end.
- [ ] **Timers.**
  - Start a timer from a block, stop it, then start a timer with no block.
    Check the recorded-time lane.
  - Reload. Check that stopped and running timers keep their state.
  - Rename the plan a recorded session came from. Check that the session still
    shows the plan's old title.
  - In a second client, change an occurrence the first client is showing. Then
    start it from the first client. The stale start must fail and must not stop
    a timer that is already running.
- [ ] **Two clients on one account.**
  - Check that create, edit and delete reach the other client live.
  - Open the same block for editing on both clients. Save one, then save the
    other. The second save must be refused, not overwrite the first.
  - Restart each native client. Check that schedule and clipboard content come
    back, deleted objects stay deleted, and file downloads still work.
- [ ] **Calendar import (desktop).**
  - Add a disposable ICS URL and sync it. Check recurrence overrides, then an
    upstream edit and an upstream removal.
  - While a sync runs, stop a timer. It must stop at once, not after the sync.
  - In the browser, check that the synced events appear. The browser cannot
    refresh a feed; refreshing is a native operation.
- [ ] **Android alarms.**
  - Sign in. Grant notification, exact-alarm and full-screen access.
  - From desktop or web, create a block with an alarm a few minutes ahead.
    Check the count of upcoming alarms in the mirrored plan, the ringing, the
    label and dismiss.
  - Repeat with the app in the background, and again after a reboot.
  - With the phone unlocked, check the heads-up notification, then tap its
    body to open the ring screen.
  - Set a short display timeout. Leave the ring screen untouched and check
    that it stays awake.
  - Leave one alarm unanswered for ten minutes. Sound, vibration, the
    notification and the ring screen must all stop.
  - Dismiss an alarm early. Check that a later alarm still rings for its own
    full ten minutes.
  - Test an alarm due right after a reboot, before first unlock, and one due
    shortly after unlock. A later alarm ringing after the reboot does not show
    that the first case works (entry 82). Capture logs
    for any missed alarm.
- [ ] **Overnight on the POCO.** Before relying on Clipper as the morning
      alarm, repeat the alarm checks overnight on the POCO with the HyperOS
      settings configured. Keep abnormalarm available until this passes.

### 111. Legacy-state cleanup code remains

- **Status:** open; checked in code
- **Where:** `users.encryption_salt` (a wrapped empty value written at every
  registration); `crates/daemon/src/keychain.rs` strips a legacy `passphrase`
  field; `web/src/backend/index.ts` removes `clipper.session.v1`;
  `mobile/src/backend.ts` deletes the v1 credential slots;
  `discard_legacy_file_store` in `crates/client/src/local_store.rs`.
- **What happens:** AGENTS.md says not to keep compatibility code for abandoned
  local state, since nothing is deployed.
- **Recommendation:** drop the column (a migration) and the cleanup code.
- **Decision:**

### 112. `hydrate_ciphertext_cache` replaces memory without the store's lock

- **Status:** open; checked in code
- **Where:** `crates/client/src/local_store.rs`, `hydrate_ciphertext_cache`.
- **What happens:** nothing races it today, because it runs inside
  `finish_auth` before the new WebSocket starts.
- **Recommendation:** take the `sync` lock so that stays true.
- **Decision:**

### 113. Shared delete paths call a no-op on native

- **Status:** open; checked in code
- **Where:** `crates/client/src/local_store.rs`, `discard_cached_payload`.
- **What happens:** the native version does nothing (SQLite removes the payload
  with its row), but the shared delete and absence paths call it before every
  marker write.
- **Recommendation:** move the browser payload removal into the browser marker
  write and drop the shared calls.
- **Decision:**

### 114. Adding an object kind touches many files by hand

- **Status:** decided
- **What happens:** a new kind needs edits across api-types, app-types, the
  client, server, daemon IPC, three adapters and both UIs. A kind registry and
  an `AppState` reshape were proposed.
- **Decision:** deferred; not part of this branch (owner).

### 127. Some Rust dependencies must stay on older version lines

- **Status:** decided
- **Severity:** low.
- **Where:** workspace and crate `Cargo.toml` files.
- **What happens:** OPAQUE 4.0.1 requires digest 0.10 and Rand 0.8. HMAC
  0.13 and HKDF 0.13 require digest 0.11. SQLx 0.9 requires
  `libsqlite3-sys < 0.38`, while rusqlite 0.40 requires 0.38. The React
  Native UniFFI generator and runtime require UniFFI 0.31. Calcard's ahash
  dependency needs getrandom 0.3's browser feature enabled on that version.
- **Decision:** keep direct SHA-2 0.10, HMAC 0.12, HKDF 0.12, the OPAQUE
  Rand alias on 0.8, rusqlite on 0.39, UniFFI on 0.31, and the getrandom
  browser-feature alias on 0.3. Upgrade these when their upstream users
  support the newer lines. Other Rust dependencies were upgraded.

### 128. Nightly compilation exceeded the async trait recursion limit

- **Status:** fixed in `f420fc8`
- **Severity:** low; compilation warning only.
- **Where:** `crates/client/src/lib.rs`, `crates/daemon/src/main.rs`.
- **What happened:** the nightly compiler used by `cargo-udeps` exceeded
  its default trait recursion limit while checking `Send` on the nested
  calendar-sync futures and the client's logout test future.
- **Decision:** raise those two crates' compilation recursion limits to 256. The unused-dependency scan then passed without these warnings.

### 129. Updated web dependencies resolve a second React through peer contexts

- **Status:** fixed in `9279752`
- **Severity:** high; the web app can fail to render.
- **Where:** `web/vite.config.ts`.
- **What happened:** the React 19.3 dependency graph included the mobile
  React 19.2.3 in the production bundle despite the React Native Web aliases.
- **Decision:** deduplicate React and React DOM at the web app root. The
  production output contains one rendered React core module.

### 130. The shared package does not declare its AbortSignal types

- **Status:** fixed in `9279752`
- **Severity:** low; the shared package type check fails.
- **Where:** `packages/shared/tsconfig.json`.
- **What happened:** the backend contract uses `AbortSignal`, but the package
  declares only the ECMAScript library.
- **Decision:** include the DOM library, as the web and mobile consumers do.

### 131. The flake pnpm launcher cannot start pnpm 12

- **Status:** decided; hold on pnpm 11.28.2
- **Severity:** medium; every npm command fails with pnpm 12 pinned.
- **Where:** the flake's pnpm launcher and `packageManager` pins.
- **What happened:** pnpm 12.9.1 ships a native executable. The current
  launcher executes it with Node, which throws a JavaScript syntax error.
- **Decision:** use the latest stable pnpm 11 release. Upgrade the flake's
  package-manager bootstrap before switching to pnpm 12.

### 132. Tamagui development prebundling cannot resolve inline-style-prefixer

- **Status:** fixed in `20bbce5`
- **Severity:** low; Vite reports a dependency optimization failure.
- **Where:** `web/package.json`.
- **What happened:** Tamagui adds `inline-style-prefixer` to Vite's dependency
  optimizer, but pnpm exposes it only inside React Native Web's dependency tree.
- **Decision:** declare the optimizer's dependency directly in the web package.
  All three required development transforms now pass without the warning.

### 133. Metro's config properties are read-only in the updated types

- **Status:** fixed in `dc05a06`
- **Severity:** low; the workspace JavaScript type check fails.
- **Where:** `mobile/metro.config.js`.
- **What happened:** assignments to `maxWorkers` and `resolveRequest` fail
  against Metro's updated config types. Different Expo Metro wrapper versions
  also disagree on the resolver context type.
- **Decision:** use `mergeConfig` and one Expo Metro 56.1.0 wrapper version.
  Keep the macOS worker limit and the lib0 Web Crypto resolver. The workspace
  type check and the Android JavaScript export pass.

### 134. The configured npm linker differs from the installed linker

- **Status:** open; checked during the npm dependency upgrade
- **Severity:** low; the configuration is misleading.
- **Where:** `.npmrc`.
- **What happens:** `.npmrc` requests `node-linker=hoisted`, but pnpm 11.28.2
  reports no configured `nodeLinker` and `node_modules/.modules.yaml` records
  `isolated`.
- **Decision:** keep the working installed layout during this upgrade. Review
  the workspace configuration before changing the linker.

### 144. Entity generation replaces direct device-user relations with row-table joins

- **Status:** fixed
- **Severity:** medium; generated relation queries can return the wrong rows.
- **Where:** `scripts/server-entities.ts`.
- **What happens:** the row table has user and device foreign keys. SeaORM
  codegen treats it as a join table and replaces the existing direct
  device-user relations with relations through app-data rows.
- **Decision:** preserve direct relations in the generator's post-processing.
  Regenerate entities instead of editing their relation implementations.

## Docs

### 29. Doc claims the code did not satisfy

- **Status:** fixed in `6e1059b` and `479ceaf`
- **What happened:** the resource-limits doc, the schedule model review, the
  Android alarm doc, the backlog and a migration comment each stated something
  the code does not do.
- **Decision:** fix (Claude).

### 115. Docs point to the wrong issue list or describe old behaviour

- **Status:** fixed by the docs consolidation
- **What happens:** README, SECURITY.md and CONTRIBUTING.md call
  `docs/rust-code-review.md` the list of known issues; SECURITY.md describes a
  "plaintext local clipboard cache" (the cache is encrypted) and an "Accepted /
  Intentional Tradeoffs" section that does not exist. `rust-code-review.md` says
  duplicate registration is hidden; it returns 409. `ws-sync-flow.md`,
  `user-data-scoping.md`, `opaque.md`, `local-at-rest-encryption.md` and the
  schedule model doc contain the stale claims listed in the docs consolidation.
- **Decision:** fix (in the docs consolidation).
