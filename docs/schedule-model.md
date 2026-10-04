# Schedule model

This document describes how Clipper represents plans, calculates calendar
occurrences, records time actually spent, and rings alarms on Android.

The domain model lives in the `clipper-schedule` crate (`crates/schedule`). It
performs calculations without storage, networking or encryption, so its inputs
can be tested independently of any platform UI. Its main files:

1. [item.rs](../crates/schedule/src/item.rs): the entities and their relationships.
2. [time.rs](../crates/schedule/src/time.rs): floating, zoned and all-day time.
3. [recurrence.rs](../crates/schedule/src/recurrence.rs): repetition rules.
4. [engine.rs](../crates/schedule/src/engine.rs): calculating occurrences.
5. [alarm.rs](../crates/schedule/src/alarm.rs): turning occurrences into alarms.

## Storage and sync

Schedule data is end-to-end encrypted like clipboard items and files. The
server stores ciphertext and never reads an event. Every piece of work that
needs the plaintext runs on a client:

- Recurrence expansion runs on each client. The server cannot filter by date,
  because it cannot read dates, so each client holds every schedule object and
  selects the date window it needs locally.
- Calendar feeds are fetched by native clients. The browser shows synced
  imported events but refuses to refresh a feed, because a page cannot read a
  third-party calendar URL. [calendar-imports.md](calendar-imports.md) specifies imports.

Each of the following is its own encrypted object with its own revisions:

- A schedule series. A repeating plan is stored once, as its rule.
  Occurrences are calculated and never stored, so a daily plan for a year is
  one object.
- A standalone override, written only when one occurrence is moved, retimed or
  cancelled.
- An actual, one session of time actually spent.
- A calendar source and each imported event.

A block is stored as a start plus a duration. The week grid is only a view:
the composer snaps a typed duration to 5 minutes, and an imported meeting that
is not grid-aligned is drawn where it actually falls.

## The four central types

- `ScheduleItem`: a plan with a repeat rule, for example a morning routine
  daily at 07:00 for 30 minutes. Stored once per series.
- `Occurrence`: one calculated instance, for example tomorrow's 07:00 to 07:30
  routine. Not stored.
- `OccurrenceOverride`: a change to one instance, for example moving tomorrow's
  routine to 08:00 or cancelling it. Stored.
- `ActualRecord`: one session of time actually spent, for example work from
  07:12 to 07:48. Stored.

Editing or deleting a plan does not rewrite recorded time, because actuals are
separate records.

`ScheduleItem.reference` optionally points to another Clipper object using the
shared `ObjectId` UUID wrapper. The reference stores only the ID; the target
supplies its kind when resolved. This is different from `ActualRecord.planned`,
which optionally links recorded time to a particular scheduled occurrence
through `PlannedRef`.

`PlannedRef` contains a series ID and a `RecurrenceId`: together they identify
"Tuesday's morning routine", not merely "morning routine". An override uses the
same identity. Moving Tuesday's start to 08:00 keeps its original 07:00
identity, so it still matches the rule position it replaces.

That pair alone does not preserve the plan across edits. `PlannedRef` also pins
the exact schedule object revision and the optional standalone override
revision. Each pin holds the storage object ID, revision number and signed body
hash. `PlannedRef` also captures the observer timezone and the resolved planned
UTC span at timer start. Stopping the timer keeps these fields unchanged.

A standalone `OccurrenceOverride` wraps a pure `OccurrenceOverrideData` plus the
base schedule revision it was written against. The client checks compatibility
before using that override with a newer schedule. Title, reference and alarm
changes are compatible; changes to identity, scheduled span or recurrence are
not. The client refuses a timing or recurrence edit to a series that has local
overrides, so an old Tuesday override is never retargeted to a new Thursday
schedule. If a synced edit is incompatible, or its base revision cannot be
loaded, the client skips that series and publishes a warning in the Schedule
UI. Other series still render and generate alarms.

Provider overrides live inside the imported event object as
`OccurrenceOverrideData` values. Pinning that object revision captures them;
they have no revisions of their own.

A recurrence identity is a local date and time for floating events, an
absolute instant for zoned events, or a date for all-day events. Floating
identities must not change when the observer travels to a different timezone.

Domain IDs and storage object IDs are distinct. `PlannedRef.item` identifies
the domain series; client commands that update or delete a stored object
address its storage object ID.

## Time representation and why TimedStart exists

`chrono` provides date and time values and `chrono-tz` provides named IANA
timezones. `TimedStart` does not replace those. It records which timezone
policy the user chose:

