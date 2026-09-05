# Scheduler review and QA handoff

Review of PR #1 (`schedule-module`), September 2026. Read this first for the
current state; `schedule-plan.md` preserves the longer product discussion and
implementation history. Web UI QA is complete; Rust review precedes the
remaining installed-app QA and merge.
Updated for the follow-up revision-aware schedule model. Current automated
validation and earlier interactive QA evidence are separated below.

## Assessment

The foundations are appropriate for this product: one Rust recurrence engine
serves every client, floating/zoned/date-only time is explicit, recurring series
are stored once, and planned time is separate from actual time. Kotlin consumes
concrete alarm times, so boot-time reliability does not depend on starting React
Native or duplicating recurrence logic. The encrypted-object boundary is kept.
Upcoming alarm labels and fire times deliberately live in device-protected
storage so they are available before unlock; that tradeoff matches the recorded
product decision and is separate from the encrypted schedule store.

The weakest part of the original implementation was integration coverage. The
branch combined the scheduler with a substantial encrypted-object revision
overhaul (D6). Several operations still behaved like the earlier immutable-object
design, even though individual crates and existing tests passed. D6 deserves its
own careful reading within this PR: it affects files and sync as well as schedule.
The broad kind registry refactor can remain deferred; more abstraction would not
have prevented these bugs.

The follow-up review also replaced native JSON object files with SQLite, keeping
durable revision anchors separate from disposable cached content. This addresses
the accumulating deleted-marker files and their startup read cost, and makes
record/payload writes transactional. It is a substantial client-storage change
alongside D6, so native restart, resync and deletion need another QA pass on the
new build. See [local-store-plan.md](local-store-plan.md) for the design and limits.

This is a useful first scheduler, not the entire original calendar/alarm wish
list. In particular, richer recurrence exists in the domain and feed parser than
the composer exposes. Deferred UI and connector work is listed below so it does
not become a surprise during QA.

## Changes made during review

- Merged `main` and resolved the schedule-plan conflict, keeping the later product
  decisions.
- Fixed floating occurrence keys so clicking a floating block can start a timer.
  Timer stop now appends revision 2 to the same object; local timer commands are
  serialized, and an invalid start cannot stop the current timer.
- Scoped streamed payload completion and upload state to the correct revision.
  Large revisions now complete and emit the appropriate live update. Oversized
  schedule records are rejected before upload using the same bound as downloads.
- Preserved accepted revision anchors through deletion, 404 responses and
  reconciliation. Rechecked incoming revisions under the storage lock, rejecting
  rollback, same-revision replacement, and incorrect immediate parent links.
  Snapshot pages must advance in order within their watermark.
- Made calendar ingest update existing objects by revision, use deterministic
  source/UID identities, and retain meetings when a feed is only partially parsed.
  Explicitly empty calendars still cancel upstream-deleted events. Removing a
  source hides its imported events while retaining history.
- Added provider overrides (`EXDATE`, `RDATE`, `RECURRENCE-ID`), timed/date-only
  `DURATION`, absolute duration across time zones, and rejection of unknown zones.
  Fixed non-hour DST gaps and recurrence limits for old series. Malformed feeds,
  oversized responses, unsafe redirects and private URL leakage are bounded or
  rejected.
- Made title-only edits preserve time zone, sub-minute start, duration, finite or
  custom recurrence, references and multi-day length. A stale open edit is
  rejected with instructions to reopen it. Prevented late week requests from
  replacing the current week and gave overlapping blocks separate visible lanes.
- Replaced the unsupported wasm clock used by clipboard suppression with
  `web-time`. Added native session-resume exports so mobile can retain revocable
  session material instead of a passphrase, and discard definitively rejected
  sessions while retaining them after transient network failures.
- Added Android notification/full-screen permission controls, refreshed the
  seven-day alarm plan on foreground, and made explicit logout clear alarms and
  the plaintext mirror. Direct Boot testing exposed an unavailable system sound;
  a bundled fallback and asynchronous playback-error handling address that path.
  Queued deliveries now match the occurrence and fire time in the current mirror,
  so replacing a plan cannot ring the new occupant of an old alarm slot.
- Added a live server regression to CI and small DST/overlap layout tests to the
  web check. Mobile checks now generate their ignored bridge inputs from a host
  Rust library, so they work on a fresh checkout without an Android SDK. Updated
  the envelope and security documentation to match the code.
- Follow-up recurrence fixes display unsupported cadences as Custom, make
  clicking the selected cadence a no-op, and preserve recurrence end conditions
  when changing repeat settings. Weekly cadences always start on Monday. The interval is preserved when
  the recurrence unit stays the same. Raw rules now reject control characters
  that could inject another calendar property.
