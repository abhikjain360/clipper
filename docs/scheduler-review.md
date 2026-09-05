# Scheduler review and QA handoff

Review of PR #1 (`schedule-module`), September 2026. Read this first for the
current state; `schedule-plan.md` preserves the longer product discussion and
implementation history. Owner QA comes before the code walkthrough and merge.

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
- Added provider exceptions (`EXDATE`, `RDATE`, `RECURRENCE-ID`), timed/date-only
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

## Validation

The live regression starts its own server, temporary database and three device
caches. It exercises OPAQUE register/login, floating times in Berlin and Tokyo,
timer start/stop, inline and streamed revisions, stale-write rejection, feed
exceptions/update/deletion, file and schedule tombstones, restore, cold-device
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

On the API 36 emulator, the final release APK passed notification permission
setup, background ringing, lock-screen display and looping playback, Dismiss,
and explicit logout canceling a future alarm. A separate PIN-protected reboot
test verified `RUNNING_LOCKED`, re-arming from `LOCKED_BOOT_COMPLETED`, and
delivery before the first unlock at 23:49:33 CEST on September 8. The native ring
screen appeared and MediaPlayer was actively looping the bundled APK sound on
the alarm audio stream. This verifies the playback path; it is not a physical
device loudness or overnight reliability test.

The Tauri bundle starts, but logged-in native Tauri UI flows were not exercised:
the computer-use harness lacked macOS screen/control permissions. The emulator
had no enrolled biometric, so manual-login fallback was exercised while the OS
biometric SecureStore save/resume round trip remains for owner QA. Rust session
resume and revoked-token rejection are covered by the live regression.

## Owner QA sequence

1. Open the Schedule tab. Create a floating morning routine, a zoned meeting, and
   a three-day all-day block. Move between weeks; check titles, times and overlap.
   Rename each, then explicitly change time or recurrence and verify the result.
2. Start a timer from a block, stop it, then start untracked time. Check the
   actual-time lane. Reload and verify stopped/running state is retained.
3. Open a second client with the same account. Check create/edit/delete live
   propagation. Open an edit on both clients; save one, then try saving the other.
4. In the desktop app, add a disposable ICS URL and sync it. Check recurrence
   exceptions, an upstream edit and removal. The browser renders synced events;
   refreshing the external feed is a native operation.
5. Sign in on Android, grant notification/exact-alarm/full-screen access, and create an
   alarm-enabled block from desktop/web a few minutes ahead. Verify mirror count,
   ringing, label and dismiss. Repeat after backgrounding and rebooting.
6. Before relying on this as the morning alarm, repeat overnight on the POCO with
   HyperOS settings configured. Keep abnormalarm available during this acceptance.

## Remaining scope and limits

- No Google/Zoho OAuth connector or outward publishing. ICS refresh is manual
  and native; per-client source opt-in and background sync are future work.
- No mobile schedule-management UI, single-occurrence override UI, undo/history
  UI, or the remaining abnormalarm features (snooze, custom sounds/ramp,
  flashlight, skip-next and widget).
- Android receives a seven-day plan. An app that never refreshes eventually runs
  out of planned alarms. Emulator checks do not establish POCO/HyperOS overnight
  reliability or behavior across every Android version and permission state.
- Revision anchors protect a device's accepted history. Fresh installs cannot
  detect a server presenting an older valid history, and skipping multiple
  revisions does not verify every intermediate parent. Remote deletion events
  do not supply a signed tombstone body. See `object-envelopes.md` for the precise
  guarantees; this is not a server-transparency protocol.
- Whole-feed ingest and retained history still need a storage/retention policy.
  The browser's localStorage quota is small. Feed and record bounds avoid
  unbounded single responses; they do not solve lifetime accumulation.
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
