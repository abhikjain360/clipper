# Schedule Plan

Design notes for adding a time-management layer to Clipper: schedulable time
blocks, recurring events, alarm dispatch into
[abnormalarm](https://github.com/abhikjain360/abnormalarm), and two-way sync
with Google Calendar and Zoho Calendar.

**Status: scheduler, Android alarm absorption, and D6 revisions implemented on
`schedule-module`; awaiting owner QA and merge review.** This branch includes
the web/desktop grid, editing, actual-time timers, and native iCalendar ingest.
Google/Zoho OAuth connectors, publishing, mobile schedule management,
single-occurrence editing, and undo UI remain deferred. See
[`scheduler-review.md`](scheduler-review.md) for the current review and QA
handoff, and the [Build log](#build-log) for implementation history. The earlier
implementation used a disposable deployment with an explicitly approved data
reset; this review uses isolated local data and makes no deployment.

D1-D11 in the [Decision Log](#decision-log) are agreed. Everything under
[Findings](#findings) was verified against the code as of 2026-09-07 and is
meant to save the next reader from re-deriving it — though the sections on where
a kind is wired are now history rather than instructions, since that wiring
exists. Start at [Next Steps](#next-steps).

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

| Layer         | Files                                                                                                |
| ------------- | ---------------------------------------------------------------------------------------------------- |
| Wire contract | `crates/api-types/src/lib.rs` (`ObjectKind`, request/response types)                                 |
| Display state | `crates/app-types/src/lib.rs` (`AppState` gains a field)                                             |
| Sync engine   | `crates/client/src/engine.rs`, `crates/client/src/local_store.rs`, `crates/client/src/api_client.rs` |
| Server        | `crates/server/src/routes/objects.rs`, route table in `crates/server/src/main.rs`                    |
| Daemon IPC    | `crates/daemon-types/src/protocol.rs` (`DaemonCommand`), `crates/daemon/src/handler.rs`              |
| Adapters      | `crates/web-wasm`, `crates/mobile-uniffi`, `web/src-tauri`                                           |
| Frontend      | `packages/shared`, `web/src`, `mobile/src`                                                           |

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

|                  | Google Calendar                                                                                                                     | Zoho Calendar                                                                |
| ---------------- | ----------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| Protocol         | REST v3                                                                                                                             | REST v1, and CalDAV                                                          |
| Auth             | OAuth2; installed-app loopback flow fits a self-hosted daemon                                                                       | OAuth2 (`ZohoCalendar.event.ALL`) for REST; app-specific password for CalDAV |
| Write scope      | `https://www.googleapis.com/auth/calendar`                                                                                          | `ZohoCalendar.event.ALL`                                                     |
| Incremental sync | `events.list` with a stored `nextSyncToken`                                                                                         | CalDAV `sync-collection` (RFC 6578) if supported; otherwise poll             |
| Push             | `events.watch` → HTTPS webhook. Needs a verified domain, delivers a bare "something changed" ping, and Google documents it as lossy | None documented                                                              |
| Recurrence       | `RRULE` on the master event; instances carry `recurringEventId` + `originalStartTime`; `singleEvents=true` expands server-side      | `rrule` parameter, RFC 5545 shaped                                           |
| CalDAV base      | Deprecated in practice for this use                                                                                                 | `https://calendar.zoho.com/` (regional: `.eu`, `.in`)                        |

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
  `RUSTFLAGS=--cfg getrandom_backend="wasm_js"` _and_ a direct `getrandom`
  dependency added purely to force feature unification. Both were verified to
  work, but they contaminate the whole workspace build and would have to be
  threaded through the flake's wasm wrappers.
- **Maintenance.** `rrule` 0.14.0 was released 2025-04-20 and has been quiet for
  seventeen months. `calcard` 0.3.13 shipped 2026-08-25 and is actively
  developed because Stalwart's CalDAV server depends on it.

**Decision: `rrule`, behind a local trait so the engine stays swappable.** The
maintenance gap is the one real argument against it, and the trait boundary plus
the test corpus is the mitigation.

`calcard` remains interesting for the _ingest_ side — it parses full iCalendar,
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
output, and per RFC 5545 `BYMONTHDAY` _expands_ the yearly period across every
month when `BYMONTH` is absent. An open issue tracker entry is a lead, not a
defect.

## Open Decisions

The initial D1-D11 decisions are resolved; see the [Decision Log](#decision-log).
Later open decisions and unfinished workflows are tracked in the maintained
[scheduler backlog](scheduler-backlog.md), including override resolution after
schedule edits. This does not mean every scheduler product decision is settled.

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
part of the model rather than a rare override. A planned block can be a
recurring _rule_; an actual is always a concrete one-off against a specific
instance. Those cannot be the same record.

Amended 2026-09-08: for an _ingested_ event there is a third layer beneath these
two — the provider's original timing, immutable and retained. Planned may
diverge from it freely. See D10, "An ingested event carries three layers of
time." A Clipper-authored block has no original layer, so planned is the top.

The write pattern that follows is the main input to decision 3: many small,
frequently-mutated per-instance records sitting alongside a much smaller set of
rarely-changed recurrence definitions.

**A running timer writes on start and stop only**, never on tick. D6 retains
every revision, so a timer that persisted progress each minute would turn one
hour of work into sixty retained envelopes. Elapsed time between the two writes
is derived, not stored.

Prior art backs the separation. Clockify keeps a repeatable `Scheduled
Assignment` distinct from its `Time Entry` rows, and Toggl 2.0 keeps calendar
time blocks distinct from time entries, auto-creating an entry when a block is
marked done. The products that instead store an `actual` field on the planned
item — Sunsama, Motion — are the ones that handle recurrence worst, because the
first occurrence either overwrites the template or forces a clone per
occurrence. Google Calendar Goals took the mutation approach, kept no durable
record of what actually happened, and was withdrawn in November 2022. See
[`docs/time-management-prior-art.md`](time-management-prior-art.md) §2.

### D3: Pay the per-kind boilerplate toll; do not refactor the object layer first

Reversed 2026-09-08. This originally said the object layer gets generalized
_before_ the schedule is built on it.

**The governing rule is type safety, and the generalization trades it away.** It
replaces the per-kind `match` arms — which the compiler forces you to update for
every new kind — with a runtime registry it cannot check, and it erases typed
variants like `CreateClipboard { … }` into `Create { kind: String, payload:
String }` at exactly the UniFFI and TypeScript boundaries that were checking
them. Boilerplate the compiler verifies beats an abstraction it cannot. Calling
that refactor "zero-design-risk" was wrong.

It is also the cheaper path under D11: the ~225 sites are lots of typing with no
verification risk, whereas the refactor is little typing that lands in one commit
across daemon, wasm, Tauri, UniFFI, `packages/shared` and both frontends, and
touches clipboard, files and collab — three things that already work.

Pay the toll once for the schedule kind. Revisit when two or three kinds exist
and what they share is visible rather than guessed. Cost accepted: the toll gets
paid again for habits and tasks.

Adding an object kind means hand-threading it through api-types, app-types, the
client engine and local store, daemon IPC, the server routes, the wasm / UniFFI /
Tauri adapters, and both frontends. Collab docs paid that toll once; the schedule
pays it again, deliberately. When the generalization does eventually happen, note
the hard constraint: `crates/app-types` derives UniFFI records for mobile and
UniFFI handles generics poorly, so it cannot simply be "make everything generic".

The touchpoint cost is measured, and a design for the generalized layer is
written up in [`docs/object-kind-plumbing.md`](object-kind-plumbing.md). The
headline: adding a kind touches roughly 30 files and 225 hand-written sites, and
about 60% of that is pure boilerplate. The worst of it is that a single
operation gets declared eight times across six forwarding layers — daemon IPC
variant, params struct, handler arm, Tauri command, wasm export, UniFFI export,
shared TypeScript method, mobile-bridge mapping.

Three findings from that survey still matter. The first is a prerequisite fix;
the other two are the evidence for deferring the refactor:

- `crates/server/src/routes/objects.rs:1539` hardcodes
  `object_kind: Set("file".into())` on the delete event. It is harmless today
  only because `:1438` rejects deletes for every kind except `File`. It becomes
  a live bug the moment a second deletable kind exists, so it is a prerequisite
  fix rather than a cleanup.
- The `AppState` reshape cannot be staged, which is a large part of why it is
  deferred. The daemon `StateChanged` event, the
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

**Encryption boundary confirmed 2026-09-08.** A review argued this decision was
settled by analogy to clipboard rather than argued, and proposed a middle: store
ingested originals server-visible so the server could run connectors and stay
fresh, keeping only owner-authored data encrypted. The owner chose to stay fully
encrypted, on empirical grounds — abnormalarm already syncs client-side only,
that model has worked well in daily use for months, and stale-while-asleep is
acceptable given enough lookahead. The two costs the middle would have avoided
are accepted deliberately.

That makes **the ingest horizon load-bearing rather than an optimisation**: the
whole argument for tolerating staleness is that a week or more of events is
already on the device. Forward horizon is therefore at least 7 days, and wants
~35 to render a month grid. The backward horizon should be short — old meetings
are dead weight against a `localStorage` budget of roughly 5MB.

One guard this design needs, which does not change the shape above:

**Source ownership needs a per-client toggle, not a lease.** An earlier draft
specified a server-held lease so exactly one device syncs a given source. That
was over-built and is dropped. Ingest keyed by remote id is idempotent by nature,
and publish already requires deterministic remote ids, so two clients racing on
the same source costs redundant API calls and one retried revision — not
corruption. A lease also cannot fix the failure that actually matters, which is
no client being awake. A manual per-client enablement toggle is enough, with
`clipper-daemon` on the desktop as the sensible default worker.

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

A block is stored as a start plus a duration. The 5- or 10-minute grid is a UI
snap and a validation rule, not a storage format.

**The start is one of three kinds, and conflating them breaks the alarm path.**
An earlier draft stored a single instant plus one IANA zone id, which is only
correct for the middle row:

| Kind          | Meaning                               | Behaviour when the owner travels             |
| ------------- | ------------------------------------- | -------------------------------------------- |
| **Floating**  | a wall-clock time with no zone        | follows the device — 07:00 stays 07:00       |
| **Zoned**     | an instant pinned to an IANA zone     | stays put — a Berlin meeting is still Berlin |
| **Date-only** | an all-day event, a date with no time | no instant at all                            |

This is RFC 5545's own distinction: a floating `DTSTART` carries neither `TZID`
nor a `Z` suffix. A daily 07:00 alarm must float, and abnormalarm already behaves
that way by computing occurrences in device-local time. An ingested meeting must
stay zoned, or it silently moves when you cross a border. All-day events, which
D9 commits to ingesting, are neither. Storing one kind and inferring the rest is
not possible, so the kind is explicit on the block.

The bake-off's DST result reads correctly in this light: `rrule` shifting a
nonexistent 02:30 forward to 03:30 is floating-local behaviour, matching
`java.time` and therefore matching the alarms already in daily use.

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
is invisible to the server, so "give me the current state of object X" degrades
into "give me _every revision_ of X and let the client work out which is
newest". A plain counter in a column lets the server index and serve the latest
revision directly, while telling it nothing beyond the fact that an object
changed and how often — which it already infers from event-log rows and
timestamps.

This is a different axis from D7 and the two compose rather than cancel, which is
easy to misread. D7 says the server cannot filter by **date**, so clients fetch
every schedule object. D6 says the server cannot filter by **version**, so
without the counter it must ship every revision of each of those objects. Their
product is the cold-sync cost. Because history is retained deliberately, the
revision factor grows without bound: an object edited two hundred times would
ship two hundred envelopes on every reconnect. D7 fetching the full object set is
precisely what makes D6 load-bearing.

The alternative considered and rejected: supersede chains inside the ciphertext,
the way clipboard already replaces rather than mutates, with forks detected
client-side. It needs no format break, but it scales cold sync with edit count
for exactly the reason above, and it gives up server-arbitrated optimistic
concurrency and clean chain deletion — both listed as consequences below.

**The AAD change no longer fails silently, as of 2026-09-08.** D11 named this
the one place where fast generation is a liability, because omitting a field
from the AAD projection is not a compile error, not a decryption error, and not
a signature error — the only symptom is a ciphertext replayable across the
missing field. That is now enforced in `crates/core/src/crypto.rs`:
`object_aad_v1` destructures the envelope body exhaustively, so adding
`revision` to it cannot compile until binding it is decided, and `mod
object_aad` tables every field with which side of the line it falls on, failing
by name if a bound field stops being bound or an unbound one starts. Both
directions were verified by deliberately breaking them.

What that does _not_ cover: a test asserts the projection you wrote, not the one
you should have written. Whether the right set of fields is bound at all is
still a judgment call, and still wants a human read.

This is a _smaller_ change to the crypto model than mutable objects would have
been. The question is no longer "is it safe for a sealed object to be
overwritten" but "which of these sealed objects is newest", so the existing
immutability argument in `docs/object-envelopes.md` survives. The AAD gains
`revision` alongside the identity fields it already binds, and because postcard
is positional this is a format break: `object_version = 2`.

**Deployment note, 2026-09-08.** `CLAUDE.md` says the project is not deployed
anywhere. That is stale — there is a live instance at `api.clipper.abhikja.in`,
a cloudflared tunnel to the netcup box. Asked directly, the owner confirmed the
data there is disposable: **no migration is required, and recreating the
database is the sanctioned path** for a format break. So the cutover is free in
substance even though the premise was wrong. Any agent planning one should still
say out loud that it destroys the netcup data, rather than inferring permission
from `CLAUDE.md`.

**Retention, settled 2026-09-08.** History is retained. The owner's reasoning
is not that a history UI is wanted now — none is planned — but that undo is a
plausible enough future feature that the format should already accommodate it,
so adding it later does not mean breaking the format a second time. Latest-only
revisions would have been cheaper and would still have given optimistic
concurrency and clean chain deletion, but they foreclose undo at the format
level, which is exactly the outcome being avoided.

Two consequences follow that the rest of D6 did not previously state.

**Deletion becomes a tombstone.** "Deleting an object deletes its whole
revision chain" is incompatible with undoing a delete, which is the single most
wanted undo there is. So `delete` appends a terminal revision carrying no
payload, leaving the chain intact and the object resurrectable by appending a
further revision that restores an earlier one. Actually reclaiming the space
becomes a separate, explicit **purge**, which is irreversible and is what the
storage quota and any TTL sweep act on. This costs a deleted object its storage
until purged, which for schedule records is negligible and for files is why the
per-kind retention table exists.

> Follow-up clarification: this describes the storage operation, not a complete
> undo feature. Reading retained content requires the revision-specific read
> path, and undo still needs UI and conflict handling. Actuals now pin historical
> schedule/override definitions; retained history alone did not provide that
> association. See [scheduler-review.md](scheduler-review.md).

**Undo needs no machinery beyond retention.** Restoring revision `N-1` is
writing its content as revision `N+1`. There is no separate undo log, no
reverse-diff, and nothing to design now — which is the whole justification for
paying the retention cost today.

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
  Follow-up clarification: current anchors reject rollback of accepted heads,
  but no revision-enumeration or transparency protocol proves history complete.
  Explicit historical reads check an exact pinned body without replacing the
  current head or changing its anchor. Browser retention is best-effort; see
  [object-envelopes.md](object-envelopes.md) for platform-specific limits.
- **Delete is a tombstone; purge is the destructive one.** See _Retention_
  above. The storage quota accounting in `crates/server/src/storage_quota.rs`
  has to count revisions rather than objects either way, and has to keep
  counting a tombstoned chain until it is purged.

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

**Correction, 2026-09-08 — the stated mechanism does not exist, and the real one
is cheaper.** This decision assumed `mobile/android/` is a checked-in
bare-workflow tree that Kotlin can be dropped into. It is gitignored
(`.gitignore:34`) and regenerated by `expo run:android`.

The replacement turned out to need no config plugin at all. An **Expo local
module** under `mobile/modules/` is autolinked by SDK 57, is tracked in git, and
survives prebuild — and because it is an Android library, its own
`AndroidManifest.xml` merges into the app's. Permissions, `directBootAware`
receivers, the foreground service and the ring activity are all declared there.
Verified by building and installing on an Android 16 emulator.

One thing the build surfaced: the module must match the app's `minSdk` (24)
rather than raise it, so `VibratorManager` (31), `setShowWhenLocked` (27) and the
typed `startForeground` (29) are guarded at their call sites.

**Kotlin does not compute recurrence.** abnormalarm recomputes the next
occurrence on every fire, which was right when Kotlin was the only place the
rule existed. Here it would mean two recurrence implementations obliged to agree
forever, and only one of them has the bake-off corpus behind it. So Rust expands
a horizon and hands down a list of concrete instants; Kotlin registers each as a
one-shot `setAlarmClock` and recomputes nothing.

**The Direct Boot mirror is what makes exact alarms compatible with E2EE.** The
schedule is ciphertext the app can only read after unlock and login, but an
alarm has to survive a 3am reboot and ring at 7am with neither. Mirroring just
the upcoming fire times — instants and labels, no schedule, no keys — into
device-protected storage the system unlocks at boot is the whole trick, and it
is how abnormalarm has always worked. The label travels with the alarm for the
same reason: before unlock there is nothing to look it up in.

**Reaffirmed 2026-09-08 — bridging rejected.** A review argued for bridging to
abnormalarm first and absorbing later. The owner rejected it: a bridge is more
total work than going straight to absorption, and the risk it hedges against is
small because alarms are cheap to re-enter by hand if anything is lost. Go
straight to absorption.

This also changes what the D11 step 0 spike is _for_. It no longer gates a choice
between two designs, because there is only one. It gates nothing in the schedule
module at all — it tells the owner whether the absorbed alarm path is reliable on
HyperOS, and if it is not, the fallback is the status quo: keep running
abnormalarm, unintegrated, while the problem is fixed. So the spike should still
start early because it is pure wall-clock, but it is no longer on the critical
path and nothing waits on it.

Settled 2026-09-07.

The goal is one app, not two apps talking. Alarms then stop being a parallel
concept with their own list and become a property of a schedule block, and they
sync across devices like everything else — which a standalone Android app could
never do.

**React Native is not the obstacle it looks like.** RN does not implement alarms,
but an RN app hosts arbitrary native Kotlin, and `mobile/` is an Expo 57
_prebuild_ with `android/` checked into the repo, so there is no managed sandbox
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

### D9: Three calendar sources; the source sets capability, the event sets direction

Settled 2026-09-07. Amended 2026-09-08: direction is per event, not per source.

| Source                  | What lands there                                    | Capability       |
| ----------------------- | --------------------------------------------------- | ---------------- |
| Google Workspace (work) | work meetings and invites                           | ingest only      |
| Google personal (Gmail) | Luma, Meetup, Ticketnation and other social invites | ingest + publish |
| Zoho (custom domain)    | professional-but-not-main-work                      | ingest + publish |

The capability column is an envelope, not a setting. It says what a source is
_allowed_ to do. Which way any individual event actually travels is decided per
event, within that envelope — see D10, where the binding is the unit.

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
there was never wanted. Test both routes together: Workspace admins can disable
the secret iCal address as well, so a blocked OAuth app does not guarantee the
fallback is available.

**Ingest must not inherit abnormalarm's qualification filter.** That app admits
an event only if the user organizes it, has accepted it, or it has no attendees,
and it drops all-day events entirely. Those rules suit an alarm app. A planner
wants the opposite default: every invite visible, with RSVP status shown as a
property rather than used as a filter, so a meetup that has not been replied to
still occupies its evening on the grid. All-day events must appear too.

### D10: ingest and publish semantics

Written 2026-09-07 as a proposal, per D1's asymmetry. Rewritten 2026-09-08
around per-event bindings. Core settled 2026-09-08; three sub-questions listed
at the end remain open and none of them affect the object's shape.

#### The binding is the unit

A **binding** is an `(object, source)` pair carrying its own direction, its own
remote id, and its own sync state. Direction is a property of the binding, never
of the source and never of the object alone. One object may hold several
bindings at once: a work meeting arrives with an `ingest` binding to work Google,
and gains a `publish` binding to Zoho if it is republished there. The two
coexist and behave differently.

Consequences that fall out of this and have to be built in from the start:

- The remote id derives from `(object_id, source_id)`, not from `object_id`
  alone. Deriving it from the object alone makes two published copies collide.
- Source-level settings are **defaults that pre-fill a new binding**, never live
  rules evaluated at sync time. Otherwise changing "auto-publish this category"
  later silently republishes hundreds of old events.
- The D4 sync lease is held per binding, so responsibility for pushing to Zoho
  can sit on a different client than responsibility for pulling work Google.
- Un-publishing removes one binding and deletes only that remote copy. Other
  bindings on the same object are untouched.

#### An ingested event carries three layers of time

An earlier draft of this decision said ingested events are read-only. That was
too blunt: it conflated the invite, which belongs to whoever sent it, with the
owner's plan for the invite, which belongs to the owner. They separate cleanly,
and the split lines up with D2's planned-versus-actual model already decided.

| Layer        | Record            | Sole writer | Editable by the owner | Travels upstream       |
| ------------ | ----------------- | ----------- | --------------------- | ---------------------- |
| **Original** | ingested event    | sync worker | no                    | n/a — it _is_ upstream |
| **Planned**  | plan + decoration | the owner   | yes                   | never                  |
| **Actual**   | actual (D2)       | the timer   | yes                   | never                  |

The original is what the invite says, retained permanently as a fact about the
meeting. The planned layer defaults to it and may diverge freely: moving a 09:30
standup to 08:00 moves the plan, not the meeting. Alarms hang off the planned
layer, which makes alarm override a consequence of this model rather than a
separate feature. Clipper is a planner, not an enforcer — it must be able to
disagree with an invite without arguing with the sender about it.

The UI shows the original alongside the plan whenever they differ, so the
divergence is visible rather than a source of confusion about which time is real.

**Each layer is a separate record with exactly one writer.** The original lives
in an ingested-event record written only by the sync worker; the plan and all
decorations live in a record written only by the user; actuals are their own
records per D2. They are linked by id.

This is not tidiness, it is conflict avoidance. Putting the provider's fields and
the owner's decorations on one object means an automated writer polling Google
and a human writer editing offline on a phone both bump the same revision
counter. That is the D6 optimistic-concurrency race, and unlike the two-devices
case it would fire routinely rather than almost never. Splitting by writer
removes it by construction, and it matches the separation D2 already chose for
planned versus actual. The cost is a join on read, plus handling a plan record
whose ingested event has been tombstoned — which D2's independent-actuals rule
already required.

**Divergence is sticky, and upstream changes notify rather than overwrite.** If
the plan has diverged and the provider then moves the original, the plan stays
put and Clipper raises a flag. Snapping the plan back would defeat the purpose;
ignoring the change silently would hide a real reschedule. An undiverged plan
tracks the original automatically, since there is nothing to lose.

Divergence works at both grains, reusing D7's existing structure: a series-level
shift for a meeting always taken 15 minutes late, or an override record on one
occurrence for a one-off move.

Ingest **replaces the original layer wholesale** on each pull rather than merging
field by field. This is simpler and avoids a class of merge bugs, and it is safe
precisely because the planned and actual layers are separate — nothing the owner
wrote can be clobbered by a refresh. It does require that the three layers be
partitioned at the type level rather than by convention.

#### Ingested events can still be decorated

An alarm, a link to a project or collab doc, private notes, a category, and
completion state are Clipper-native fields on the same object. None are ever
pushed upstream, so there is nothing to conflict. Wholesale replacement of the
upstream half leaves all of them intact.

#### Upstream deletion tombstones rather than erases

A cancelled or deleted remote event marks the Clipper object cancelled — a state
distinct from deleted. Because actuals are separate objects (D2), time already
logged against a meeting survives the meeting being deleted, which is the
correct outcome and falls out of the model rather than needing special handling.

#### Published copies are authoritative from Clipper

A published binding pushes the object's current state to its remote on each
sync, idempotently via the derived remote id (D4). An edit made to the copy
inside Google is overwritten on the next push, and the UI should say so where
publishing is enabled rather than letting it surprise anyone.

What gets pushed is the **planned** layer, not the original. For an ingested
event republished elsewhere, the outgoing copy should say when the owner will
actually be busy, which is the only reason to put it there.

#### A recurring series publishes as a series, within a client-set horizon

Settled 2026-09-08. The publish unit is the series, not the occurrence: one
remote recurring event that the provider expands itself, rather than one remote
object per occurrence. A daily habit is then one API object instead of ~250 a
year, and it stays in sync without per-occurrence bookkeeping. Individual
occurrences can be excluded, travelling as `EXDATE`, which is what RFC 5545
designed it for.

The cost, accepted: a Clipper-side deviation only survives the trip if it is
expressible in RRULE terms. Skips travel. Odder overrides may not, and where
they cannot the published copy will disagree with the planner — the UI should
surface that rather than let it pass unnoticed.

**The outgoing binding carries a horizon, set client-side.** The published
recurrence is bounded by an `UNTIL` derived from it, and each sync rolls that
bound forward. A rolling six months of gym on Zoho stays six months rather than
becoming an unbounded series that some other calendar has to reason about. This
is D4's principle — the client decides how far to expand — applied outbound.

#### Fields with no remote representation simply do not travel

A project link, an alarm policy and an actual-time log have no Google or Zoho
equivalent. Because nothing round-trips — Clipper always holds the authoritative
copy — none of it is lost. This is the payoff of D1's asymmetry.

#### Every binding carries its own sync state

Clean, pending, or failed with a reason. D4 puts sync work on the clients, so
"did this actually reach Google" is a per-binding, per-client question and the UI
has to be able to answer it. A failed push must be visible rather than silent.

#### All-day events are not intervals

D5 stores intervals in absolute time. An all-day event is a _date_, and it spans
a different absolute range depending on the observer's zone. It needs its own
representation rather than being coerced into an interval at ingest. D9 already
committed to ingesting all-day events, so this is not optional.

#### Open sub-questions

- **Cross-source duplicates.** A meeting can land in both work Google and Gmail.
  `iCalUID` is stable across calendars for the same meeting so deduplication is
  tractable, but which binding is then primary is undecided.
- **RSVP from Clipper.** Responding to an invite is a write to an ingest-only
  source. It is arguably not mirroring, since it changes attendance rather than
  the event. Carve-out or explicit non-goal — undecided.
- **Upstream recurrence mapping.** Google can return expanded instances
  (`singleEvents=true`) or the master plus RRULE. D7 wants the master, but
  upstream overrides (a moved instance) then have to map onto Clipper's own
  override records. This is the hardest part of ingest and needs its own
  treatment before connectors are built.
  Current ICS implementation embeds provider overrides inside each imported
  event revision as pure `OccurrenceOverrideData` values. Only separately authored
  local overrides use revision-pinned `OccurrenceOverride` objects. Future
  connectors should preserve this distinction, rather than interpreting the
  earlier wording as requiring standalone objects for provider overrides.

### D11: Build order follows verification cost, not code volume

Settled 2026-09-07.

With code generation cheap, the bottleneck is not writing but confirming. Four
things gate this work, and none are typing: the owner's review bandwidth, on
which the atomic `AppState` reshape and the envelope change depend; wall-clock
verification that cannot be compressed, such as leaving alarms overnight on the
POCO under real background pressure or waiting for a `syncToken` to go stale;
human-in-the-loop external setup like the Google Cloud consent screen; and the
envelope change itself, where fast generation is a liability because a wrong AAD
projection fails silently rather than loudly. _Amended 2026-09-08: the silent
part is fixed — see the AAD guard in D6. What still wants review is the choice
of which fields to bind, not the mechanical risk of forgetting one._

Order that follows from that:

Reordered 2026-09-08 around **first-usable**. The previous order front-loaded a
recurrence crate and an envelope break — the two items that produce nothing
visible and consume the most review bandwidth. If confirmation is the bottleneck,
the first deliverable should be something confirmable by using it.

**Milestone 1 — a grid you can look at.** Done when ingested meetings and
hand-made blocks render on a week grid in the web client and the owner has lived
with it for two weeks.

1. **`crates/schedule` recurrence engine.** Pure, no I/O, no crypto. `rrule`
   behind a local trait, with the bake-off corpus reproduced as permanent tests.
   Fully verifiable by running it, and everything else needs it.
2. **The prerequisite fix**: the hardcoded `object_kind: Set("file".into())` at
   `routes/objects.rs:1539`.
3. **A plain schedule object kind**, wired through sync by paying the D3
   boilerplate toll. Immutable objects, create-plus-delete for edits. No
   revisions yet.
4. **Read-only Google ingest**, personal account first — the one whose OAuth
   cannot be blocked by an employer.
5. **The web grid.** Then stop and use it.

**Milestone 2 — what use reveals.** Not planned in detail on purpose; two weeks
of real use should reorder it.

6. **The D6 revision layer**, as its own carefully-reviewed change, designed
   against observed edit patterns rather than predicted ones. Only the edit path
   changes: types, engine, ingest and UI from milestone 1 survive intact, which
   is why building the kind first does not mean building it twice.
7. **Mobile UI**, then **abnormalarm absorption** per D8.
8. **Remaining connectors** and **publish**, per D9 and D10.

Deferred indefinitely: the kind registry and the `AppState` reshape. See D3 —
this is now a reversal, not a scheduling choice.

Runs in parallel, gated on wall-clock rather than effort and blocking nothing:
the D8 HyperOS spike on the POCO, and the Google Cloud OAuth setup — including
whether the private iCal fallback is also blocked (D9).

## Review round 2 (2026-09-08) — accepted, pending restructure

A second adversarial review argued against the decisions rather than the facts.

**Resolved and written into the decisions above:** the D6 cutover premise (the
project _is_ deployed, but the data is disposable and the DB gets recreated);
the D8 mechanism (`mobile/android/` is gitignored, so native code lives in a tracked
Expo local module whose manifest is merged during prebuild); D3 reversed outright; the D4 lease dropped;
D4's encryption boundary confirmed as fully encrypted; D8 confirmed as straight
absorption with bridging rejected.

**Accepted but not yet written into the decisions they affect.** Do not treat
those decisions as final where they conflict with this list.

- **The conflict rule generalizes.** "Conflicts disappear by construction" was
  over-claimed: only the sync worker got its own record. Alarm fired/dismissed
  state, completion toggles and RSVP are further automated or second-device
  writers on the plan record. The rule is _every automated writer gets its own
  record_, and D6 must still state what a human loser of a rejected revision
  sees.
- **Publish is cut from v1.** Most of D10's machinery — bindings, rolling
  horizons, dedupe, remote ids, per-event direction — serves publish, which D11
  schedules last. Designing it now is the exact error D11 warns about for the
  kind registry. Ingest-only first. Two publish details to keep for later: a
  rolling `UNTIL` rewrites every series on every sync to solve something
  providers handle natively, and "overwrite the remote copy" must mean patching
  owned fields or a full update wipes attendee and reminder state.
- **An ingest horizon is required.** The volume risk is not actuals. It is
  ingested one-off meetings across three sources with a revision per attendee
  change, and wholesale ingest means years of history on first connect. The
  browser store is `localStorage`-backed (`crates/client/src/local_store.rs:1894`),
  so the ceiling is ~5MB per origin — a harder wall than an object count.
- **The iCal fallback is not free.** A polled ICS feed is coarser and slower than
  the API and drops RSVP and attendee fields. D9 overstates it as costless.
- **Actuals stop being load-bearing.** The ask was planning. The timer habit is
  the one people abandon first, so the archival and growth arguments must not
  rest on it.
- **D11 reorders around first-usable.** Applied below.

Open, needing the owner rather than an agent:

- ~~**D6's history retention.** Name the UI that reads plan history or drop
  it.~~ **Answered 2026-09-08: retained.** The UI is undo, which is not planned
  yet — and that is the point. The format is being broken once, deliberately,
  so that adding undo later is a feature rather than a second overhaul. See
  D6's _Retention_ section for what that costs and the tombstone rule it
  forces.

Structural gaps to fix in the restructure: no partition between owner-gated work
(the POCO spike, OAuth consent) and agent-doable work; no definition of done, no
first-usable milestone, and no non-goals, which invites scope expansion; and
decisions are not ranked by reversibility — D4 and D6 are one-way doors carrying
the same "Settled" stamp as D5, which invites override in its own text.

## Build log

Branch `schedule-module`, started 2026-09-08. Commits, in order:

1. `schedule: add the recurrence engine crate` — D11 step 1.
2. `server: admit schedule as an object kind` — migration, deletability, and
   the `"file"` delete-event fix.
3. `client: sync schedule records end to end` — create, delete, live
   materialize, reconciliation, windowed expansion.
4. `schedule: expose the schedule through IPC, wasm, Tauri and the shared types`
   — plus a self-describing wire format, pinned by a test.
5. `web: add the schedule grid` — week view, composer, series list.
6. `server: fix a second hardcoded delete kind, and test both`.
7. `schedule: parse iCalendar feeds` — ingest, starting with ICS because it
   needs no OAuth.
8. `schedule: pull calendar feeds and show them on the grid` — **milestone 1
   complete**.
9. `schedule: plan alarms from occurrences` — the Rust half of D8.
10. `mobile: absorb abnormalarm's alarm layer` — the Android half, verified on
    an emulator.
11. `schedule: let a block be edited` — create-then-delete, since objects are
    immutable until D6.
12. `schedule: track time actually spent` — D2's other half: the timer.

**Verified against a live local server**, not just in unit tests: the migration
applied cleanly to the existing dev database (collab docs survived); a cold
second device pulled a series back and decrypted it; and a floating 07:00 series
expanded to 05:00Z for a Berlin observer and 22:00Z the previous day for a Tokyo
one — the same wall-clock morning at different instants, which is the property
D5 exists to protect. The web UI was driven headlessly through CDP (the laptop
display was asleep, so screenshots had to bypass it): login, create, render,
delete.

Five things the build changed about the plan:

- **`RecurrenceId` cannot be an instant.** A floating series has to be
  identified by wall-clock time or an override recorded in one zone silently
  fails to match the same occurrence in another. It is an enum.
- **The hardcoded `"file"` delete kind appeared twice**, not once — the
  `event_log` row and the WebSocket broadcast. Reading the handler found one;
  the regression test found the other on its first run.
- **The serialized form is three contracts at once** — TypeScript, IPC, and the
  ciphertext at rest — so it is tagged and self-describing rather than
  positional, with a test pinning the shape.
- **Ingested rules needed a variant, not a loophole.** `Recurrence::Raw` carries
  a provider's RFC 5545 rule verbatim: validated on the way in, expanded as-is,
  never editable. Modelling ingested rules as typed cadences would have implied
  an editability D10 does not offer. The type now says which rules Clipper
  understands and which it merely passes through.
- **Ingest is native-only, and that is not a limitation to fix.** A browser
  cannot fetch a third-party calendar — no provider sends CORS headers — so the
  wasm build drops the parser entirely. This is D4 working as designed: the
  desktop and mobile clients pull feeds, and every other device sees the results
  through ordinary object sync. Verified: a native client ingested a feed and
  the browser rendered it without ever touching the feed URL.

**Milestone 1 is done, and D8's alarm absorption with it.** An alarm planned in
Rust from a recurrence rule fired on an Android 16 emulator at 23:33:19.571 for
a 23:33:19 instant, showed its ring screen, and stopped on dismiss.
`LOCKED_BOOT_COMPLETED` was observed re-arming from the device-protected mirror.

Three things only the device could have told us:

- **A hand-written `build.gradle` breaks the module at runtime, not at compile
  time.** Setting the Kotlin version, compile SDK or core dependency by hand
  desynchronises the module from the `expo-modules-core` its inline functions
  were compiled against, and it fails on load with "this function has a reified
  type parameter and thus can only be inlined". Use the published-module plugin
  shape (`id 'expo-module-gradle-plugin'`) and set nothing else.
- **Expo's typed-record marshalling does not survive a list parameter**, failing
  the same way. The alarm plan crosses as JSON, which it already was on both
  sides.
- **Android 15's foreground-service restriction is satisfied.** This was flagged
  as unverified. `dumpsys` shows the `mediaPlayback` start admitted with
  `reasonCode: ALARM_MANAGER_ALARM_CLOCK` — granted precisely because it came
  from an exact alarm.

Still untested and still the real risk: whether this survives overnight on the
POCO under HyperOS. An emulator says nothing about a vendor that kills
background processes, and no code substitutes for Autostart and
battery-optimisation exemptions set by hand.

**The first usable scheduler is implemented.** Blocks in 5- or 10-minute grid
steps, recurrence, alarms, and plain ICS ingest are present; editing and the
planned-versus-actual timer landed after milestone 1. The domain accepts richer
recurrence rules than the current composer exposes. The original request's
Google/Zoho connectors and outward publishing remain future work.

What remains: the mobile schedule UI, publish, a UI for single-occurrence
overrides (the engine and record type support them, but nothing creates one yet
— "skip today's gym" has no button), and undo, which D6 now makes possible
without another format change but which has no button either.

The historical `v2` format name below was subsequently normalized to initial
format version 1 before release, without compatibility for development data.

**D6 landed on 2026-09-08**, after the owner settled the retention question and
asked for the parent link. It was deliberately skipped the night before, on the
grounds that a wrong AAD projection fails silently — that premise no longer
holds, because the projection was made to fail loudly first. In order:

1. `core: make the envelope AAD projection fail loudly` — the guard, landed on
   its own so the change it protects could be read against it. Verified by
   breaking it both ways.
2. `envelope: v2 — revisions chained by parent hash` — the format only.
   Nothing wrote a revision above 1 yet.
3. `server: split object identity from its content, and give content a chain` —
   `objects` keeps identity and a head pointer; `object_revisions` holds every
   sealed byte; payloads hang off a revision. Two routes: `POST
/api/objects/{id}/revisions` appends, `DELETE /api/objects/{id}` became
   purge and refuses an object that has not been tombstoned.
4. `client: edit by revision, delete by tombstone, and check the chain` — plus
   the two checks that need local state: no rollback, and continuity against
   the held head.
5. `d6: fix what only a live run could show` — see below.

**What the live run caught that nothing else did.** A shadowed
`OBJECT_ENVELOPE_VERSION` in the client, still holding 1 after the shared
one became 2 — two constants, one name, two crates, and a compiler with no
opinion. A listing that returned payloads from _every_ revision, because the
query still filtered on object id alone. A WS handler that ignored `updated`
for everything but collab, so an edit never reached a second device live (and
one that had been ignoring `deleted` for schedule since the night before).
`delete_file` still calling DELETE, which is purge now. None of these are
type errors and none had a unit test that would have failed.

Verified end to end against a live server and through the web UI: create, edit
keeping the object id, a second device pulling and decrypting revision 2, an
edit arriving over the socket, a stale device's write refused with 409, delete
leaving nothing on a cold device, and a file deleted the same way.

**Not built, and deliberately.** The per-kind retention table. Schedule records
are a few hundred bytes, so keeping every revision of one costs nothing worth
managing; files have no rename path at all today, so nothing creates a file
revision that carries a payload, and the duplicate-blob problem the retention
bullet warns about is latent rather than live. Building the policy now would be
designing against predicted edit patterns instead of observed ones, which is
the error D11 exists to prevent. The quota already counts revisions, so when
the first kind does need pruning the accounting is already right.

Bugs the build found that reading had not, beyond those listed above:

- `ScheduleItemView.id` was the _series_ id while every caller passed it to
  routes wanting the _object_ id, so Delete from the list could never have
  worked. The end-to-end test used the object id directly and hid it — an
  argument for driving the real UI, not only the API.
- The edit form reloaded the stored record on every render, because its effect
  depended on callbacks whose identity changes each time. A sync push mid-edit
  silently reverted unsaved input.
- The mobile alarm push was keyed on formatted summary strings. Toggling an
  alarm changes neither the recurrence text nor the time text, so the registry
  would have gone stale in exactly the case that matters.
- `cancelAll` swept only `0..MAX_REGISTERED` of the request-code space, but
  past entries at the front of a plan push live codes beyond it, stranding
  alarms that could then fire after being cancelled. Of abnormalarm, these are not ported: snooze, per-alarm sound
  and volume ramp, the flashlight, the upcoming-notification lead window,
  skip-next, timers, and the clock widget. abnormalarm stays installed.

One thing milestone 1 deliberately does not have, so its absence is not a gap
to be surprised by: any Google or Zoho connector beyond a plain ICS URL. That
waits on the owner's OAuth consent screen, and D9 already treats ICS as the
supported fallback.

## Next Steps

Start with [scheduler-backlog.md](scheduler-backlog.md) for the maintained list
of outstanding work and decisions. The following is supporting context.

1. Owner QA using [`scheduler-review.md`](scheduler-review.md), followed by a
   walkthrough of the changes and merge review. D6 history retention is settled;
   it is no longer an open product question.
2. POCO/HyperOS reliability acceptance over multiple nights. Emulator results
   cannot establish vendor battery-management behavior.
3. Workspace OAuth/private iCal availability and the remaining connectors.
   Publishing is explicitly outside this PR, even where D10 describes its future
   design.

Other deferred work: mobile schedule-management UI, single-occurrence override
UI, undo UI, source sync opt-in/automation, ingest history retention, and the
remaining abnormalarm features (snooze, sounds/ramp, flashlight, skip-next and
widget). Do not equate the implemented core model with these product surfaces.
The broad kind registry and `AppState` redesign also remain out of scope.

D6's retained revision anchors had a storage cost the JSON-file local store was
never shaped for. The native store is SQLite now, with the anchors in a table of
their own so that discarding cached content cannot reach them. See
[`local-store-plan.md`](local-store-plan.md) for the findings and the decisions;
it was a consequence of D6 rather than a defect in it.

Reference docs produced alongside this plan, each with a provenance header
stating what was verified by hand: [`object-kind-plumbing.md`](object-kind-plumbing.md),
[`time-management-prior-art.md`](time-management-prior-art.md) (note its §4.8
verdict is superseded), [`rn-alarm-absorption.md`](rn-alarm-absorption.md).
