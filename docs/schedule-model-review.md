# Schedule and recorded-time model: review walkthrough

This is the first part of the Rust code review: representing plans, calculating
calendar occurrences, and recording what actually happened. Storage, revision
security, sync, and native alarm delivery are subsequent review topics.

## Reading order

1. [item.rs](../crates/schedule/src/item.rs): the entities and their relationships.
2. [time.rs](../crates/schedule/src/time.rs): floating, zoned, and all-day time.
3. [recurrence.rs](../crates/schedule/src/recurrence.rs): repetition rules.
4. [engine.rs](../crates/schedule/src/engine.rs): calculating occurrences.
5. The client integration files below: connecting the model to the app.

The `clipper-schedule` crate performs calculations without storage, networking,
or encryption. Its inputs can be tested independently of any platform UI.

## The four central types

| Type                 | Meaning                            | Example                                        | Stored?         |
| -------------------- | ---------------------------------- | ---------------------------------------------- | --------------- |
| `ScheduleItem`       | A plan with a repeat rule          | Morning routine, daily at 07:00 for 30 minutes | Once per series |
| `Occurrence`         | One calculated instance            | Tomorrow's 07:00–07:30 routine                 | No              |
| `OccurrenceOverride` | An override to one instance        | Move tomorrow's routine to 08:00, or cancel it | Yes             |
| `ActualRecord`       | One session of time actually spent | Worked from 07:12 to 07:48                     | Yes             |

A daily plan does not create hundreds of stored objects. Editing or deleting a
plan does not rewrite recorded time, because actuals are separate records.

`ScheduleItem.reference` optionally points to another Clipper object using the shared `ObjectId` UUID wrapper. The reference stores only the ID; the target supplies its kind when resolved. This is
different from `ActualRecord.planned`, which optionally links recorded time to
a particular scheduled occurrence through `PlannedRef`.

`PlannedRef` contains a series ID and a `RecurrenceId`: together they identify
“Tuesday's morning routine,” not merely “morning routine.” An override uses the
same identity. Moving Tuesday's start to 08:00 preserves its original 07:00
identity so it can still be matched to the rule position it replaces.

That pair alone does not preserve the plan across edits. `PlannedRef` also pins
the exact schedule object revision and optional standalone override revision,
with each reference containing the storage object ID, revision number and signed
body hash. It captures the observer timezone and resolved planned UTC span at
timer start. Timer stop retains these fields unchanged.

A standalone `OccurrenceOverride` wraps a pure `OccurrenceOverrideData` plus the
base schedule revision it was authored against. The client checks compatibility
before using that override with a newer schedule. Title, reference and alarm
changes are compatible; changes to identity, scheduled span or recurrence are
not. Structural edits with local overrides are blocked until an explicit
override-handling UI exists. An old Tuesday override is not silently retargeted
to a new Thursday schedule. If a synced edit is incompatible or its base cannot
be loaded, the client skips that series and publishes a Schedule UI warning.
Unaffected series continue to render and generate alarms.

Provider overrides live inside the imported event object as
`OccurrenceOverrideData` values. Pinning that object revision captures them; they
do not have independent revisions.

A recurrence identity is a local date/time for floating events, an absolute
instant for zoned events, or a date for all-day events. Floating identities must
not change when the observer travels to a different timezone.

Domain IDs and encrypted storage object IDs are distinct. `PlannedRef.item`
identifies the domain series; client commands for updating/deleting a stored
object address its storage object ID.

## Time representation and why TimedStart exists

`chrono` provides date/time values and `chrono-tz` provides named IANA timezones.
`TimedStart` does not replace those capabilities. It records which timezone
policy the user chose:

- `Floating(NaiveDateTime)`: use the observer's timezone when resolving the time.
- `Zoned { local: NaiveDateTime, zone: Tz }`: always use the stored timezone.

For example, a floating 07:00 routine follows you from Berlin to Tokyo. A 07:00
Berlin meeting stays tied to Berlin and appears at a different local time in
Tokyo. Both need timezone calculations, but the choice of timezone is different.

A `DateTime<Tz>` represents an already-resolved instant in a zone. It cannot on
its own also express “resolve this wall-clock time in whichever zone the user
is in.” The enum keeps that distinction explicit at every call site and in
serialized data, using the library's types inside each variant. A struct with
`local: NaiveDateTime` and `zone: Option<Tz>` could express the same distinction;
the enum is a readability choice, not a requirement imposed by the library.

The zoned variant stores a wall-clock time and named zone as the recurrence
intent: “09:00 Europe/Berlin” continues at 09:00 across daylight-saving changes.
A fixed UTC offset is not equivalent to a named timezone. A UTC-zoned one-off
can express a fixed instant regardless of travel.

`ScheduleSpan` adds either a positive minute duration to a timed start, or a
positive number of calendar days to an all-day date. All-day values are separate
from `TimedStart` because they have no time of day.

`ScheduleSpan::resolve(observer)` returns a `TimeRange` with UTC start/end:

- Timed duration means elapsed minutes.
- All-day duration means local calendar days, potentially 23 or 25 hours around
  daylight-saving changes.
- Intervals exclude their end: 07:00–08:00 and 08:00–09:00 do not overlap.
- Ambiguous local times use the earlier instant; a nonexistent local time shifts
  forward by the timezone transition's offset change. These are application
  policies implemented using the library's timezone resolution results.

Actuals always use concrete UTC instants. Their recorded times do not change
when the observer changes timezone.

