# Time-Management Prior Art

Background research for [`docs/schedule-plan.md`](schedule-plan.md): how
iCalendar models recurrence exceptions, how real time-tracking products relate a
planned block to what actually happened, timer state machines, Rust recurrence
crates, and whether dense time blocks want a grid or an interval model.

**Provenance.** Produced by a delegated research agent on 2026-09-07, then
spot-checked. Verified independently against crates.io and docs.rs: `rrule`
0.14.0 was last released 2025-04-20 (~17 months stale, 1.1M downloads);
`calcard` 0.3.13 was released 2026-08-25 and its docs do claim "Recurrence rules
expansion: Accurately computes and enumerates repeating events based on
iCalendar and JSCalendar RRULEs".

**Section 4.6's `rrule` bug list is superseded — do not act on it.** Those are
open GitHub issues, not verified defects, and two of them were tested directly
afterwards. #148 (`NWeekday::Nth` losing its ordinal on serialization) does not
reproduce. #139 (`FREQ=YEARLY;BYMONTHDAY` expanding monthly) reproduces but is
correct RFC 5545 behaviour, confirmed by `calcard` producing identical output.
Section 4.8's verdict, which leans away from `rrule` on the strength of that
list, is therefore wrong. See
[the recurrence-engine section of the plan](schedule-plan.md#recurrence-engine-measured-not-assumed)
for the measured comparison and the actual decision.

**Where this repo disagrees with section 5.** The report recommends a fixed
grid/bitmap as the internal source of truth. See
[D5](schedule-plan.md#d5-intervals-are-the-stored-form-the-grid-is-a-view) for
why this plan stores intervals instead — the bitmap literature it cites is about
availability search across many participants, which is a different problem from
a single user's planner carrying rich per-block metadata.

---


## 1. RFC 5545 instance overrides, and Google Calendar's model of the same

### 1.1 RECURRENCE-ID (RFC 5545 §3.8.4.4)

> **Property Name:** RECURRENCE-ID
> **Purpose:** This property is used in conjunction with the "UID" and "SEQUENCE" properties to identify a specific instance of a recurring "VEVENT", "VTODO", or "VJOURNAL" calendar component. The property value is the original value of the "DTSTART" property of the recurrence instance.
>
> **Value Type:** The default value type is DATE-TIME. The value type can be set to a DATE value type. This property MUST have the same value type as the "DTSTART" property contained within the recurring component. Furthermore, this property MUST be specified as a date with local time if and only if the "DTSTART" property contained within the recurring component is specified as a date with local time.
>
> **Description:** The full range of calendar components specified by a recurrence set is referenced by referring to just the "UID" property value corresponding to the calendar component. The "RECURRENCE-ID" property allows the reference to an individual instance within the recurrence set.
>
> The DATE-TIME value is set to the time when the original recurrence instance would occur; meaning that if the intent is to change a Friday meeting to Thursday, the DATE-TIME is still set to the original Friday meeting.
>
> The "RECURRENCE-ID" property is used in conjunction with the "UID" and "SEQUENCE" properties to identify a particular instance of a recurring event, to-do, or journal. For a given pair of "UID" and "SEQUENCE" property values, the "RECURRENCE-ID" value for a recurrence instance is fixed.

The RANGE parameter description in the same section:

> The "RANGE" parameter is used to specify the effective range of recurrence instances from the instance specified by the "RECURRENCE-ID" property value. The value for the range parameter can only be "THISANDFUTURE" to indicate a range defined by the given recurrence instance and all subsequent instances. Subsequent instances are determined by their "RECURRENCE-ID" value and not their current scheduled start time. Subsequent instances defined in separate components are not impacted by the given recurrence instance. When the given recurrence instance is rescheduled, all subsequent instances are also rescheduled by the same time difference. For instance, if the given recurrence instance is rescheduled to start 2 hours later, then all subsequent instances are also rescheduled 2 hours later.
>
> Similarly, if the duration of the given recurrence instance is modified, then all subsequence instances are also modified to have this same duration.
>
> Note: The "RANGE" parameter may not be appropriate to reschedule specific subsequent instances of complex recurring calendar component. Assuming an unbounded recurring calendar component scheduled to occur on Mondays and Wednesdays, the "RANGE" parameter could not be used to reschedule only the future Monday instances to occur on Tuesday instead. In such cases, the calendar application could simply truncate the unbounded recurring calendar component (i.e., with the "COUNT" or "UNTIL" rule parts), and create two new unbounded recurring calendar components for the future instances.

Source: https://www.rfc-editor.org/rfc/rfc5545.txt (§3.8.4.4)

### 1.2 Master VEVENT vs override VEVENTs

The model is: a **master** VEVENT carries `UID:x`, an `RRULE`, and `DTSTART`. Each **override** is a separate VEVENT (in the same iCalendar object) with the **same `UID:x`**, a `RECURRENCE-ID` equal to the *original* instance start it replaces, a new `DTSTART`/`DTEND`, and optionally its own `SEQUENCE`. The override's RECURRENCE-ID stays pinned to the original occurrence time even when the instance is moved — that is what binds the override to its slot in the series.

In practice:

- master: `UID:x`, `RRULE:FREQ=WEEKLY;...`, `DTSTART:20260105T090000Z`
- override: `UID:x`, `RECURRENCE-ID:20260112T090000Z`, `DTSTART:20260113T100000Z` (moved instance)

### 1.3 EXDATE (§3.8.5.1)

> **Purpose:** This property defines the list of DATE-TIME exceptions for recurring events, to-dos, journal entries, or time zone definitions.
>
> **Description:** The exception dates, if specified, are used in computing the recurrence set. The recurrence set is the complete set of recurrence instances for a calendar component. The recurrence set is generated by considering the initial "DTSTART" property along with the "RRULE", "RDATE", and "EXDATE" properties contained within the recurring component. The "DTSTART" property defines the first instance in the recurrence set. ... The final recurrence set is generated by gathering all of the start DATE-TIME values generated by any of the specified "RRULE" and "RDATE" properties, and then excluding any start DATE-TIME values specified by "EXDATE" properties. This implies that start DATE-TIME values specified by "EXDATE" properties take precedence over those specified by inclusion properties (i.e., "RDATE" and "RRULE").

Source: https://www.rfc-editor.org/rfc/rfc5545.txt (§3.8.5.1)

### 1.4 RDATE (§3.8.5.2)

> **Purpose:** This property defines the list of DATE-TIME values for recurring events, to-dos, journal entries, or time zone definitions.
>
> **Description:** This property can appear along with the "RRULE" property to define an aggregate set of repeating occurrences. When they both appear in a recurring component, the recurrence instances are defined by the union of occurrences defined by both the "RDATE" and "RRULE".

Source: https://www.rfc-editor.org/rfc/rfc5545.txt (§3.8.5.2)

### 1.5 SEQUENCE (§3.8.7.4)

> **Purpose:** This property defines the revision sequence number of the calendar component within a sequence of revisions.
>
> **Description:** When a calendar component is created, its sequence number is 0. It is monotonically incremented by the "Organizer's" CUA each time the "Organizer" makes a significant revision to the calendar component. ... Recurrence instances of a recurring component MAY have different sequence numbers.

The last sentence is the key point for overrides: **the master and each override can evolve their own SEQUENCE counters**, so a given (UID, SEQUENCE, RECURRENCE-ID) triple identifies one revision of one instance.

Source: https://www.rfc-editor.org/rfc/rfc5545.txt (§3.8.7.4)

### 1.6 RANGE parameter: THISANDFUTURE vs THISANDPRIOR (§3.2.13)

> **Parameter Name:** RANGE
> **Purpose:** To specify the effective range of recurrence instances from the instance specified by the recurrence identifier specified by the property.
>
> ```
> rangeparam = "RANGE" "=" "THISANDFUTURE"
> ; To specify the instance specified by the recurrence identifier
> ; and all subsequent recurrence instances.
> ```
>
> **Description:** ... The parameter value can only be "THISANDFUTURE" to indicate a range defined by the recurrence identifier and all subsequent instances. The value "THISANDPRIOR" is deprecated by this revision of iCalendar and MUST NOT be generated by applications.
>
> **Example:** `RECURRENCE-ID;RANGE=THISANDFUTURE:19980401T133000Z`

Appendix A (changes from RFC 2445):

> 2. The "THISANDPRIOR" value can no longer be used with the "RANGE" parameter.

So: **THISANDFUTURE** = "the identified instance plus every later instance in the series"; **THISANDPRIOR** existed in RFC 2445 but is deprecated in 5545 and must not be emitted.

Source: https://www.rfc-editor.org/rfc/rfc5545.txt (§3.2.13, Appendix A)

### 1.7 Google Calendar API v3 representation

On the Events resource (https://developers.google.com/calendar/api/v3/reference/events):

> **recurringEventId** — `string`: For an instance of a recurring event, this is the id of the recurring event to which this instance belongs. Immutable.

> **originalStartTime** — nested object: For an instance of a recurring event, this is the time at which this event would start according to the recurrence data in the recurring event identified by recurringEventId. It uniquely identifies the instance within the recurring event series even if the instance was moved to a different time. Immutable.

On `events.list` (https://developers.google.com/calendar/api/v3/reference/events/list):

> **singleEvents** — `boolean`: Whether to expand recurring events into instances and only return single one-off events and instances of recurring events, but not the underlying recurring events themselves. Optional. The default is False.

And ordering: `"startTime": Order by the start date/time (ascending). This is only available when querying single events (i.e. the parameter singleEvents is True)`.

The recurring-events guide (https://developers.google.com/calendar/api/guides/recurringevents):

> By default, the events.list method returns single events, recurring events, and exceptions; instances that are not exceptions are not returned.
>
> If the singleEvents parameter is set to true, all individual instances appear in the result, but underlying recurring events don't. ... Individual instances are similar to single events. Unlike their parent recurring events, instances don't have the recurrence field set.
>
> The following event fields are specific to instances:
> - recurringEventId — the ID of the parent recurring event this instance belongs to
> - originalStartTime — the time this instance starts according to the recurrence data in the parent recurring event. This can be different from the actual start time if the instance was rescheduled. It uniquely identifies the instance within the recurring event series even if the instance was moved.

**Deleted instances** appear as events with `status = "cancelled"`:

> "cancelled" - The event is cancelled (deleted). The list method returns cancelled events only on incremental sync (when syncToken or updatedMin are specified) or if the showDeleted flag is set to true. ... Cancelled exceptions of an uncancelled recurring event indicate that this instance should no longer be presented to the user. Clients should store these events for the lifetime of the parent recurring event. Cancelled exceptions are only guaranteed to have values for the id, recurringEventId and originalStartTime fields populated. The other fields might be empty.

A cancelled instance looks like:

```json
{
  "kind": "calendar#event",
  "id": "instanceId",
  "status": "cancelled",
  "recurringEventId": "recurringEventId",
  "originalStartTime": "2011-06-03T10:00:00.000-07:00"
}
```

### 1.8 Where Google deviates from RFC 5545

1. **No native `RANGE=THISANDFUTURE`.** For "change this and all future instances", Google's documented pattern is a two-request series split, not a RECURRENCE-ID with a RANGE parameter:

   > To change all the instances of a recurring event on or after a given (target) instance, make two separate API requests. These requests split the original recurring event into two: ... (1) Call events.update to trim the original recurring event of the instances to be updated. Do this by setting the UNTIL component of the RRULE to point before the start time of the first target instance. ... (2) Call events.insert to create a new recurring event with all the same data as the original, except for the change you are making. The new recurring event must have the start time of the target instance.

   (Flag: I found no Google statement that literally says "we do not support RANGE=THISANDFUTURE"; the two-request split is simply the documented workflow, which is the safest way to characterize the deviation.)

2. **Deletions are cancelled events, not EXDATE on the master.** Pure RFC 5545 typically deletes one instance by adding `EXDATE` to the master component; Google surfaces a separate Event resource with `status="cancelled"`, `recurringEventId`, and `originalStartTime`, and requires clients to retain it for the lifetime of the parent series.

3. **Exceptions are first-class Event resources**, not additional VEVENT components inside one iCalendar object. The parent carries the `recurrence[]` array; instances do not.

4. **`recurrence[]` omits DTSTART/DTEND:**

   > List of RRULE, EXRULE, RDATE and EXDATE lines for a recurring event, as specified in RFC5545. Note that DTSTART and DTEND lines are not allowed in this field; event start and end times are specified in the start and end fields.

5. **`iCalUID` is shared across the series; `id` is per-instance:**

   > Note that the iCalUID and the id are not identical and only one of them should be supplied at event creation time. One difference in their semantics is that in recurring events, all occurrences of one event have different ids while they all share the same iCalUIDs.

### 1.9 Design takeaway

The RFC 5545 / Google model converges on: one master entity carrying the rule; per-instance override entities keyed by (series id, original occurrence start); deletions as first-class tombstone records pinned to an occurrence. For a syncing data model, this maps directly to: `series { id, rrule, dtstart, duration, ... }` + `exception { series_id, original_start, new_start?, new_end?, deleted, sequence }`.

### URLs opened (section 1)

- https://www.rfc-editor.org/rfc/rfc5545.txt
- https://developers.google.com/calendar/api/v3/reference/events
- https://developers.google.com/calendar/api/v3/reference/events?hl=en
- https://developers.google.com/calendar/api/v3/reference/events/list?hl=en
- https://developers.google.com/calendar/api/guides/recurringevents?hl=en

---

## 2. Planned vs actual: how real products model it

### 2.1 Summary table

| Product | Planned entity | Actual entity | Relationship | Key actual-record fields |
|---|---|---|---|---|
| **Toggl Track** (classic) | None (only `estimated_seconds` on projects/tasks) | `Time Entry` | **Actual-only / separate entity** — time entries are the core object | `start`, `stop`, `duration` (seconds; negative while running), `description`, `project_id`, `task_id`, `tags`, `billable`, `created_with`, `duronly` |
| **Toggl 2.0** | Calendar "time block" / scheduled task block | `Time Entry` | **Separate entity, action-linked** — marking a scheduled block Done auto-creates a time entry | same Toggl time-entry fields |
| **Clockify** | `Scheduled Assignment` (Schedule feature) | `Time Entry` | **Separate entities** — reports compare "assigned vs actual working hours" | `id`, `description`, `start`, `end`, `timeInterval {start, end, duration}`, `projectId`, `taskId`, `tagIds`, `billable`, `customFields` |
| **Sunsama** | Task + `planned time` field + calendar working session | `actual time` field on the same task | **Mutation of the planned entity** — actual can even fall back to planned | `planned time`, `actual time` (task properties) |
| **Motion** | Task with `Duration` (planned) | `Completed At` field on the same task | **Mutation of the planned entity** | `Duration` (planned), `Completed At` (actual), `Status` |
| **Google Calendar Goals** (deprecated Nov 2022) | Goal event (auto-scheduled) | None distinct | **Mutation of the planned event** — defer/complete only, no actual-duration record | N/A |
| **Timing** | None | `Time Entry` + automatic app-usage records | **Actuals-only** | `start_date`, `end_date`, `duration`, `project`, `title`, `notes`, `is_running`, `billing_status` |
| **RescueTime** | None | Analytic activity rows | **Actuals-only** | `timestamp`, `duration`, `activity`, `category`, `productivity`, `document`, `source` (row headers inferred from integration code) |

### 2.2 Toggl Track

- v8 time-entry fields: `description`, `wid`, `pid`, `tid`, `billable`, `start`, `stop`, `duration`, `created_with`, `tags`, `duronly`, `at` (https://github.com/toggl/toggl_api_docs/blob/master/chapters/time_entries.md).
- v9 `/me` schema adds: `client_id`, `client_name`, `project_active`, `project_billable`, `project_color`, `project_name`, `shared_with`, `tag_ids`, `task_active`, `task_name`, `uid`/`user_id`, `user_name`, `workspace_id` (https://engineering.toggl.com/docs/track/api/me/).
- Running timer: "If the time entry is currently running, the `duration` attribute contains a negative value, denoting the start of the time entry in seconds since epoch."
- Classic Toggl has **no scheduled/planned block entity** — only numeric estimates. In **Toggl 2.0**, planned calendar blocks and actual time entries are separate entities linked by an action: "Mark a scheduled block Done → logs time automatically." (https://community.toggl.com/t/toggl-2-0-onboarding-guide-for-toggl-plan-owners/3187)

### 2.3 Clockify

- Time entry (from the official OpenAPI sample):

```json
{
  "billable": true,
  "description": "This is a sample time entry description.",
  "end": "2021-01-01T00:00:00Z",
  "id": "64c777ddd3fcab07cfbb210c",
  "projectId": "25b687e29ae1f428e7ebe123",
  "start": "2020-01-01T00:00:00Z",
  "tagIds": ["..."]
}
```

  plus `timeInterval: { start, end, duration }`, `taskId`, `customFields` in other endpoints.
- The **Scheduling** feature (Pro/Enterprise) is a separate entity: "Fill out the required fields in the Create assignment window: Task, Period, Hours per day, Start time (optional), Billable/Non-billable, Note (optional), Repeat (None, Weekly, Every 2 weeks, Monthly, etc.)" (https://clockify.me/help/projects/manage-scheduled-team-assignments). The Entity Changes API exposes `SCHEDULED_ASSIGNMENT` as a distinct document type from `TIME_ENTRY`. The Assignments report exists "to compare assigned vs actual working hours."

### 2.4 Sunsama

> "There are two types of 'times' you can have entered on a task: planned time and actual time."
> "Planned time is used to set time estimates on your individual tasks."
> "Use actual time to record how long tasks actually take to complete."
> "Actual time: Calculated via the timer or manual updates."
> "Count planned time as actual time. When enabled, your planned time will be used as actual time when no actual time has been recorded."

(https://help.sunsama.com/docs/usage-guides/tasks/planned-and-actual-times/, https://help.sunsama.com/docs/settings/user-settings/)

Timeboxing creates calendar "working sessions" from planned time, but actual time is written back **onto the task itself** — mutation of the planned entity.

### 2.5 Motion

> "Completed At – Time spent before the task was closed. Duration – Planned length of time to complete the task." (https://www.usemotion.com/help/getting-started/navigation-basics/navigating-motion-features/navigating-project-management-features)

Motion distinguishes master recurring tasks from child instances: "Master Task … updates apply to all future child task instances." / "Child Task (recurring instance) … You can only edit status, duration, and deadline for that single instance." So even Motion, which mutates the task, has to materialize per-instance child records to hold the actual. (Flag: no public API schema for actuals found; official help pages only.)

### 2.6 Google Calendar Goals (deprecated November 2022)

> "Calendar will look at your schedule and find the best windows to pencil in time for that goal."
> "Calendar will automatically reschedule if you add another event that's a direct conflict with a goal."
> "Calendar actually gets better at scheduling the more you use it—just defer, edit or complete your goals like normal, and Calendar will choose even better times in the future."

(https://blog.google/products-and-platforms/products/workspace/find-time-goals-google-calendar/)

No separate actual-duration record existed. The "actual" was implicit in whether the event happened or was deferred — mutation of the planned event. (Flag: only the launch blog was reachable; no archived support page with a full data model.)

### 2.7 Timing / RescueTime (automatic trackers)

- **Timing** API time entry: `start_date`, `end_date`, `duration` (seconds), `project`, `title`, `notes`, `is_running`, `billing_status` (`undetermined`/`not_billable`/`billable`/`billed`/`paid`), `creator_id`, `custom_fields` (https://web.timingapp.com/docs/). No plan concept — actuals-only.
- **RescueTime** Analytic Data API returns `row_headers` + `rows`; the official docs describe the query envelope (perspective, restrict_kind, resolution_time) but not the concrete row fields. Integration code consuming the API reports row headers: `timestamp`, `duration` (seconds), `activity`, `category`, `productivity`, `document`, `people`, `details`, `source` (https://github.com/mlindgren/aw-rescuetime-import). Flag: exact row headers inferred from third-party integration code, not official docs.

### 2.8 Recommendation: separate entity survives recurrence better

**Use a separate entity for the actual, linked to the plan.** Reasoning:

1. **A recurring plan is a rule; an actual is always a concrete one-off.** If actuals mutate the planned entity, the first occurrence either destroys the template or forces a clone-per-occurrence. Clockify's `Scheduled Assignment` (repeatable weekly/monthly) vs. `Time Entry` instances is the clean separation exemplar.
2. **One plan generates many actuals without identity conflicts.** Motion had to introduce child task instances precisely because mutating the master can't hold per-occurrence actuals.
3. **History survives plan edits.** Editing a recurring plan's duration must not retroactively rewrite past actuals; with separate entities the plan row and the actual rows evolve independently.
4. **Products that do recurrence well separate the two.** Clockify (assignments vs entries), Toggl 2.0 (blocks vs entries). Google Goals — the mutation-based design with no durable actual record — was deprecated; causation is unprovable but its model retained no record of what actually happened.

Mutation (an `actual_time` field on the plan) is only acceptable for strictly non-recurring one-shot tasks with no plan-vs-actual comparison requirement.

### URLs opened (section 2)

- https://developers.track.toggl.com/docs/
- https://developers.track.toggl.com/docs/api/time_entries *(fetch failed — JS-rendered or moved)*
- https://github.com/toggl/toggl_api_docs/blob/master/chapters/time_entries.md
- https://engineering.toggl.com/docs/track/api/me/
- https://community.toggl.com/t/toggl-2-0-onboarding-guide-for-toggl-plan-owners/3187
- https://toggl.com/focus/
- https://docs.clockify.me/
- https://clockify.me/developers-api
- https://clockify.me/help/projects/manage-scheduled-team-assignments
- https://lobehub.com/tr/skills/openclaw-skills-clockify *(cross-reference for Clockify time-entry schema)*
- https://help.sunsama.com/
- https://help.sunsama.com/docs/usage-guides/tasks/planned-and-actual-times/
- https://help.sunsama.com/docs/usage-guides/timeboxing/timeboxing-concepts-and-principles/
- https://help.sunsama.com/docs/settings/user-settings/
- https://www.usemotion.com/help
- https://www.usemotion.com/help/project-management/task
- https://www.usemotion.com/help/getting-started/navigation-basics/navigating-motion-features/navigating-project-management-features
- https://blog.google/products-and-platforms/products/workspace/find-time-goals-google-calendar/
- https://web.timingapp.com/docs/
- https://timingapp.com/help/time-entries
- https://www.rescuetime.com/rtx/developers
- https://help.rescuetime.com/article/465-analytic-data-api-what-you-can-access
- https://github.com/mlindgren/aw-rescuetime-import *(cross-reference for RescueTime row headers)*

---

## 3. Timer semantics

### 3.1 How running timers are represented

**Toggl Track** (https://engineering.toggl.com/docs/track/api/time_entries/):

> "`duration`: Time entry duration. For running entries should be negative, preferable -1"
> "`stop`: Stop time in UTC, can be null if it's still running"
> `PATCH /v9/workspaces/{workspace_id}/time_entries/{time_entry_id}/stop` — "Stops a workspace time entry."

**Clockify** (https://docs.clockify.me/openapi.json): a running timer is a time entry whose `timeInterval.end` is absent (`end` is optional in `CreateTimeEntryRequest`/`UpdateTimeEntryRequest`). There is a `GET /v1/workspaces/{workspaceId}/time-entries/status/in-progress` endpoint ("Get all in progress time entries on a workspace"). The published spec defines a `StopTimeEntryRequest` (only an `end` time) but no dedicated `/stop` path — stopping is a PUT that sets `end`. (Flag: the spec does not explicitly mark `end` nullable; running = omitted `end`.)

**Harvest** (https://help.getharvest.com/api-v2/timesheets-api/timesheets/time-entries/):

> "`is_running`: Whether or not the time entry is currently running. `timer_started_at`: Date and time the running timer was started (if tracking by duration). ... Returns null for stopped timers."
> "`PATCH /v2/time_entries/{TIME_ENTRY_ID}/restart` — Restarting a time entry is only possible if it isn't currently running."
> "`PATCH /v2/time_entries/{TIME_ENTRY_ID}/stop` — Stopping a time entry is only possible if it's currently running."

### 3.2 Edge cases across products

| Edge case | Toggl Track | Clockify | Harvest |
|---|---|---|---|
| **Timer left running overnight** | Idle detection prompts; desktop can auto-stop on sleep/shutdown. No hard max duration documented. | Auto-stop on sleep/lock available; idle detection; **email alert at 8 h** ("Long-running timer"). | Idle detection (default 10 min alert); timers auto-stop when a timesheet lock takes effect. |
| **Timer started with no plan** | Allowed: `project_id`, `task_id`, `description` all optional. | Allowed: `projectId`, `taskId`, `description` optional. | **Not allowed**: `project_id` and `task_id` required. |
| **Plan completed with no timer** | Manual entry (start/stop or duration) added later. | Manual "Add & edit time". | Manual entries via duration or start/end. |
| **Pause vs resume** | No true pause. "Discard and continue" stops the old entry and starts a new one. | No true pause button; "continue" creates a new timer with the same details. Break mode starts a separate break entry. | Restart endpoint creates a new running entry. |
| **Editing a past entry** | `PUT` accepts `start`/`stop`. | `PUT` accepts `start`/`end`. | `PATCH` accepts `started_time`, `ended_time`, `hours`. |

Idle-detection option sets are remarkably consistent. Toggl desktop (https://support.toggl.com/toggl-track-desktop-app-for-macos):

> "On sleep: Do Nothing, Start Tracking Idle Time or Stop Running Time entry. On shutdown: Do Nothing or Stop Running Time entry."
> "Discard idle time ... Discard idle and continue ... Add idle time as new time entry ... Keep idle time."

Clockify (https://clockify.me/help/track-time-and-expenses/idle-detection-reminders):

> "If there's no mouse movement or keyboard strokes for X minutes, the timer will enter idle mode. ... Discard idle time / Discard and continue / Keep idle time."
> "Stop timer if screen locks / Stop timer if Mac goes to sleep / Stop timer if Mac powers off."

Harvest (https://support.getharvest.com/hc/en-us/articles/360048685231):

> "Stop timer and remove X minutes ... Continue timing and remove X minutes ... Add X minutes as a new entry ... Ignore."

And on overnight runaways, Clockify (https://clockify.me/help/administration/profile-settings): "Long-running timer alerts (triggered if a timer runs past 8 hours)". Harvest auto-stops running timers when a timesheet lock takes effect (https://support.getharvest.com/hc/en-us/articles/360048687491). Flags: Harvest's "Stopping a stuck or runaway timer" article returned HTTP 403 (quote taken from search snippet); no documented hard duration cap for Toggl or Harvest.

### 3.3 Enumerated state machine

None of the major products implement a true "pause" state — pause is always realized as **stop + optional new entry**. A timer attached to a planned block therefore needs a small set of discrete states, with elapsed time stored as a list of finalized **segments** rather than a single mutable interval.

**States**

- **Planned** — block on the schedule; no segments, no active timer.
- **Running** — exactly one open segment (`start` set, `stop` null) linked to the block.
- **Interrupted** — timer running but idle detected; awaiting a user decision (discard / discard-and-continue / keep / log-idle-separately).
- **InProgress** — one or more finalized segments exist, no timer running. This is the closest the model gets to "paused" (it is not a stored pause; it's just a block with logged segments).
- **Orphaned** — a running segment crossed a boundary it shouldn't have (overnight, device sleep with auto-stop policy, timesheet lock) and needs user resolution.
- **Completed** — block marked done; all segments finalized and editable-until-locked.
- **Abandoned** — running segment discarded; no time logged.

**Transitions** (grounding in parentheses)

1. `plan` → **Planned**.
2. `start`: Planned/InProgress → **Running**; create segment `{start: now, stop: null}` (Toggl negative duration / null stop; Clockify omitted end; Harvest `is_running`).
3. `idle_detected`: Running → **Interrupted**, after configurable threshold (all three products; Harvest default 10 min).
4. Resolve Interrupted:
   - `discard_idle` → stop segment at idle-start; → **InProgress** (Toggl/Clockify/Harvest "discard and stop").
   - `discard_and_continue` → stop old segment at idle-start, open a new Running segment with the same metadata (all three products' "discard and continue").
   - `keep_idle` → stay **Running** ("keep idle time" / "ignore").
   - `log_idle_separately` → stop work segment, create a separate idle segment (Toggl "add idle time as new time entry"; Harvest "add X minutes as a new entry").
5. `device_sleep_or_lock`: Running → finalize segment if the user's auto-stop policy is set, else → **Orphaned** (Toggl on-sleep options; Clockify stop-on-lock/sleep/power-off; Harvest stop-on-lock).
6. `long_running_alert`: Running stays Running; notify only, never auto-stop silently (Clockify 8-hour email).
7. `period_lock`: Running → finalize segment automatically (Harvest timesheet-lock behavior).
8. `stop`: Running → finalize segment (`stop = now`); block → **InProgress** or **Completed**.
9. `pause`: equivalent to `stop` — no stored paused segment; block → **InProgress** (no product has a true pause).
10. `resume`: InProgress → **Running**; open a **new** segment on the same block (Toggl/Clockify "continue", Harvest `restart`).
11. `manual_log`: Planned/InProgress/Completed → append a finalized segment with user-supplied start/end or duration (all products).
12. `complete_no_timer`: Planned → **Completed**; optionally log a manual segment after the fact.
13. `edit_segment`: mutate a finalized segment's `start`/`stop`/duration until locked (Toggl PUT start/stop; Clockify PUT start/end; Harvest PATCH started_time/ended_time/hours).
14. `abandon`: Running/Orphaned → **Abandoned**; segment discarded.

### URLs opened (section 3)

- https://developers.track.toggl.com/docs/api/time_entries *(failed to render)*
- https://engineering.toggl.com/docs/track/api/time_entries/
- https://support.toggl.com/en-us/article/how-to-get-everyone-to-track-time-1riia9a/
- https://support.toggl.com/toggl-track-desktop-app-for-macos
- https://support.toggl.com/en/articles/2206941-the-timeline-feature
- https://clockify.me/help/track-time-and-expenses/idle-detection-reminders
- https://clockify.me/help/track-time-and-expenses/breaks-web
- https://clockify.me/help/administration/profile-settings
- https://clockify.me/features/timer
- https://clockify.me/time-management-app
- https://docs.clockify.me/
- https://docs.clockify.me/openapi.json
- https://docs.clockify.me/openapi.yaml
- https://docs.clockify.me/swagger.json
- https://docs.developer.clockify.me/
- https://docs.developer.clockify.me/openapi.json
- https://api.clockify.me/api/v1
- https://help.getharvest.com/api-v2/timesheets-api/timesheets/time-entries/
- https://support.getharvest.com/hc/en-us/articles/360048685231-How-does-the-idle-timer-work-for-the-Mac-and-Windows-apps
- https://support.getharvest.com/hc/en-us/articles/47201092522509-Stopping-a-stuck-or-runaway-timer *(HTTP 403)*
- https://support.getharvest.com/hc/en-us/articles/360048687491-Unlocking-time-and-expenses

---

## 4. Recurrence expansion in Rust: the `rrule` crate (0.14) and alternatives

### 4.1 Baseline

Latest on docs.rs is **0.14.0** (released 2025-04-20): "A performant rust implementation of recurrence rules as defined in the iCalendar RFC. This crate provides `RRuleSet` for working with recurrence rules. It has a collection of `DTSTART`, `RRULE`s, `EXRULE`s, `RDATE`s and `EXDATE`s." (https://docs.rs/rrule/0.14.0/rrule/) MSRV 1.74, MIT/Apache-2.0, ~7.7K SLoC.

### 4.2 API shape

| Type | Purpose |
|------|---------|
| `RRuleSet` | Validated container for `DTSTART` + `RRULE`s + `EXRULE`s + `RDATE`s + `EXDATE`s; the main entry point |
| `RRule<Stage>` | One RFC 5545 rule; `Stage` is `Unvalidated` or `Validated` |
| `RRuleSetIter` / `RRuleIter` | Iterators over a set or single rule |
| `Frequency` | `Yearly, Monthly, Weekly, Daily, Hourly, Minutely, Secondly` |
| `Tz` | Wrapper enum: `Local(Local)` or `Tz(chrono_tz::Tz)` |
| `RRuleResult` | `{ dates: Vec<DateTime<Tz>>, limited: bool }` |
| `RRuleError` | `ParserError(ParseError)`, `ValidationError(ValidationError)`, `IterError(String)` |

Construction is by `FromStr` on RFC content lines or a fluent builder:

```rust
let rrule_set: RRuleSet = "DTSTART:20120201T023000Z\n\
    RRULE:FREQ=MONTHLY;COUNT=5\n\
    RDATE:20120701T023000Z,20120702T023000Z\n\
    EXDATE:20120601T023000Z".parse().unwrap();

let rrule = RRule::new(Frequency::Daily).count(40).interval(3);
```

Expansion:

- `pub fn all(mut self, limit: u16) -> RRuleResult` — "Limit must be set in order to prevent infinite loops. The max limit is 65535."
- `pub fn all_unchecked(self) -> Vec<DateTime<Tz>>` — no limits; README/SECURITY warn it can hang or exhaust memory.
- `IntoIterator` yielding `RRuleSetIter`; note **the raw iterator does not enforce limits** and (per open issue #137) ignores `after`/`before` bounds — only `all()`/`all_unchecked()` apply them.

Internal structure (from source): `RRuleSet { rrule: Vec<RRule>, rdate: Vec<DateTime<Tz>>, exrule: Vec<RRule>, exdate: Vec<DateTime<Tz>>, dt_start: DateTime<Tz>, ... }`; the set iterator merges rule iterators and RDATEs into a sorted stream, excluding EXRULE/EXDATE hits via timestamp lookup. Validation limits (README): year range −262000..=262000, max interval 65535 per frequency, internal `MAX_ITER_LOOP: u32 = 100_000`, `all()` caps returned dates at u16::MAX = 65535.

### 4.3 DST / timezone handling

- Public API uses its own `Tz` enum: "A wrapper around `chrono_tz::Tz` that is able to represent `Local` timezone also." (rrule/src/core/timezone.rs)
- "Note: All the generated recurrence will be in the same time zone as the `dt_start` property." (docs.rs)
- "Supported timezones are limited to by the timezones that Chrono-Tz supports. This is equivalent to the IANA database." (README)
- Changelog shows DST work over time: 0.9.1 "Fixes issue where iterations that passed a daylight saving time had incorrect hour"; 0.10.0 switched returned datetimes to `chrono::DateTime<rrule::Tz>`; 0.13.0 "Fix to respect timezone when serializing EXDATE and RDATE".
- **Open DST bugs remain:** issue #109 (CEST treated as invalid; iterator "stuck with the original offset of the timezone"; reporter suggests internally holding a single `NaiveDateTime` in UTC), issue #115 ("DST transition issues with zoned rrules" — duplicate entries when clocks go forward, missing entries when clocks go back, reproduced with Europe/London).

Verdict: timezone-aware expansion exists in principle via chrono-tz, but **DST transition correctness is a known, currently-open risk area**.

### 4.4 EXDATE / RDATE support

Yes, first-class: "`RRuleSet` allows for a combination for `RRule`s and some other properties. List of RDates ... (Union, A ∪ B). List of ExDate ... (Complement A \ B)" (README). Builder methods `rdate`, `exdate`, `set_rdates`, `set_exdates` exist. `EXRULE` is also supported but **behind the non-default `exrule` feature flag**, since RFC 5545 obsoleted EXRULE. Caveat: issue #146 — `EXDATE;VALUE=DATE:...` (date-valued exdates) is not supported; only DATE-TIME.

### 4.5 wasm32-unknown-unknown

**Not explicitly documented and not CI-tested.** No wasm mention in README, CHANGELOG, issues, or Cargo.toml. Dependencies (chrono 0.4.39, chrono-tz 0.10.1, regex with `perf,std`, thiserror 2, log) all compile to wasm32-unknown-unknown in general: chrono needs the `wasmbind` feature only for `Local::now()`/JS `Date` integration; chrono-tz is a compile-time IANA database with no OS deps. So **there is no dependency-level reason it cannot compile to wasm**, but this is unverified — do a test build before committing, and avoid `Tz::Local` in wasm without `chrono/wasmbind`. (Adjacent data point: the `rrule-rust` npm package wraps this crate via NAPI, not wasm.)

### 4.6 Known limitations (GitHub issue tracker)

- #148 — `NWeekday::Nth(1, Mon)` serializes to `MO` instead of `1MO`; nth-vs-every weekday becomes indistinguishable.
- #146 — `EXDATE;VALUE=DATE` unsupported.
- #139 — `FREQ=YEARLY;BYMONTHDAY=20` incorrectly yields monthly occurrences within the year.
- #137 — `IntoIterator` ignores `after`/`before` bounds on `RRuleSet`.
- #115 / #109 — DST transition bugs (see 4.3).
- #112 — `DTEND`/`DURATION` are not parsed (only DTSTART/RRULE/RDATE/EXDATE/EXRULE).
- #130 — no human-readable rule text output.
- #147 — discussion of chrono being "soft deprecated" in favor of `jiff`; no port exists.
- SECURITY.md: with limits disabled there are three DoS vectors (panics, CPU exhaustion, memory exhaustion); keep limits on and run untrusted expansion in a separate thread with a timeout.

**Maintenance status:** last release 0.14.0 on 2025-04-20; last commit to `main` the same day. As of 2026-09-07 that's **~17 months with no commits or releases**, while bugs filed 2024–2026 remain open. The crate is mature and widely used but appears to be in low-maintenance mode.

### 4.7 Alternatives

| Crate | Recurrence expansion? | Notes |
|---|---|---|
| **`calcard`** (Stalwart Labs) | **Yes** — "Recurrence rules expansion: Accurately computes and enumerates repeating events based on iCalendar and JSCalendar RRULEs" | Actively maintained (v0.3.13 released 13 days before check), iCalendar/JSCalendar/vCard/JSContact. **The most serious alternative.** |
| `icalendar` | No native engine; re-exports/uses `rrule` | Higher-level wrapper, not an alternative engine |
| `ical` (Peltoche/ical-rs) | No | Parser-only, ~1.48M downloads |
| `libical-sys` | Yes (via C libical FFI) | 4 GitHub stars, stale, pulls in a C dependency |
| `rrule-rust` | Yes (same engine, NAPI for Node) | Not a Rust crate, not wasm |

No other prominent pure-Rust RFC 5545 expansion crate exists (searches for `recurring`, `chrono-recurrence`, etc. returned nothing).

### 4.8 Verdict

`rrule` 0.14 is a capable, RFC-shaped expansion engine with clean API and safety limits, and it is likely (unverified) wasm-compatible. But DST-transition bugs, the yearly-BYMONTHDAY bug, and iterator-bound gaps are open, and maintenance has been quiet for ~17 months. If maintenance trajectory matters, **`calcard` is the credible actively-maintained alternative**. Either way, isolate recurrence expansion behind your own trait so the engine can be swapped, and never expand untrusted rules without limits.

### URLs opened (section 4)

- https://docs.rs/rrule/latest/rrule/
- https://docs.rs/rrule/0.14.0/rrule/
- https://docs.rs/rrule/0.14.0/rrule/struct.RRuleSet.html
- https://docs.rs/rrule/0.14.0/rrule/struct.RRule.html
- https://docs.rs/rrule/0.14.0/rrule/enum.Frequency.html
- https://docs.rs/rrule/0.14.0/rrule/struct.RRuleResult.html
- https://docs.rs/rrule/0.14.0/rrule/enum.RRuleError.html
- https://docs.rs/rrule/0.14.0/rrule/struct.RRuleSetIter.html
- https://docs.rs/rrule/0.14.0/rrule/enum.Tz.html
- https://github.com/fmeringdal/rust-rrule
- https://github.com/fmeringdal/rust-rrule/releases
- https://github.com/fmeringdal/rust-rrule/issues
- https://github.com/fmeringdal/rust-rrule/issues?q=is%3Aissue+is%3Aclosed
- https://github.com/fmeringdal/rust-rrule/commits/main/
- https://api.github.com/repos/fmeringdal/rust-rrule/commits?per_page=10
- https://api.github.com/repos/fmeringdal/rust-rrule/releases
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/Cargo.toml
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/rrule/Cargo.toml
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/CHANGELOG.md
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/SECURITY.md
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/rrule/src/lib.rs
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/rrule/src/core/timezone.rs
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/rrule/src/core/datetime.rs
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/rrule/src/core/rruleset.rs
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/rrule/src/core/rrule.rs
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/rrule/src/iter/mod.rs
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/rrule/src/iter/rruleset_iter.rs
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/rrule/src/iter/rrule_iter.rs
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/rrule/src/parser/content_line/mod.rs
- https://raw.githubusercontent.com/fmeringdal/rust-rrule/main/rrule/src/validator/mod.rs
- Issues: #148, #146, #139, #137, #130, #123, #115, #112, #109, #147, #135 (all under https://github.com/fmeringdal/rust-rrule/issues/)
- https://crates.io/crates/rrule
- https://crates.io/crates/calcard
- https://crates.io/crates/icalendar
- https://crates.io/crates/ical
- https://docs.rs/calcard/latest/calcard/
- https://docs.rs/icalendar/latest/icalendar/
- https://github.com/stalwartlabs/calcard
- https://github.com/Valodim/libical-sys
- https://github.com/lsndr/rrule-rust
- https://raw.githubusercontent.com/chronotope/chrono-tz/main/chrono-tz/Cargo.toml
- https://docs.rs/chrono-tz/latest/chrono_tz/enum.Tz.html

---

## 5. Time-block granularity: grid/slot model vs arbitrary start+duration

### 5.1 What real systems actually do

**Scheduling/availability APIs talk in slots but store intervals.**

- **Cronofy Availability API** (https://docs.cronofy.com/developers/api/scheduling/availability/) returns either `available_periods` (contiguous intervals) or fixed-length `available_slots`, with a caller-chosen `start_interval` of **5, 10, 15, 20, 30, or 60 minutes**. Their FAQ exposes the pitfall of an underspecified grid: "If a `start_interval` isn't specified, it will match the `required_duration` by default, meaning that 60 minute slots will begin on the hour" — so a 9:30–10:30 meeting can hide a viable 10:00 slot. Slots are **derived** from interval-shaped calendar data, not stored.
- **Google Calendar freebusy.query** returns intervals: "`busy[]`: List of time ranges ... `busy[].start`: The (inclusive) start ... `busy[].end`: The (exclusive) end" (https://developers.google.com/calendar/api/v3/reference/freebusy/query). Google events themselves are `start.dateTime`/`end.dateTime` (RFC 3339) — arbitrary intervals.
- Google **Appointment Slots** were retired 2024-08-07 in favor of **Appointment Schedules** with a configurable duration ("Appointments must be at least 5 minutes long") — again, slots generated from interval availability.
- **Nylas** converts intervals to slots on the fly with configurable rounding: "`round_to` snaps slot start times to a clean boundary; with `round_to` set to 15, a slot that would start at 9:05 rounds to 9:15." (https://developer.nylas.com/docs/cookbook/calendar/check-availability/)

**Meeting-poll tools are grid-native in the UI; storage is mostly undocumented.** When2meet, Crab Fit, etc. use a drag-select grid of date/time cells, but I found **no written prior art** describing how When2meet stores responses. The one exception is **OpenSlots** (https://github.com/tani/openslots), a decentralized when2meet alternative over Nostr that explicitly uses a fixed-slot bitmask:

> "1. Choose an epoch t0 (start time of the first slot). 2. Define n slots at resolution Δ. 3. Construct a binary string b ∈ {0,1}^n where b_i = 1 denotes 'available' for slot i."
> "The storage benefit is substantial: a bitmask requires n bits (plus encoding overhead), whereas timestamps require O(n) integers."

**Time-blocking apps snap in the UI, store events.** Ellie Planner documents a user-configurable snap increment ("Choose the Timebox snap increment from 5 minutes through 1 hour", https://guide.ellieplanner.com/features/settings-and-personalization). Akiflow, Sunsama, TickTick, Structured describe drag-and-drop time blocking with no published storage model or snap spec — **no written prior art found** that any of them store blocks as fixed slots. DayPilot (calendar UI component) treats snap-to-grid as a UI setting, confirming it's a presentation concern.

**Rostering/shift-planning:** no written prior art found showing mainstream rostering systems storing schedules as 5/10-minute slot bitmaps.

**RFC 5545** itself is interval-based: "The `DTSTART` property for a `VEVENT` specifies the inclusive start of the event. ... The `DTEND` property ... specifies the non-inclusive end of the event." (§3.6.1)

### 5.2 Written guidance on the tradeoff

The clearest engineering writeup is CodeLink's "Fixing the Availability Problem in Scaling Scheduling Systems" (https://www.codelink.io/blog/post/scaling-scheduling-systems):

> Approach 1 — "Some systems save time as DateTime type in the database. When calculating available slots, the system uses text comparison to find overlapping times. ... While this DateTime type-based approach can work in the beginning, it quickly becomes complex and hard to maintain as the system grows."
> Approach 2 — "We found a simpler way: convert time to numbers. We split the day into small 'blocks.' 1 block = 5 minutes (or X minutes, depending on our business logic). A full day = 24 hours × 60 ÷ 5 = 288 blocks."
> "In other words, availability is not stored; it is derived."

OpenSlots supplies the complementary storage argument (bitmask = n bits vs O(n) integers). Flag: I found **no dedicated article comparing interval trees vs bitmaps specifically for personal calendar storage** — that part of the landscape is thin.

### 5.3 Recommendation

For a personal time-blocking system with dense 5/10-minute blocks that syncs across devices with cheap edits: **use the fixed grid/slot (bitmap) model as the internal source of truth, and intervals only at the interoperability boundary.**

| Concern | Grid/slot (bitmap) | Interval (start+duration) |
|---|---|---|
| Merge/union/intersect | Bitwise OR/AND/NOT — deterministic, O(n/word) | Sort/sweep/interval-tree; harder to make deterministic across replicas |
| Sync / cheap edits | Each slot an independent bit; per-bit LWW or OR-set merges concurrent edits naturally; a whole day is one fixed-size payload | Concurrent edits to start/end of the same event are structural conflicts needing semantic resolution |
| Edit cost | Flip the bits covering the edited range | Cheap per row, but occupied/free overlays need recomputation |
| Dense 5-min data | Compact: one day = 288 bits at 5-min; Roaring-style compression shrinks further | Compact only when sparse; dense blocks mean many rows |
| Recurrence | Expand rules into daily bitmaps on read; keep the rule as the stored form | Native RRULE + exceptions |
| Interop | Must rasterize to/from DTSTART/DTEND | Native to every calendar API |

Concretely: (1) store your own blocks as per-day bitmaps at a chosen resolution, with metadata attached to contiguous runs of set bits (or keep block entities with slot-aligned start/duration — either way, the grid makes overlap arithmetic trivial); (2) sync at slot granularity (per-bit LWW is enough for single-user multi-device); (3) rasterize imported Google/Zoho events onto the same grid for display and conflict detection, but keep their native interval form for round-trip fidelity; (4) store recurrence as rules (per section 1's series/exception model) and expand to the grid on read.

Caveats: if arbitrary non-snapped boundaries (9:07–9:23) must be representable, the grid forces rounding — store exact intervals for ingested external events; for long horizons use compressed bitmaps or day-segmented storage; and never expose the grid as the external exchange format.

### URLs opened (section 5)

- https://docs.cronofy.com/developers/api/scheduling/availability/
- https://docs.cronofy.com/developers/faqs/availability/no-slots-available/
- https://www.codelink.io/blog/post/scaling-scheduling-systems
- https://developers.google.com/calendar/api/v3/reference/events
- https://developers.google.com/calendar/api/v3/reference/freebusy/query
- https://www.rfc-editor.org/rfc/rfc5545
- https://crab.fit/how-to
- https://github.com/ember-island-engineer/when2meetclone
- https://savvycal.com/articles/when2meet/
- https://www.usecarly.com/blog/how-to-use-when2meet/
- https://product.akiflow.com/help/articles/3677363-time-blocking-101
- https://www.sunsama.com/blog/time-blocking
- https://guide.ellieplanner.com/features/settings-and-personalization
- https://forums.daypilot.org/question/2083/snap-to-grid
- https://github.com/tani/openslots
- https://github.com/ErikBjare/timeslot
- https://developer.nylas.com/docs/cookbook/calendar/check-availability/
- https://canvasinfo.blogs.rice.edu/google-calendar-appointment-slots-replaced-by-appointment-schedules/
- https://support.stedwards.edu/TDClient/96/Portal/KB/PrintArticle?ID=1009
- https://www.lclark.edu/live/news/53720-google-appointment-slots-transition-to-schedules
- https://a-know-dev.medium.com/long-time-no-see-pixela-here-2bcac37895b7
- https://habitpocket.io/tools/year-in-pixels/
- https://koder.ai/blog/create-mobile-app-time-blocked-daily-planning
- https://senior-stack.uz/Roadmap/Data/datastructures-and-algorithms/18-bit-manipulation/03-bitmask-enumeration/middle/

---

## Cross-cutting synthesis

The five answers compose into one coherent data model:

1. **Plan series**: master entity with RRULE/EXDATE/RDATE (RFC 5545 semantics; Google's series-split instead of THISANDFUTURE) + per-instance exception entities keyed by original occurrence start.
2. **Actuals**: separate entities linked to (series, occurrence) — never fields mutated onto the plan (Sunsama/Motion's approach breaks on recurrence; Clockify/Toggl-2.0's separation survives it).
3. **Timer**: an open segment (start set, stop null) on an actual entity; pause = stop + new segment; idle/overnight handled by an Interrupted/Orphaned resolution flow, never silent auto-stop (Clockify's 8-hour alert is the industry ceiling).
4. **Expansion engine**: `rrule` 0.14 (with DST caveats and isolation behind a trait) or `calcard` if maintenance matters.
5. **Storage granularity**: fixed 5/10-minute slot grid internally for cheap sync-friendly edits; intervals at the external calendar boundary.