- Follow-up native storage fixes preserve anchors when cached content cannot be
  read, and move objects, payloads and anchors into SQLite. Deleted objects leave
  an anchor row rather than a marker file; hydration skips departed objects,
  reconciliation uses indexed queries, and deleting cached objects also reclaims
  their payloads. Anchors are retained indefinitely. The browser keeps its
  bounded localStorage backend and best-effort history checks.

## Plans across edits and historical recordings

A series ID identifies the continuing schedule; an immutable object revision
identifies the definition that gave one occurrence its meaning. `RecurrenceId`
remains the original date/time, not a revision number. Occurrences are computed,
but their identity is persisted in overrides and linked actual records.

- A standalone `OccurrenceOverride` contains its pure `OccurrenceOverrideData`
  plus the exact base schedule reference: storage `ObjectId`, revision number,
  and signed envelope body hash. Before applying it to a newer definition, the
  client checks that the series identity, scheduled span and recurrence match.
  Title, linked-object and alarm-setting edits preserve overrides. Structural
  edits with local overrides are blocked pending an explicit override-editing
  workflow; silently retargeting an old occurrence is not supported. If a synced
  definition is incompatible, or the base revision cannot be loaded, expansion
  skips that series and publishes a warning in the Schedule UI. Other series
  continue to appear and generate alarms; affected alarms are omitted rather
  than guessed.
- Provider overrides are embedded in the imported event object. They have no
  independent envelope revision. Pinning the imported object revision captures
  its provider overrides as well. Standalone local overrides have their own
  revisions and are treated separately.
- A linked actual pins the schedule revision and the applied standalone override
  revision, if any, when recording starts. It also stores the original occurrence
  identity, observer timezone and resolved planned UTC bounds. These preserve the
  effective plan after travel or timezone database updates without duplicating
  the entire schedule. Stopping the timer preserves that context.
- Calculated occurrence views carry the revision context used to render them.
  Timer start validates that context and occurrence against its local snapshot
  before stopping an existing
  timer. A stale click must fail rather than start against a substituted plan.
- Historical definitions use revision-specific authenticated reads and bounded
  payload validation. They are checked against the pinned object ID, revision
  and body hash and decrypted using the revision's AAD. Historical reads do not
  install an old head, update sync cursors, or weaken accepted-history anchors.
  Missing or purged history is unavailable, never replaced with today's plan.
  Already-cached history may remain readable after server purge until the
  memory cache is cleared; purge does not retroactively erase client memory.
- The Rust `recorded_plan(actual_id)` read returns the pinned definition, applied
  override and captured context. The history cache holds at most 64 entries and
  is scoped to the authenticated session epoch. Routine state publication does
  not wait for historical network requests, so titles can initially show
  “Historical plan unavailable”; an explicit actuals-window read resolves them.
  The historical-detail UI remains deferred.

The calendar and alarm plan still expand the latest definition. Revision pins
provide a historical comparison for an actual, not a complete historical
calendar. “Change this and all future occurrences” and effective-date series
splits remain unimplemented. Actual manual entry, reassociation, correction and
historical-detail UI also remain future work.

## Validation

The revision-aware change passes the Rust workspace tests (including 73 domain
and 93 server tests), the isolated live server/client regression, workspace
Clippy with warnings denied, wasm and mobile bridge checks, web type/lint/tests
and standalone web build. Focused additions cover cosmetic versus structural
edits, exact signed revision pins, stale timer selections preserving the running
timer, captured floating bounds, provider overrides, and reconstruction after
edits and tombstones. History tests cover ownership, pending revisions, retention,
purge, and preservation of the current sync head. This change has not received
a new interactive Android or Tauri QA pass.

The broad build and UI results below describe the earlier review build, before
the follow-up recurrence and SQLite changes. They are useful baseline evidence,
not a claim that the current branch received the same end-to-end QA again.
The follow-up commits report recurrence regression tests and a browser check of
editing a fortnightly series, native storage regression coverage, a passing live
server integration test, and SQLite cross-compilation for Android. This document
update checked the changes and recorded evidence; it did not independently rerun
those tests. Rebuild native clients before testing the SQLite cutover.

The live regression starts its own server, temporary database and three device
caches. It exercises OPAQUE register/login, floating times in Berlin and Tokyo,
timer start/stop, inline and streamed revisions, stale-write rejection, feed
overrides/update/deletion, file and schedule tombstones, restore, cold-device
sync, session resume and token revocation. Run it with:

```sh
cargo build -p clipper-server
cargo test -p clipper-client live_schedule -- --ignored
```