## Recurrence rules

`Recurrence` is `Once`, `Every(Cadence)`, or `Raw`.

A cadence combines a frequency (daily, weekly, monthly, yearly), a positive
interval, and an ending condition (never, occurrence count, or an end instant).
Structured rules describe intent without exposing arbitrary rule strings to
ordinary callers. Imported rules that the structured model cannot express use
`RawRule`; these are validated and preserved rather than simplified into a
potentially different schedule.

## Calculating the calendar

The recurrence engine receives a series, compatible pure overrides, a date window, and an
observer timezone. It:

1. Generates candidate wall-clock times using the `rrule` library (or directly
   resolves a one-off). The library is given a UTC wall-clock `DTSTART`, so it
   does pure calendar arithmetic and never resolves a local time itself; every
   candidate is then resolved through the policy in `time.rs`. A series whose
   own start falls in a DST gap expands like any other, and a floating
   identity is the same for every observer.
2. Matches candidates to overrides, removing cancellations and substituting
   rescheduled spans.
3. Also includes occurrences moved into the window from outside it.
4. Resolves spans into UTC and sorts them by start.

`overlapping_occurrences()` includes events that begin before the window but
continue into it, such as overnight events. Plain `occurrences()` selects by
start time instead.

Expansion has candidate and historical-scan limits. Exceeding a limit produces
an error rather than silently returning a truncated calendar. The client
currently logs expansion errors and skips the affected series; it does not
fail the entire calendar or show a dedicated per-series error in the UI.

## Client integration and code flow

| File                                                         | Relevant responsibility                                                                                                                         |
| ------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| [client/schedule.rs](../crates/client/src/schedule.rs)       | `ScheduleRecord` wraps items, overrides, actuals, sources, and imported events for encrypted storage; conversion helpers produce display values |
| [client/engine.rs](../crates/client/src/engine.rs)           | `expand_schedule`, `start_actual`, and `stop_actual` coordinate the operations                                                                  |
| [client/local_store.rs](../crates/client/src/local_store.rs) | Supplies local schedule records to the client engine                                                                                            |
| [app-types/lib.rs](../crates/app-types/src/lib.rs)           | `ScheduleItemView`, `OccurrenceView`, and `ActualView` cross the UI boundary                                                                    |

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
  -> receives the displayed occurrence revision context
  -> serializes local timer commands with a lock
  -> validates the context against the current local snapshot and resolves the occurrence
  -> rejects stale/invalid context before stopping any timer
  -> stops the currently known running timer, if any
  -> creates an ActualRecord with Running { started: current UTC time }
     and pins the schedule/override revisions, observer zone and planned bounds
  -> saves it through the encrypted-object system

Stop timer
  -> stop_actual loads the actual and its accepted revision head
  -> changes Running to Complete { original start, current end }
  -> writes a new revision of the same stored object
```

The timer writes on start and stop, not every second. Elapsed display time is
calculated from the start timestamp. Stopping clamps the end to at least the
start in case the device clock moved backwards.

The local lock is not a global lock across disconnected devices. Starting a
new timer also involves separate stop/create operations, not one atomic
transaction across both objects.

## Assessment and questions for review

Separating plans, overrides, and actuals is a useful foundation: planned time
can change without rewriting the history of time spent.

An actual stores no independent title or notes. Its plan description comes from
the pinned immutable schedule revision, not the current definition. The client
uses authenticated revision-specific reads with bounded payload checks and a
separate historical cache; it never replaces the current head or changes its
rollback anchor while reading old history. The session-epoch-scoped, memory-only
cache holds at most 64 definitions. `recorded_plan(actual_id)` returns the pinned
schedule, effective override and captured plan context for a future details UI.
Routine state publication does not perform historical network reads; initial
titles may be placeholders until the explicit actuals-window read resolves them.
Missing or permanently purged history
is unavailable rather than substituted with the current definition. The actual
retains its resolved planned bounds and recorded times regardless. Previously
verified history may remain in memory after server purge until eviction, logout
or restart; remote purge does not erase content already held by a client.

Future calendar occurrences and alarms still use the latest definition. This
does not implement a full historical calendar or “this and all future” edits;
effective dates or series splitting would need a separate design.

The future recorded-time editor should consider manual entry,
start/end correction, and attaching or reassigning an actual to an occurrence.
Those workflows are not supplied merely by having these types.

Read [behaviour.rs](../crates/schedule/tests/behaviour.rs) alongside the model for
concrete timezone-travel, cancellation, rescheduling, daylight-saving, overnight,
and expansion-limit examples. [corpus.rs](../crates/schedule/tests/corpus.rs)
covers recurrence-library comparisons, and
[schedule_integration_tests.rs](../crates/client/src/schedule_integration_tests.rs)
exercises revisions, timers, feeds, and two-client sync against an isolated server.

## Validation status for the revision-aware change

Rust workspace tests pass: 420 in total on the polished branch, including 140
in the schedule crate (13 unit, 55 adversarial, 33 behaviour, 15 corpus, 2
imported-rule, 22 ingest) and 104 in the server. The live integration covers
pinned plans through edits, overridden/provider occurrences, stale selections,
deleted history, and unchanged sync heads. Workspace Clippy, wasm/mobile bridge
checks, web type/lint/tests and standalone web build also pass. The 2026-09-12
pre-review polish and its QA are recorded in
[scheduler-review.md](scheduler-review.md#pre-review-polish-2026-09-12).
