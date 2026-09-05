# Schedule Plan

Design notes for adding a time-management layer to Clipper: schedulable time
blocks, recurring events, alarm dispatch into
[abnormalarm](https://github.com/abhikjain360/abnormalarm), and two-way sync
with Google Calendar and Zoho Calendar.

**Status: design settled, implementation not started.** No code has been
written. D1-D11 in the [Decision Log](#decision-log) are agreed; D10 is a
proposal awaiting sign-off and is marked as such. Everything under
[Findings](#findings) is verified against the code as of 2026-09-07 and is meant
to save the next reader from re-deriving it. Start at
[Next Steps](#next-steps).

## The ask

A personal time-management system inside Clipper:

- Time allocated in 5- or 10-minute blocks, scheduled across a day.
- Repeating events at a custom cadence: daily, weekly, a specific weekday, a
  specific month of the year, a specific day of the month, and so on.
- Selected events can raise an alarm through abnormalarm.
- Two-way integration with Google Calendar.
- Two-way integration with Zoho Calendar (custom-domain email).

## Findings

### Clipper has two storage classes, and they behave very differently

**Encrypted objects** (`ObjectKind::Clipboard`, `ObjectKind::File`). Content is
sealed with `K`, an XChaCha20-Poly1305 key derived from the user's OPAQUE export
key. The server never learns `K` and stores only ciphertext plus a signed
envelope. See `docs/object-envelopes.md`.

**Server-visible objects** (`ObjectKind::Collab`). Y.Doc state and title are
plaintext columns, because the server has to merge CRDT updates and render a doc
list. `CLAUDE.md` calls collab "the one server-visible object kind", and the
justification is the sharing requirement, not convenience.

Relevant consequences for a schedule:

- **Encrypted objects are immutable.** `ObjectEnvelopeOperation` has a single
  `Create` variant, and the `ObjectEventType::Updated` doc comment in
  `crates/api-types/src/lib.rs` says so outright. The only emitters of `Updated`
  today are the collab rename handlers (`crates/server/src/routes/collab.rs:280`
  and `:305`). Immutability is also what gives the current model rollback
  resistance for free: there is no older valid ciphertext for a server to replay.
- **The envelope AAD binds identity.** `object_id`, `object_type`,
  `object_version`, `source_device_id`, `created_at`, `operation`, and the
  payload-id set all sit in the AAD, so none of them can be retargeted without
  breaking the AEAD tag. Any mutation scheme has to extend this deliberately.
- **`GET /api/objects?kind=` already filters by kind**
  (`crates/server/src/routes/objects.rs:903-929`), so a new kind gets a snapshot
  endpoint without new routes.
- **Retention only touches clipboard.** `crates/server/src/cleanup.rs` filters on
  `Kind.eq("clipboard")` throughout, so a new kind is durable by default, like
  files.
- **Sync is generation-based.** Snapshot over HTTP up to a watermark, live
  WebSocket events after it, per-object create-sequence ordering. A new kind
  needs its own snapshot pass in the client, mirroring the file snapshot.
  `docs/ws-sync-flow.md` is the reference, including its recorded flaws (R18
  delete-marker lifetime, R21 missing materialization retry).

### Where a new object kind has to be wired

Following the path collab docs took:

| Layer | Files |
| --- | --- |
| Wire contract | `crates/api-types/src/lib.rs` (`ObjectKind`, request/response types) |
| Display state | `crates/app-types/src/lib.rs` (`AppState` gains a field) |
| Sync engine | `crates/client/src/engine.rs`, `crates/client/src/local_store.rs`, `crates/client/src/api_client.rs` |
| Server | `crates/server/src/routes/objects.rs`, route table in `crates/server/src/main.rs` |
| Daemon IPC | `crates/daemon-types/src/protocol.rs` (`DaemonCommand`), `crates/daemon/src/handler.rs` |
| Adapters | `crates/web-wasm`, `crates/mobile-uniffi`, `web/src-tauri` |
| Frontend | `packages/shared`, `web/src`, `mobile/src` |

`IPC_AUTH_VERSION` in `crates/daemon-types/src/protocol.rs` must be bumped
whenever a type crossing the daemon boundary changes; an old daemon can outlive
an app update.

### abnormalarm has no external surface, but its calendar layer is the seam

Verified against `app/src/main/AndroidManifest.xml`: exactly three components
are exported, and none of them accept data — `MainActivity` (launcher),
`HomeClockWidgetConfigActivity`, and the `HomeClockWidget` app-widget receiver.
Every alarm-path receiver and service is `exported="false"`. The Room database
is app-private with no `ContentProvider`. Nothing outside the app can reach it
today.

What it does have is a generic, string-keyed calendar ingestion pipeline:

- `data/calendar/CalendarProviders.kt` is two constants, `DEVICE = "device"` and
  `GOOGLE = "google"`.
- `CalendarRepository.CalendarAlarmCandidate` carries
  `(provider, calendarId, eventKey, instanceStartMillis, fireTimeMillis, title)`.
- `CalendarSync.sync(nowMillis)` collects candidates from each enabled backend
  over a rolling `HORIZON_HOURS = 48` window and reconciles them into alarm rows
  keyed by
  `"$provider:$calendarId:$eventKey:$instanceStart:${fireMillis / 60_000L}"`.
  Matching rows are updated in place for label/time only, so a user's
  per-instance dismiss, `firedCount`, and `RingSettings` survive re-sync.
  Unmatched `CALENDAR` rows are cancelled and deleted, which is how remote
  deletes propagate. A provider whose permission or token is missing is skipped
  rather than swept.
- Each instance materializes as `RepeatRule.OnceOnDate(date)` — a fixed-date
  one-shot. Recurrence lives in the source feed; abnormalarm never expands it.
- Refresh triggers are a `ContentObserver` (device provider only), boot, a
  ~5-hour `CalendarSyncWorker`, app foreground, and manual refresh. Remote
  changes can therefore lag by hours.
- `AlarmSource.CALENDAR` rows are read-only in the UI, and the Google path drops
  all-day events (`startMillis()` returns null when only `start.date` is set).

Adding a third provider is a small, well-shaped change on that side. The
transport into it is an open question — see decision 4.

### Google Calendar and Zoho Calendar

| | Google Calendar | Zoho Calendar |
| --- | --- | --- |
| Protocol | REST v3 | REST v1, and CalDAV |
| Auth | OAuth2; installed-app loopback flow fits a self-hosted daemon | OAuth2 (`ZohoCalendar.event.ALL`) for REST; app-specific password for CalDAV |
| Write scope | `https://www.googleapis.com/auth/calendar` | `ZohoCalendar.event.ALL` |
| Incremental sync | `events.list` with a stored `nextSyncToken` | CalDAV `sync-collection` (RFC 6578) if supported; otherwise poll |
| Push | `events.watch` → HTTPS webhook. Needs a verified domain, delivers a bare "something changed" ping, and Google documents it as lossy | None documented |
| Recurrence | `RRULE` on the master event; instances carry `recurringEventId` + `originalStartTime`; `singleEvents=true` expands server-side | `rrule` parameter, RFC 5545 shaped |
| CalDAV base | Deprecated in practice for this use | `https://calendar.zoho.com/` (regional: `.eu`, `.in`) |

Two things follow. Push notifications are an optimization, not a foundation —
polling has to work regardless, since Google's own docs say notifications drop.
And a generic CalDAV client covers Zoho with far less setup than a Zoho OAuth
self-client, while also covering Fastmail, iCloud, and Nextcloud later; Google
is better served by its REST API because that is where `syncToken` lives.

### Recurrence engine: measured, not assumed

Both candidates were run against a real corpus before choosing. The corpus is
abnormalarm's `NextOccurrenceTest` — 18 cases covering short-month skipping,
Feb-29 leap years, nth-weekday, last-weekday, end conditions and a DST gap,
described in that repo as its most-tested code — translated into RFC 5545 rules.
The harness lives in the session scratchpad; it should be reproduced inside
`crates/schedule` as a permanent test when that crate is created.

Result: **`rrule` 0.14.0 and `calcard` 0.3.13 both pass 13/13** of the
translatable cases, with identical output on every one.

Three things separated them:

- **DST gap.** For a daily 02:30 alarm in `Europe/Berlin` crossing the
  2026-03-29 spring-forward, `rrule` shifts the nonexistent time forward to
  03:30, matching `java.time` and therefore matching what abnormalarm does
  today. `calcard` returns 02:30 unchanged as a floating datetime with no
  offset, leaving the caller to resolve a wall time that does not exist. For an
  alarm, the first behaviour is correct and the second is a bug waiting to fire
  at the wrong hour.
- **wasm.** `rrule` compiles to `wasm32-unknown-unknown` with no configuration.
  `calcard` does not: `ahash` pulls `getrandom` 0.3 without its `wasm_js`
  feature, so building it for the browser needs a global
  `RUSTFLAGS=--cfg getrandom_backend="wasm_js"` *and* a direct `getrandom`
  dependency added purely to force feature unification. Both were verified to
  work, but they contaminate the whole workspace build and would have to be
  threaded through the flake's wasm wrappers.
- **Maintenance.** `rrule` 0.14.0 was released 2025-04-20 and has been quiet for
  seventeen months. `calcard` 0.3.13 shipped 2026-08-25 and is actively
  developed because Stalwart's CalDAV server depends on it.

**Decision: `rrule`, behind a local trait so the engine stays swappable.** The
maintenance gap is the one real argument against it, and the trait boundary plus
the test corpus is the mitigation.

`calcard` remains interesting for the *ingest* side — it parses full iCalendar,
which the private-iCal-URL fallback for a locked-down Workspace calendar would
need. That is a separate decision, on non-wasm targets, made when the connector
is built.

**A correction worth recording, because it nearly drove the wrong decision.**
Research initially reported `rrule` as carrying open bugs on exactly the cadences
this feature needs — `FREQ=YEARLY;BYMONTHDAY` expanding monthly (#139) and
`NWeekday::Nth` losing its ordinal on serialization (#148). Both were tested
directly. #148 does not reproduce: ordinals serialize as `2TU` and survive a
round-trip, which matters because that path is how a rule would be pushed to
Google. #139 reproduces, but is not a bug — `calcard` produces byte-identical
output, and per RFC 5545 `BYMONTHDAY` *expands* the yearly period across every
month when `BYMONTH` is absent. An open issue tracker entry is a lead, not a
defect.

## Open Decisions

All resolved. See the [Decision Log](#decision-log) for D1-D11.

One item still needs an answer from the repo owner, and it gates the shape of
the Google connector rather than the design:

- **Can a personal OAuth app be authorized against the work Google Workspace
  account?** Workspace admins can block unverified third-party apps, and a
  personal side project is exactly what that control exists to stop. If it is
  blocked, work becomes ingest-only over its private iCal URL, which costs
  nothing because D9 already makes work ingest-only. Quickest check is trying to
  authorize any third-party calendar app with that account.

## Decision Log

### D1: Clipper is the hub; external calendars are not mirrors

Settled 2026-09-07.

Clipper's schedule is the system of record. External calendars are peripheral
in both directions, and the two directions are deliberately asymmetric:

- **Inbound is wholesale.** Meeting invites from other people land in Google
  Calendar or Zoho, and those have to show up in the planning system. Ingest
  pulls everything from a connected calendar.
- **Outbound is per-event opt-in.** Nothing leaves Clipper by default. A single
  event can be marked visible in a given external channel, and only then is it
  published.

There is no full bidirectional mirror, and this is a product requirement rather
than an implementation shortcut. It removes most of the conflict surface that
makes calendar sync miserable: the only records that can diverge are the ones
explicitly published, and Clipper holds the authoritative copy of those.

This also generalizes past calendars. "Visible in another channel" is a property
of an event, and Google and Zoho are the first two channels, not the model.

### D2: A block carries an optional reference, and a planned-versus-actual record

Settled 2026-09-07.

**References are optional, not structural.** A block may point at something —
a task, a project, an existing Clipper collab doc or file — and it may equally
be bare labelled time. The reference is a nullable field on the block, so no
task subsystem is required for the schedule to be useful, and adding one later
does not reshape the block.

**Planned and actual are both tracked.** A block records what was intended; a
separate record captures what actually happened, and the difference between them
is a thing the user wants to see. This implies a start/stop timer in the UI, and
it means concrete per-instance records are a first-class, frequently-written
part of the model rather than a rare exception. A planned block can be a
recurring *rule*; an actual is always a concrete one-off against a specific
instance. Those cannot be the same record.

The write pattern that follows is the main input to decision 3: many small,
frequently-mutated per-instance records sitting alongside a much smaller set of
rarely-changed recurrence definitions.

Prior art backs the separation. Clockify keeps a repeatable `Scheduled
Assignment` distinct from its `Time Entry` rows, and Toggl 2.0 keeps calendar
time blocks distinct from time entries, auto-creating an entry when a block is
marked done. The products that instead store an `actual` field on the planned
item — Sunsama, Motion — are the ones that handle recurrence worst, because the
first occurrence either overwrites the template or forces a clone per
occurrence. Google Calendar Goals took the mutation approach, kept no durable
record of what actually happened, and was withdrawn in November 2022. See
[`docs/time-management-prior-art.md`](time-management-prior-art.md) §2.

### D3: The object layer gets generalized before the schedule is built on it

Settled 2026-09-07.

The schedule is module one of several — habits, tasks and others follow. Adding
an object kind to Clipper today means hand-threading it through api-types,
app-types, the client engine and local store, daemon IPC, the server routes, the
wasm / UniFFI / Tauri adapters, and both frontends. Collab docs paid that toll
once; each later module would pay it again.

So the mechanical part of that plumbing gets collapsed first — a kind registry
or a generic structured-record kind with a typed schema above it — and the
schedule is built as the first consumer of the generalized layer. The hard
constraint is that `crates/app-types` derives UniFFI records for mobile and
UniFFI handles generics poorly, so the generalization cannot simply be "make
everything generic".

The touchpoint cost is now measured, and a design for the generalized layer is
written up in [`docs/object-kind-plumbing.md`](object-kind-plumbing.md). The
headline: adding a kind touches roughly 30 files and 225 hand-written sites, and
about 60% of that is pure boilerplate. The worst of it is that a single
operation gets declared eight times across six forwarding layers — daemon IPC
variant, params struct, handler arm, Tauri command, wasm export, UniFFI export,
shared TypeScript method, mobile-bridge mapping.

Three findings from that survey change what has to happen first:

- `crates/server/src/routes/objects.rs:1539` hardcodes
  `object_kind: Set("file".into())` on the delete event. It is harmless today
  only because `:1438` rejects deletes for every kind except `File`. It becomes
  a live bug the moment a second deletable kind exists, so it is a prerequisite
  fix rather than a cleanup.
- The `AppState` reshape cannot be staged. The daemon `StateChanged` event, the
  wasm `getState`, the Tauri state commands, the UniFFI record, the
  `packages/shared` type, and both `App.tsx` files all consume the same shape,
  so the cut lands in one commit and bumps `IPC_AUTH_VERSION`.
- Replacing the per-kind `match` arms with a runtime registry trades compile
  errors for runtime gaps. Those exhaustive matches are currently what forces a
  new kind to be handled everywhere. A registry-completeness test over
  `ObjectKind::iter()` has to replace them.

### D4: End-to-end encrypted, with clients as the sync workers

Settled 2026-09-07.

Schedule data is encrypted like clipboard and files. The server stores
ciphertext and never reads an event. The objection this normally raises — that
the server then cannot run the Google or Zoho connectors — is answered by moving
that work to the clients rather than to a privileged daemon:

- **Recurrence expansion is client-side.** It is pure computation over a small
  rule set with no database access, so there is no reason for the server to do
  it, and under encryption it could not anyway. Each client also picks its own
  expansion horizon: a phone might expand the 48 hours abnormalarm cares about
  while a desktop expands a month for the grid.
- **External API calls are client-side.** Any logged-in client can be a sync
  worker for a given source. `clipper-daemon` is then one possible worker rather
  than a required component.
- **Credentials are encrypted objects that sync.** OAuth refresh tokens and
  app-specific passwords live in the same encrypted object store as everything
  else, so authorizing Google once on any device lets any other device take over
  sync duty, and the server never sees a token.
- **Divergence uses the existing pattern.** Clipper already refuses to show an
  object as present until the server has returned its committed sequence
  (`created_seq`, see `docs/ws-sync-flow.md`). Client-performed work confirms the
  same way, so this is not a new problem.

Two guards this design needs, neither of which changes the shape above:

**Source ownership needs arbitration, not just a per-client toggle.** If two
clients both sync Google, ingest doubles and writes race; if none do, ingest
stops silently. Manual per-client enablement alone leaves both failure modes
open. The fix that preserves encryption is a server-held lease: exactly one
device holds a lease on an opaque source id until it expires, and another device
takes over when it lapses. A lease is metadata, so the server can arbitrate it
while still learning nothing about the source beyond its existence and which
device is currently syncing it.

**Pushing to an external provider needs an idempotency key.** If a client
publishes an event to Google and dies before recording the returned external id,
the next pass publishes it again. Google's `events.insert` accepts a
client-supplied event id, so deriving that id deterministically from the Clipper
object id makes republication idempotent. The same care applies to sync tokens:
a `nextSyncToken` must be persisted only after the events it covers are
committed locally, or a crash silently drops them.

One security note worth carrying forward: a synced credential exists, encrypted
at rest, on every device. That is strictly more exposure than pinning it to one
machine's OS keychain, and it is the price of letting any device take over sync.
Individual sources should be able to opt out of syncing their credential and
stay bound to one device.

### D5: Intervals are the stored form; the grid is a view

Settled 2026-09-07. Engineering call rather than a product one — override if you
disagree.

A block is stored as a start instant plus a duration, with an IANA timezone id.
The 5- or 10-minute grid is a UI snap and a validation rule, not a storage
format.

The background research recommends the opposite: a fixed per-day bitmap at slot
resolution as the source of truth, with intervals only at the interoperability
boundary. That advice comes from availability-search systems — Cronofy, when2meet,
booking engines — where the question is "when are these fifteen people
simultaneously free" and a bitwise AND answers it in one instruction. That is a
genuinely different problem from a single user's planner.

Three things make the grid the wrong internal form here:

- A bitmap slot holds one bit, but a block carries a title, an optional
  reference, an alarm policy, and per-channel publish flags. The entities have to
  exist anyway, so the bitmap becomes a second representation to keep in sync
  rather than a replacement for the first.
- Ingested events are not grid-aligned. A meeting that runs 09:07–09:23 is
  ordinary, and D1 requires ingesting those faithfully. The research concedes
  this in its own caveats and ends up recommending intervals for external events,
  which means both forms exist regardless.
- The per-slot last-writer-wins merge that motivates the bitmap does not apply.
  Under D4 the sync unit is an encrypted object, not a bit, so the merge
  advantage never materializes.

Overlap detection and free-slot search still want the grid, and they get it by
rasterizing on demand over a bounded window. That is cheap for a day or a week,
and it keeps the grid where it belongs — in the query and rendering path, not in
the store.

### D6: Server-visible revisions over encrypted content, with history retained

Settled 2026-09-07.

An object gets a stable id and a sequence of revisions. Each revision is its own
immutable, individually-signed envelope carrying its own encrypted metadata and
payloads; nothing is ever overwritten in place. The revision number is a
**server-visible integer**, deliberately hoisted out of the ciphertext.

That hoisting is the whole point. A version buried inside the encrypted payload
is invisible to the server, so "give me the current state of this object"
degrades into "give me everything and let the client work it out". A plain
counter in a column lets the server index and serve the latest revision directly,
while telling it nothing beyond the fact that an object changed and how often —
which it already infers from event-log rows and timestamps.

This is a *smaller* change to the crypto model than mutable objects would have
been. The question is no longer "is it safe for a sealed object to be
overwritten" but "which of these sealed objects is newest", so the existing
immutability argument in `docs/object-envelopes.md` survives. The AAD gains
`revision` alongside the identity fields it already binds, and because postcard
is positional this is a format break: `object_version = 2`, cut over rather than
migrated, which `CLAUDE.md` permits.

History is retained rather than discarded, because being able to see how a plan
changed is wanted in its own right.

Consequences to build:

- **Retention is per kind.** Schedule items keep every revision; they are a few
  hundred bytes. Files must not, or renaming a large blob duplicates it. This
  belongs in the same per-kind policy table as TTL and delete eligibility.
- **Concurrent writes need optimistic concurrency.** Two devices both submitting
  revision `N+1` must not silently resolve to whichever landed second. The server
  accepts only `current + 1` and rejects the loser, which rebases and retries.
  With one user this will almost never fire, but the alternative is silent data
  loss.
- **Rollback protection still needs a client check.** A server can serve revision
  3 while 7 exists. Clients track the highest revision seen per object and refuse
  to go backwards. Retained history makes this stronger than overwrite would
  have — a client can enumerate revisions and notice a gap — but a freshly
  installed device has no history to compare against and must trust what it is
  given. That limit is inherent, requires an actively malicious server, and gets
  written into the envelope doc rather than glossed.
- **Deleting an object deletes its whole revision chain,** and the storage quota
  accounting in `crates/server/src/storage_quota.rs` has to count revisions, not
  objects.

### D7: One object per recurring series, not per occurrence

Settled 2026-09-07.

A repeating event is stored once, as the rule — the series definition, matching
RFC 5545's master `VEVENT` plus `RRULE`. Occurrences are computed by clients, not
stored. Only two things become objects of their own:

- **An override**, when a single occurrence is moved, retimed, or cancelled. This
  is RFC 5545's `RECURRENCE-ID` mechanism, and it exists only for occurrences
  that actually deviate.
- **An actual**, per D2 — always a concrete one-off, never a rule.

Clients fetch the full set of schedule objects and select the window they need
locally. This falls out of D4 rather than being a separate choice: the server
cannot filter by date because it cannot read dates. It is cheap because the rule
representation is compact — a daily alarm for a year is one object, not 365 —
and because the server-visible revision from D6 lets clients fetch incrementally
instead of re-pulling the world on every reconnect.

The volume that does grow without bound is actuals, at roughly one small record
per completed block. At a few thousand a year that is fine for a long time, but
it means an archival story is eventually needed, and it should not be designed in
from the start.

### D8: abnormalarm gets absorbed into Clipper rather than bridged to

Settled 2026-09-07.

The goal is one app, not two apps talking. Alarms then stop being a parallel
concept with their own list and become a property of a schedule block, and they
sync across devices like everything else — which a standalone Android app could
never do.

**React Native is not the obstacle it looks like.** RN does not implement alarms,
but an RN app hosts arbitrary native Kotlin, and `mobile/` is an Expo 57
*prebuild* with `android/` checked into the repo, so there is no managed sandbox
to escape. abnormalarm's Kotlin moves in close to as-is: the receivers, the
`AlarmManager.setAlarmClock` scheduler, the `mediaPlayback` foreground service,
and the ring activity.

**The ring screen stays native.** It launches from a `BroadcastReceiver` while
the device may be dozing and the app process may have been killed. Booting a JS
runtime first is the wrong trade for the one screen that must never fail to
appear. The management UI — setting an alarm on a block, choosing how it rings —
moves to React Native alongside the rest of the schedule UI.

**Direct boot is already solved, and it is what makes encryption compatible with
alarms.** `scheduling/ScheduleMirror.kt` keeps a device-protected
`SharedPreferences` mirror of just enough alarm and timer state to re-register
and ring after a reboot before the user has unlocked; writes use `commit = true`;
`RescheduleReceiver` and `AlarmReceiver` branch on `DirectBoot.isUserUnlocked()`
and fall back to the mirror. Room stays the source of truth once credential-
protected storage is available.

That pattern transfers directly. After unlock, the Rust core decrypts the
schedule, expands the next horizon, and refreshes the mirror; the mirror is what
actually fires at 06:00 after a 03:00 reboot, because nothing can decrypt before
first unlock. The concession is explicit and bounded: fire times and labels for
the next horizon sit in device-protected storage in the clear. That is
unavoidable for any alarm that rings before unlock, and abnormalarm already
accepts it.

Consequences:

- **Reliability is the real cost, and it is not theoretical.** abnormalarm is
  tuned against HyperOS background-killing, and its own notes say acceptance has
  to happen on the owner's POCO F7 Ultra because stock Android will not reproduce
  it. Folding the alarm engine into a larger process carrying a JS runtime and a
  Rust core makes it fatter and more killable. abnormalarm must keep running
  untouched until the absorbed path is proven on that device.
- **`domain/schedule/NextOccurrence.kt` goes away**, replaced by the shared Rust
  recurrence engine from D7. Its 18 unit tests — short months, Feb-29 leap years,
  last-weekday, DST, end conditions, described in abnormalarm's own notes as the
  most-tested code in that repo — get ported into the Rust engine's suite. They
  are the ready-made corpus for deciding between `rrule` and `calcard`, since
  they cover exactly the cadences where `rrule` has open bugs.
- **abnormalarm's countdown timers come along** and share the ring infrastructure
  with the stopwatch-style tracking timer D2 calls for. They are different
  features that happen to need the same service.
- **Logging in stops being a problem.** There is no second app to type a
  passphrase and server URL into.

Four platform constraints found while researching this, detailed with sources in
[`docs/rn-alarm-absorption.md`](rn-alarm-absorption.md):

- **`expo prebuild` would destroy the native tree.** SDK 57 changed it to clear
  and regenerate `android/` and `ios/` by default; `--no-clean` is now opt-in.
  `mobile/android/` is checked in and would carry the entire alarm stack, so the
  rule is to treat `mobile/` as a bare workflow and never run prebuild. Verified
  against Expo's SDK 57 changelog.
- **Android 15 attributes foreground-service starts to `BOOT_COMPLETED`,** and
  `mediaPlayback` is on the forbidden list for that attribution. An alarm firing
  inside roughly the first 45 seconds after a reboot can therefore fail to start
  the ring service, while the same alarm a minute later succeeds. Rescheduling
  from `LOCKED_BOOT_COMPLETED` may dodge it — that is an inference, not
  documented, and needs a device test.
- **`setAlarmClock` is gated on the exact-alarm permission** on API 31+, and
  revoking `SCHEDULE_EXACT_ALARM` deletes every alarm already scheduled with it.
  `USE_EXACT_ALARM` is auto-granted and not user-revocable, which removes that
  failure class. Play restricts it to dedicated alarm, timer, and calendar apps
  and reviews requests — a risk only if Clipper is ever published there, which
  is not the current plan.
- **The JS bundle can technically load before first unlock** in a release build,
  because it sits in APK assets on device-encrypted storage. It must not be
  relied on regardless: an OTA-delivered bundle lands in credential-protected
  storage and becomes unreadable pre-unlock. The pre-unlock path stays entirely
  native, reading the device-protected mirror.

### D9: Three calendar sources, and direction differs per source

Settled 2026-09-07.

| Source | What lands there | Direction |
| --- | --- | --- |
| Google Workspace (work) | work meetings and invites | ingest only |
| Google personal (Gmail) | Luma, Meetup, Ticketnation and other social invites | ingest + selective publish |
| Zoho (custom domain) | professional-but-not-main-work | ingest + selective publish |

Two Google accounts, not one. Multi-account support is therefore required from
the first commit rather than added later — abnormalarm already learned this and
keys its calendars as `"accountEmail|calendarId"` precisely because two Google
accounts collide on calendar ids. Source identity in Clipper is
`(provider, account, calendar_id)`.

Work is ingest-only on purpose: a personal side project should not write into an
employer's calendar. That also defuses the main risk here, which is that
Workspace admins can block unverified third-party OAuth apps. If that block is
in place, the work calendar can still be ingested through its private iCal URL —
read-only, no OAuth, no admin approval — and nothing is lost, because publishing
there was never wanted.

**Ingest must not inherit abnormalarm's qualification filter.** That app admits
an event only if the user organizes it, has accepted it, or it has no attendees,
and it drops all-day events entirely. Those rules suit an alarm app. A planner
wants the opposite default: every invite visible, with RSVP status shown as a
property rather than used as a filter, so a meetup that has not been replied to
still occupies its evening on the grid. All-day events must appear too.

### D10 (PROPOSAL — not yet signed off): ingest and publish semantics

Written 2026-09-07 as a concrete proposal to react to, per D1's asymmetry.

**An ingested event's core fields are owned upstream.** Its start, end, title and
description come from the provider and are not editable in Clipper. Offering an
edit would either lose it on the next ingest or require pushing back, and pushing
back is exactly the mirror behaviour D1 rules out. The UI should show these as
read-only with their origin visible.

**Ingested events can still be decorated.** An alarm, a link to a project or
collab doc, private notes, a category, and completion state are Clipper-native
fields living on the same object. None of them are ever pushed upstream, so
there is nothing to conflict.

**Upstream deletion tombstones rather than erases.** A cancelled or deleted
remote event marks the Clipper object cancelled. Because actuals are separate
objects (D2), time already logged against a meeting survives the meeting being
deleted, which is the correct outcome and falls out of the model rather than
needing special handling.

**Published copies are authoritative from Clipper.** A published block pushes its
current state to the remote on each sync. The remote event id is derived
deterministically from the Clipper object id, which makes republication
idempotent (D4). An edit made to the copy inside Google will be overwritten on
the next push, and the UI should say so where publishing is enabled rather than
letting it surprise anyone. Un-publishing deletes the remote copy.

**Fields with no remote representation simply do not travel.** A project link,
an alarm policy and an actual-time log have no Google or Zoho equivalent. Because
nothing round-trips — Clipper always holds the authoritative copy — none of it is
lost. This is the payoff of D1's asymmetry.

**Re-publishing an ingested event is allowed but secondary.** Copying a work
meeting onto a personal Zoho calendar is a legitimate thing to want. The
published copy derives from the ingested state rather than from a Clipper-authored
block, and Clipper still owns the copy it created.

### D11: Build order follows verification cost, not code volume

Settled 2026-09-07.

With code generation cheap, the bottleneck is not writing but confirming. Four
things gate this work, and none are typing: the owner's review bandwidth, on
which the atomic `AppState` reshape and the envelope change depend; wall-clock
verification that cannot be compressed, such as leaving alarms overnight on the
POCO under real background pressure or waiting for a `syncToken` to go stale;
human-in-the-loop external setup like the Google Cloud consent screen; and the
envelope change itself, where fast generation is a liability because a wrong AAD
projection fails silently rather than loudly.

Order that follows from that:

1. **`crates/schedule` recurrence engine.** Pure, no I/O, no crypto. `rrule`
   behind a local trait, with the bake-off corpus reproduced as permanent tests.
   Fully verifiable by running it.
2. **The D6 revision layer**, as its own carefully-reviewed change. Not folded
   into anything else.
3. **Zero-design-risk plumbing**, in parallel: the hardcoded `"file"` fix at
   `routes/objects.rs:1539`, and collapsing the six forwarding layers into
   generic object commands.
4. **The schedule object kind** wired through sync.
5. **UI**, web and desktop first, then mobile.
6. **abnormalarm absorption**, last of the app work, with abnormalarm left
   running untouched until the absorbed path is proven on the POCO.
7. **Connectors**, Google first per D9.

Deferred deliberately: the kind registry and the `AppState` reshape from D3 wait
until the schedule module exists, so the abstraction is designed against two real
consumers instead of one and a guess.

Started early because they are wall-clock rather than effort: the Google Cloud
OAuth setup, and any POCO reliability testing.

## Next Steps

1. Answer the Workspace OAuth question in [Open Decisions](#open-decisions).
2. Get D10 signed off or amended.
3. Create `crates/schedule`: domain types (`ScheduleItem`, `Recurrence`,
   `Occurrence`, override and actual records per D2/D7), a `RecurrenceEngine`
   trait, an `rrule` implementation behind it, and the bake-off corpus as tests.
   The throwaway harness proving `rrule` 13/13 is at
   `/private/tmp/claude-502/-Users-abhik-coding-llms-clipper/89384155-99bd-4eca-8e99-19c71a983950/scratchpad/rrule-bakeoff`
   (`src/main.rs` is the corpus, `src/bin/bugprobe.rs` and `src/bin/yearlycmp.rs`
   are the bug probes). It survives compaction but not a new session, and is
   committed nowhere — port it, do not rewrite it.
4. Then D11 step 2.

Reference docs produced alongside this plan, each with a provenance header
stating what was verified by hand: [`object-kind-plumbing.md`](object-kind-plumbing.md),
[`time-management-prior-art.md`](time-management-prior-art.md) (note its §4.8
verdict is superseded), [`rn-alarm-absorption.md`](rn-alarm-absorption.md).