Chrome and Playwright WebKit QA use the actual wasm backend and a separate local
server/account. Chrome covers create/start/stop, advanced stored-rule preservation
through the composer, stale-edit rejection, multi-day editing, week-boundary
overlap and visible overlap lanes. Both engines pass alarm-toggle persistence and
clipboard send/payload retrieval without errors. Playwright WebKit is additional
browser coverage, not a test of the native Tauri command bridge.

The local sandbox's addresses, test login and restart commands are in the
gitignored `data/scheduler-qa/README.md`. It uses ports 53885/18787 and its own
server database/secret, leaving the existing development server intact.

Local checks passed: workspace tests and Clippy with warnings denied; the live
regression above; wasm, web and mobile checks; standalone web build; Android
release packaging; and the macOS Tauri bundle. The unused-dependency check
passed, entity generation produced no entity changes, and formatting passed.
The dependency audit passed its existing policy with 13 filtered advisories;
that is not a claim that all dependencies are vulnerability-free.
Linux CI also surfaced GHSA-2883-xcg3-v3hh; the transitive `js-yaml` dependency
is pinned to `4.3.2` in the workspace override ([advisory](https://github.com/advisories/GHSA-2883-xcg3-v3hh)).

On the API 36 emulator, the final release APK passed notification permission
setup, background ringing, lock-screen display and looping playback, Dismiss,
and explicit logout canceling a future alarm. A separate PIN-protected reboot
test verified `RUNNING_LOCKED`, re-arming from `LOCKED_BOOT_COMPLETED`, and
delivery before the first unlock at 23:49:33 CEST on September 8. The native ring
screen appeared and MediaPlayer was actively looping the bundled APK sound on
the alarm audio stream. This verifies the playback path; it is not a physical
device loudness or overnight reliability test.
That alarm fired more than four minutes after the boot receiver ran; this did
not test the immediate post-boot foreground-service restriction described below.

The Abnormalarm comparison led to three Android ring-lifecycle changes:
`FLAG_KEEP_SCREEN_ON` now applies on every supported API level; each new alarm
starts a fixed ten-minute auto-silence window (matching Abnormalarm's default);
and tapping the ringing notification opens the native ring screen. Dismissal
and auto-silence stop audio/vibration, remove the notification, release the wake
lock and close the ring screen. A new delivery resets the timeout and renews the
wake lock. Auto-silence is not yet configurable and does not post a missed-alarm
notification.

An unlocked phone may show a heads-up notification instead of opening the full
ring screen. This is [Android's intended full-screen-intent behavior](https://source.android.com/docs/core/permissions/fsi-limits),
not evidence that the alarm failed. Clipper retains that behavior without adding
Abnormalarm's overlay permission; the notification offers Dismiss and opens the
ring screen when tapped.

A focused API 36 debug-APK check on September 10 exercised the changed native
alarm path using a temporary instrumentation fixture (no server/Rust scheduling
flow). The alarm fired at 19:20:00 CEST and auto-silenced at 19:30:00. The ring
screen stayed awake beyond a 15-second system display timeout. Notification-body
tapping reopened the screen. At timeout, the service, MediaPlayer, vibration,
notification and wake lock were gone and the ring activity closed. This checks
emulator lifecycle behavior, not physical-device loudness or overnight reliability.
A second alarm fired with the launcher visible and phone unlocked: it rang
without taking over the screen, tapping its notification opened RingActivity,
and notification Dismiss stopped the service and closed the activity.
The module's Kotlin compilation and debug APK build passed. Android lint could
not complete because its Kotlin analysis crashed in `react-native-worklets`
(`Cannot find a KaModule for the VirtualFile`).

The Tauri bundle starts, but logged-in native Tauri UI flows were not exercised:
the computer-use harness lacked macOS screen/control permissions. The emulator
had no enrolled biometric, so manual-login fallback was exercised while the OS
biometric SecureStore save/resume round trip remains for owner QA. Rust session
resume and revoked-token rejection are covered by the live regression.

The revised development payloads and timer IPC intentionally have no legacy
compatibility path. Rebuild/restart the server and clients together and regenerate
old QA recordings/overrides before exercising the new historical comparisons.

## Owner QA sequence

1. Open the Schedule tab. Create a floating morning routine, a zoned meeting, and
   a three-day all-day block. Move between weeks; check titles, times and overlap.
   Rename each, then explicitly change time or recurrence and verify the result.
   For an existing custom/finite series, check that Custom is shown when needed,
   clicking the selected cadence changes nothing, and editing weekdays retains
   the end condition and the interval when the recurrence unit is unchanged.
2. Start a timer from a block, stop it, then start untracked time. Check the
   actual-time lane. Reload and verify stopped/running state is retained.
   Rename the originating plan and
   check that the recorded session retains its historical plan title. In a
   second client, change a displayed occurrence before starting it from the
   first client; a stale click must not stop an already-running timer.
3. Open a second client with the same account. Check create/edit/delete live
   propagation. Open an edit on both clients; save one, then try saving the other.
   On rebuilt native clients, restart and verify schedule and clipboard content
   returns, deleted objects stay deleted, and file downloads still work. The
   SQLite cutover discards the old object/payload cache and refetches content;
   allow the first sync to finish before judging missing data.
4. In the desktop app, add a disposable ICS URL and sync it. Check recurrence
   overrides, an upstream edit and removal. The browser renders synced events;
   refreshing the external feed is a native operation.
5. Sign in on Android, grant notification/exact-alarm/full-screen access, and create an
   alarm-enabled block from desktop/web a few minutes ahead. Verify mirror count,
   ringing, label and dismiss. Repeat after backgrounding and rebooting.
   With the phone unlocked, verify the heads-up notification and tap its body
   to open the ring screen. With a short normal display timeout, leave the ring
   screen untouched and verify it stays awake. Leave one alarm unanswered for
   ten minutes: sound/vibration, notification and ring screen should all stop.
   Also dismiss an alarm early and verify a later alarm still gets its own full
   ten-minute window.
   Separately test an alarm due immediately after reboot, before first unlock,
   and one shortly after unlock. A later post-reboot success does not cover the
   known foreground-service attribution issue; capture logs for a missed alarm.
6. Before relying on this as the morning alarm, repeat overnight on the POCO with
   HyperOS settings configured. Keep abnormalarm available during this acceptance.

## Remaining scope and limits

The maintained checklist of open decisions, missing workflows and follow-up
work is [scheduler-backlog.md](scheduler-backlog.md). In particular, override
resolution after a timing/recurrence edit has backend guards but no resolution UI.

- No Google/Zoho OAuth connector or outward publishing. ICS refresh is manual
  and native; per-client source opt-in and background sync are future work.
- No mobile schedule-management UI, single-occurrence override UI, undo/history
  UI, or the remaining abnormalarm features (snooze, custom sounds/ramp,
  flashlight, skip-next and widget).
- Android receives a seven-day plan. An app that never refreshes eventually runs
  out of planned alarms. Emulator checks do not establish POCO/HyperOS overnight
  reliability or behavior across every Android version and permission state.
- The Android post-boot ringing failure remains unresolved. A media-playback
  foreground service can be refused when Android attributes its start to
  `BOOT_COMPLETED`, including a reported exact-alarm delivery during that
  temporary attribution window. There is no reliable recovery in the current
  ring path. The window is not a universal 45-second rule, and the behavior for
  `LOCKED_BOOT_COMPLETED` still needs testing. See
  [rn-alarm-absorption.md](rn-alarm-absorption.md). Treat this as an open alarm
  reliability finding before relying on the app for wake-up alarms.
- Revision anchors protect a device's accepted history. Fresh installs cannot
  detect a server presenting an older valid history, and skipping multiple
  revisions does not verify every intermediate parent. Remote deletion events
  do not supply a signed tombstone body. See `object-envelopes.md` for the precise
  guarantees; this is not a server-transparency protocol.
- Native anchors now survive cache eviction in their own SQLite table, but
  deleting or recreating the database loses them. The development cutover does
  not migrate old anchors, and schema-version changes currently recreate the
  database. The browser offers no durable accepted-history guarantee: eviction
  can lose anchors, and a hostile web origin can replace the checking code.
  Retained browser checks still help when an honest web client uses a separately
  operated API.
- Whole-feed ingest and retained history still need a storage/retention policy.
  The browser's localStorage quota is small. Feed and record bounds avoid
  unbounded single responses; they do not solve lifetime accumulation.
  SQLite removes the deleted-file scan cost, but native anchor rows still grow
  with accepted object history. Hydration still loads all held objects; bounded
  display queries and anchor-size measurements remain follow-ups.
- The shared TypeScript recurrence union still omits Rust's Raw variant.
  Aligning that contract remains a follow-up; the composer safeguards do not
  resolve the type mismatch.
- Recurrence expansion has a 65,535 generated-history ceiling and a 10,000
  in-window ceiling. This accommodates decades of daily recurrence while
  rejecting very dense old rules. The upstream `rrule` library also bounds empty
  iteration periods but does not always report that internal stop; exotic sparse
  raw rules may therefore yield an incomplete/empty result. The tested daily,
  weekly, monthly and yearly patterns do not hit that limitation.
- Concurrent timer starts on separate devices are not a global transaction.
  Local serialization prevents duplicate local commands; cross-device timer
  conflict UX remains a follow-up.
- Existing security decisions and accepted risks remain in `security-review.md`.
  This review hardens the touched paths and selected documented issues; it is not
  a new comprehensive security audit.