- `Floating(NaiveDateTime)`: use the observer's timezone when resolving the time.
- `Zoned { local: NaiveDateTime, zone: Tz }`: always use the stored timezone.

For example, a floating 07:00 routine follows you from Berlin to Tokyo. A 07:00
Berlin meeting stays tied to Berlin and appears at a different local time in
Tokyo. Both need timezone calculations, but the choice of timezone differs.

A `DateTime<Tz>` is an instant already resolved in a zone. On its own it cannot
also express "resolve this wall-clock time in whichever zone the user is in".
The enum keeps that distinction explicit at every call site and in serialized
data, using the library's types inside each variant. A struct with
`local: NaiveDateTime` and `zone: Option<Tz>` could express the same
distinction; the enum is a readability choice, not a requirement of the
library.

The zoned variant stores a wall-clock time and a named zone as the recurrence
intent: "09:00 Europe/Berlin" stays at 09:00 across daylight-saving changes. A
fixed UTC offset is not equivalent to a named timezone. A UTC-zoned one-off
expresses a fixed instant regardless of travel.

`ScheduleSpan` adds either a positive number of minutes to a timed start, or a
positive number of calendar days to an all-day date. All-day values are
separate from `TimedStart` because they have no time of day.

`ScheduleSpan::resolve(observer)` returns a `TimeRange` with UTC start and end:

- A timed duration means elapsed minutes.
- An all-day duration means local calendar days, which can be 23 or 25 hours
  around daylight-saving changes.
- Intervals exclude their end: 07:00 to 08:00 and 08:00 to 09:00 do not overlap.
- An ambiguous local time uses the earlier instant; a nonexistent local time
  shifts forward by the transition's offset change. These are application
  policies built on the library's timezone resolution results.

Actuals always use concrete UTC instants. Their recorded times do not change
when the observer changes timezone.

## Recurrence rules

`Recurrence` is `Once`, `Every(Cadence)`, or `Imported { import, uid }`.

A cadence combines a frequency (daily, weekly, monthly, yearly), a positive
interval, and an ending condition (never, an occurrence count, or an end
instant). Structured rules describe intent without exposing arbitrary rule
strings to ordinary callers. An imported rule that `Cadence` cannot express is
re-read from the raw ICS snapshot at expansion time, using its import and event
UID.

## Calculating the calendar

The recurrence engine receives a series, its compatible pure overrides, a date
window and an observer timezone. It:

1. Generates candidate wall-clock times using the `rrule` library, or resolves a
   one-off directly. The library is given a UTC wall-clock `DTSTART`, so it does
   pure calendar arithmetic and never resolves a local time itself. Candidates
   more than a day before the window are skipped; every other candidate is
   resolved through the policy in `time.rs`. An `UNTIL` given as an instant is
   applied to the resolved instant, not the wall clock. A series whose own start
   falls in a daylight-saving gap expands like any other, and a floating
   identity is the same for every observer.
2. Matches candidates to overrides, removing cancellations and substituting
   rescheduled spans.
3. Includes occurrences moved into the window from outside it.
4. Resolves spans into UTC and sorts them by start.

`overlapping_occurrences()` includes events that begin before the window but
continue into it, such as overnight events. Plain `occurrences()` selects by
start time instead.

Expansion scans at most 65,535 candidates per rule and produces at most 10,000
candidates per expansion. Exceeding a limit produces an error rather than a
silently truncated calendar, with one exception: the `rrule` library can give
up on a sparse imported rule without reporting it, so such a rule yields fewer
occurrences or none ([issues.md](issues.md), entry 84). The client adds a warning for
each series that fails to expand and skips it; the calendar still renders the
other series.

## Client integration and code flow

- [client/schedule.rs](../crates/client/src/schedule.rs): `ScheduleRecord`
  wraps items, overrides, actuals, calendar sources and imported events for
  encrypted storage; conversion helpers produce display values.
- [client/engine.rs](../crates/client/src/engine.rs): `expand_schedule`,
  `next_alarms`, `start_actual` and `stop_actual` coordinate the operations.
- [client/local_store.rs](../crates/client/src/local_store.rs): supplies local
  schedule records to the client engine.
- [app-types/lib.rs](../crates/app-types/src/lib.rs): `ScheduleItemView`,
  `OccurrenceView` and `ActualView` cross the UI boundary.

```text
UI requests a date window
  -> client/engine.rs loads local schedule records and gathers overrides
  -> schedule/engine.rs expands rules and applies overrides
  -> schedule/time.rs resolves times
  -> client/schedule.rs converts occurrences into display values
  -> UI renders the calendar
```

Imported events are adapted into series for the same expansion engine. Their
source and cancellation labels are carried separately for display.

## Recording time

```text
Start timer
  -> receives the displayed occurrence's revision context
  -> serializes local timer commands with a lock
  -> validates the context against the current local snapshot and resolves the occurrence
  -> rejects a stale or invalid context before stopping any timer
  -> stops every running timer this device has synced
  -> creates an ActualRecord with Running { started: current UTC time }
     and pins the schedule and override revisions, observer zone and planned bounds
  -> saves it through the encrypted-object system

Stop timer
  -> stop_actual loads the actual and its accepted revision head
  -> changes Running to Complete { original start, current end }
  -> writes a new revision of the same stored object
```

The timer writes on start and stop, not every second. Elapsed display time is
calculated from the start timestamp. Stopping clamps the end to at least one
second after the start, in case the device clock moved backwards.

The local lock does not cover other devices. Starting a new timer is a
separate stop and create, not one atomic transaction across both objects.

### Plans across edits

An actual stores no title or notes of its own. Its plan description comes from
the pinned schedule revision, not the current definition. The client reads
that revision through an authenticated revision-specific read with bounded
payload checks, and keeps the result in a separate in-memory cache of at most
64 definitions, scoped to the session. A historical read never replaces the
current head or changes its rollback anchor. `recorded_plan(actual_id)` returns
the pinned schedule, the effective override and the captured plan context.

Routine state updates do not make historical network reads, so a recorded
session's title can show a placeholder until the actuals-window read resolves
it. Missing or purged history is shown as unavailable, never replaced with the
current definition. The actual keeps its resolved planned bounds and recorded
times either way. History the client has already verified can stay in memory
after a server purge until it is evicted, the user logs out or the app
restarts.

Calendar occurrences and alarms always use the latest definition of a series.

## Alarms on Android

A `ScheduleItem` can carry an alarm policy with a lead time. `plan_alarms`
turns a series' occurrences into planned alarms, each with a fire time, the
occurrence start and the series title as its label, and drops any alarm whose
fire time has passed.

Imported events use invitation rules and start-relative VALARMs, with a
five-minute fallback. Each source can silence its imported alarms. Provider
moves and cancellations apply before planning. `next_alarms` includes complete
active imported batches alongside user-authored items. Details are in
[calendar-imports.md](calendar-imports.md).

The Android app asks Rust (`next_alarms`) for the alarms in the next seven days
and hands that list to Kotlin. It sends a new list when the app becomes active
or when the schedule or session changes. A phone whose app is never opened runs
out of alarms after seven days.

The Kotlin side is an Expo local module at `mobile/modules/clipper-alarm`. Its
own `AndroidManifest.xml` merges into the app's and declares the permissions,
receivers, the ring service and the ring activity.

- The mirror is a copy of the list in device-protected storage. It holds only
  the upcoming fire times and labels: no schedule data and no keys. The
  schedule is ciphertext that can be read only after unlock and login, so the
  mirror is what lets an alarm ring after a reboot before first unlock.
- Kotlin computes no recurrence, so there is one recurrence implementation. It
  registers the nearest 24 alarms from the mirror, each as a one-shot
  `AlarmManager.setAlarmClock`. Each fire registers the next alarms from the
  mirror.
- The app requests `USE_EXACT_ALARM`, which Android grants at install for an
  alarm app and the user cannot revoke.
- Every component on the alarm path is `directBootAware`. After
  `LOCKED_BOOT_COMPLETED`, `BOOT_COMPLETED`, an app update, or a time or
  timezone change, the boot receiver registers the alarms in the mirror again.
  A timezone change does not convert floating alarms; the mirror holds
  instants, so they keep the old zone's time until the app sends a new list
  ([issues.md](issues.md), entry 77).
- The ring screen is a native activity, not React Native. It must appear when
  the app process was killed or the JavaScript bundle cannot load.
- Ringing runs in a `mediaPlayback` foreground service and stops on dismiss or
  after ten minutes unanswered. Android 15 and later can refuse to start this
  service for an alarm due right after a reboot ([issues.md](issues.md),
  entry 82).

## Tests

- [behaviour.rs](../crates/schedule/tests/behaviour.rs): timezone travel,
  cancellation, rescheduling, daylight saving, overnight events and expansion
  limits.
- [corpus.rs](../crates/schedule/tests/corpus.rs): comparisons against the
  recurrence library.
- [schedule_integration_tests.rs](../crates/client/src/schedule_integration_tests.rs):
  revisions, timers, feeds and two-client sync against an isolated server.
