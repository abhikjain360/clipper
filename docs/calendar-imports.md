# Calendar import snapshots

Each refresh reconciles one source within a rolling window: the past 14 days
through the next 90 days, measured from fetch completion in UTC. An event or
recurring series is in the window when at least one occurrence overlaps it.
The stored definition and the incoming definition both count when deciding
scope. Either one overlapping the window makes the event eligible. Provider
moves, extra dates and cancellations are included.
Zoned events use their own timezones; floating and all-day events use UTC for
this import boundary.

Within the window, new events are created, changed events get a revision, and
events removed from the feed get a tombstone. Unchanged events keep their IDs,
revisions and snapshot references. Events entirely outside the window are left
untouched as history, including when they change or disappear from the feed.
A meeting moved out of the window is updated so its old occurrence disappears.
An ended series replaces its previously unbounded rule. A cancelled provider
override removes a stored move into the window even when its original position
is outside it.

Recordings, locally authored schedules and locally authored occurrence overrides
are separate objects and are never deleted by refresh cleanup. Different sources
are independent; the same meeting or feed imported through two sources is
intentionally duplicated.

## Stored representation

- A staged refresh uploads the complete original UTF-8 ICS response once as an
  encrypted File (`calendar-import-<source>-<time>.ics`). It includes events
  outside the window, provider fields, timezone definitions and override
  components that the normalized model does not retain. It can be downloaded
  from Files. There is no plaintext server copy.
- Each parsed `IngestedEvent` carries its source, provider UID and a stable
  `import: ObjectId` identifying the source's first import. `raw_import`, when
  present, identifies the snapshot containing this event's actual definition.
  Stored `Recurrence::Imported` references name the actual raw snapshot too.
  Rule resolution uses `raw_import`, falling back to `import` for older events.
  Provider overrides use their original recurrence ID.
  New object IDs are derived from source and UID, independently of snapshots.
  Overrides stay bundled with their series and have stable IDs derived from the
  event and recurrence position. A returning tombstoned event reuses its ID.
- Existing whole-batch imports retain every storage ID. The first delta refresh
  records aliases from source/UID identity to those existing IDs in the encrypted
  source. It revises eligible events in place and keeps all outside-window history
  and unchanged events. Aliases remain after tombstones so returning events do
  not duplicate the old objects.
- The encrypted `delta.active` manifest records the newest completed refresh: raw File ID, fetch time,
  window, changed event IDs, UIDs, normalized hashes, and removed IDs.
  `delta.retained` owns unchanged events and history from earlier snapshots.
  Completed membership assigns each live event to one active or retained snapshot. Raw files
  remain while any live event still refers to them.
- The delta state is zlib-compressed JSON encoded as base64. Repeated membership
  lists, UUID aliases and hashes stay cheap while the top-level compatibility
  list remains readable by older clients. Readers also accept uncompressed
  delta state. Expanded delta data is limited to 2 MiB; the complete encrypted
  schedule record still has its 256 KiB limit.
- The top-level `active_import` is the compatibility view for older clients.
  Its ID stays fixed and its membership includes every live event, including
  retained history. Every event's `import` uses that same ID. Pending and retired delta groups stay inside `delta`, so an older
  reader cannot mistake them for whole-batch replacements. The anchor raw file
  is kept until explicit deletion or calendar removal.
- Older readers require imported-rule references to match that anchor. A changed
  or added unsupported series naming another snapshot fails their completeness
  check, so they hide that imported calendar and suppress its alarms. They cannot
  resolve it against an obsolete anchor rule. Updated readers validate the
  actual snapshot reference and display it normally. Eligible events with older
  anchor-only rule references are repaired on refresh even if the feed is unchanged.
- Completed refreshes record which snapshots they supersede. Cleanup requires
  this positive evidence. The evidence remains after cleanup so late uploads
  can be recognized. A source saved by an older client can lose the `delta`
  fields; missing evidence means keep events and raw files. Existing event
  records rebuild ID aliases on the next refresh. Explicit calendar removal
  records its own removal intent before purging.
- Full-feed and per-event hashes include normalized titles, descriptions, spans,
  recurrence, provider overrides, status, organizer, attendance, the owner's
  PARTSTAT and alarm offsets. Unsupported recurrence rules are included.
  Snapshot IDs and import ordering times are excluded, as are provider fetch
  timestamps such as DTSTAMP and LAST-MODIFIED. Event order and alarm order
  do not affect the comparison.
- Supported RRULEs become typed `Cadence` values only when conversion preserves
  every clause. An unsupported recurrence stores only its import and UID; the
  client resolves its rule from that snapshot's original file revision.
  Editing a raw file cannot reinterpret its events. A missing raw file never
  turns a recurring event into a one-off: the affected event is omitted with a
  warning. Eligible unsupported events with missing raw files are repaired from
  the next valid feed.
- Native clients cache complete encrypted raw files needed for rule resolution
  in SQLite, allowing offline expansion after hydration. Delta planning checks
  this cache before any snapshot request, verifies and decrypts the cached bytes,
  and downloads only missing or invalid ciphertext. The browser has a
  bounded in-memory rule cache and can need another download after reload.
- Parsed events store whether attendees exist and the owner's PARTSTAT.
  Other attendee addresses and replies remain in the raw file. Overrides store
  attendance only when they contain ATTENDEE lines; otherwise they inherit
  the master's attendance.
- `pending_imports` records resumable refreshes. `retired_imports` records
  superseded or unreferenced snapshots awaiting cleanup. A retired delta retains
  enough information to repair late writes without deleting shared event IDs.
  Events also carry their import's fetch time for per-object ordering.
- The encrypted source stores `owner_email`, `alarms_on` and optional
  `target_device`. These settings survive refreshes. An absent target means all
  phones. For Google calendar ICS URLs, the owner is the decoded first path
  segment after `/calendar/ical/`, when it contains an email address. Other
  feeds have an unknown owner. Alarms default to on.
- The source view exposes the active fetch time. Each device also saves successful
  fetch completion times locally. `checked_at` is the later of those times;
  “Last synced” and hourly refresh limits use it. Unchanged fetches advance this
  local time without server writes. Logout clears it. Times more than five
  minutes ahead of the current device's clock make a source due for refresh.

## Imported alarms

Display includes invitations regardless of RSVP. Ringing uses these rules:

1. Cancelled and all-day events never ring.
2. An event without attendees rings.
3. With a known owner, an event with attendees rings if the owner is the
   organizer or the owner's attendee PARTSTAT is ACCEPTED. DECLINED, TENTATIVE,
   NEEDS-ACTION, an omitted PARTSTAT or an absent owner attendee does not qualify.
4. With an unknown owner, an event with attendees rings unless STATUS is
   TENTATIVE or CANCELLED.

Each DISPLAY or AUDIO VALARM with a duration TRIGGER relative to DTSTART at or
before the start produces one alarm. Equal offsets are deduplicated; seconds
are preserved. Absolute, end-relative, positive and other-action triggers do
not qualify. Without a qualifying alarm, the lead is five minutes.

Moved occurrences ring relative to their new start, and cancelled occurrences
do not ring. A provider override can change attendance, status or alarms;
omitted fields inherit the master's values. Alarm labels use the event title.
Only complete active and retained imports ring. Turning off `alarms_on` silences the entire
source without hiding its events or clearing its target. An untargeted source
rings on all phones and stays silent on Macs. A targeted source rings only on
that device: phone plans exclude other devices' sources, and the desktop's
due-alarm selection includes sources targeted at that Mac. The event's existing
invitation and VALARM rules still apply. An unavailable target has no fallback.

The calendar list has a **Ring on** choice in the web/desktop and Android
schedule screens. It offers **All phones** and the registered devices.
Callers change the settings through `SyncEngine::set_calendar_source_alarms`
and `SyncEngine::set_calendar_source_target_device`; `next_alarms` combines
imported alarms with user-authored alarms. Local agents can use
`clipper calendar list` and `clipper calendar ring-on <source-id> <device-id|phones>`.

## Refresh and failure behavior

1. Read and authenticate the current source. Retry retired cleanup and saved
   in-window tombstones. Finish pending batches from their raw files, then fetch
   a new response even after recovery. Unreadable or inconsistent pending data
   is retired through verified cleanup. Network failures leave it for retry.
   Record fetch completion locally before parsing or uploading.
   Partially written events remain resumable when the previous raw file is
   missing. Authenticated records belonging to a pending or recorded retired
   batch provide the fallback definition. They must be claimed by the completed
   delta even when their content already matches the feed. Cleanup that cannot
   reconstruct the winning definition waits for a fresh feed instead of blocking
   it. If the pending raw file is also gone, the fresh feed repairs membership.
2. Validate the complete response before changing events. Duplicate UIDs,
   skipped or unreadable events and size-limit failures reject the refresh.
   An explicitly valid empty feed removes only held events that overlap the
   current window; outside-window history stays.
3. Compare eligible events with the held imports. Write only new or changed
   events; tombstone only missing UIDs whose stored occurrences overlap the
   window. Check typed recurrence and simple bounds before resolving raw rules.
   If a stored rule cannot be expanded because its raw snapshot is missing,
   conservatively treat it as eligible: rewrite it from the feed or tombstone
   it if absent. This also restores an unchanged unsupported rule after
   **Delete original feed**. An unchanged full feed with an available active raw file and no
   event delta writes nothing. Recompute eligibility on every fetch: events
   can enter the window even when the feed bytes do not change. A changed
   response containing only outside-window edits can replace the raw snapshot
   and source manifest without rewriting any event.
4. Upload the complete raw file and append the pending delta using the current
   authenticated source head. Preserve other pending batches and source settings
   when retrying conflicts. New deterministic IDs are created directly without
   an existence GET per event. Conflicts, updates and resumed writes authenticate
   the current object and retain revision and continuity checks.
5. Apply the delta and activate it through a source revision. A later fetch wins;
   equal fetch times are ordered by raw snapshot UUID. A fetch more than five
   minutes ahead cannot win. If another device activates a batch during upload,
   recompute the delta against that batch before retrying activation. This also
   handles devices that changed different events from the same older baseline.
   Per-event fetch ordering prevents older writers overwriting newer content.
6. Move unchanged events and history into one retained group per snapshot.
   Tombstone removed in-window events, preserving immutable revisions. Drop
   confirmed removals from the manifest; retained history carries no removal
   work. Retry unconfirmed removals after interruption. Purge a superseded
   raw file only with recorded supersession evidence and when no active,
   retained or pending group references it. Keep the compatibility anchor.
   Late writes from losing refreshes are restored to the winning content or
   tombstoned within its window. Outside-window events remain history.
   Retry unfinished cleanup on the next refresh.

There is no server transaction covering all objects. The source manifest
controls visibility: devices hide an incomplete imported view and show a warning
until its required records arrive. During a pending delta, membership also
accepts a written event whose snapshot matches that pending batch and whose
object ID is listed in it, when the pending batch is newer than the active
import by fetch time, then snapshot ID. An older pending batch has already
lost to the active import, so its writes are not shown. The changed revision appears immediately; the other
events and their alarms remain visible after interruption, before any retry.
Initial imports and whole-batch replacements still wait for activation.
Cleanup authenticates source ownership, object
identity, revisions and parent links. It never targets recordings, authored plans
or authored overrides. Concurrent cleanup removes only the retired entries it
finished and preserves entries added while it was running.

Source reads during a refresh still fetch current metadata and verify its
signature, revision and continuity. If that head matches this session's
in-memory source record's own revision and envelope body hash, and its verified
payload ciphertext is available, reuse the decrypted source payload.
Each source read performs this cache check once. It checks presence and recorded
ciphertext length and hash without reading or hashing the cached payload bytes.
Each new head is downloaded and authenticated once, then reused by later reads
in that refresh and later refreshes until the head changes. Source write
conflicts still reread current metadata; the cache never substitutes an older
source revision for the server's current one. Another browser tab can advance
shared localStorage without changing this tab's memory. Source reads then
download the current payload; writes derived from an older in-memory record
retain that record's own head so the server can reject a conflicting write.

A device can finish a batch after another device has superseded it and removed
its retired entry. The late uploader restores that entry before cleanup.
Hydrated events unclaimed by the current source are registered for cleanup only
when that source records their snapshot as superseded. Missing membership alone
does not authorize deletion. A cleanup retry
cannot overwrite content from a newer pending refresh.

Refresh and source/file deletion are serialized locally with authentication
changes. Concurrent devices can resume the same batch or stage independent
batches. UTC clock differences within the five-minute tolerance still affect
ordering. An unchanged check writes no shared ordering state, so a slowly
uploaded older feed can activate after another device's unchanged check until
the next refresh. See issues 136 and 139.

Pending batches must finish or retire before their source or raw files can be
removed. A crash or ambiguous failure between raw-file upload and pending
manifest publication can leave an unreferenced raw file. It activates no events;
automatic recovery of such files remains open in issue 92.

Raw feeds are limited to 8 MiB. Event records and source manifests, including
retained groups and legacy ID aliases, must fit the 256 KiB encrypted
schedule-record limit. This remains a bounded manifest.

## Deletion and historical references

- **Delete original feed:** purges the active raw File only. Stored one-off and
  typed-cadence events remain. Unsupported recurrence that needs this file is
  omitted with a warning. Other retained snapshots are unaffected.
- **Refresh calendar:** revises eligible changed events and tombstones removed
  eligible events. It preserves history, unchanged events and their raw files.
- **Remove calendar:** purges all of that source's active and retained imported
  objects and raw files, then tombstones the source. Other sources, recordings,
  local plans and local overrides remain.

Recordings pin exact plan revisions. Delta updates and tombstones keep these
revisions available; lookup never substitutes the latest event. Explicit calendar
removal purges imported objects, after which historical-plan lookup can report
unavailable. Captured recording times and planned bounds remain. Another device's
decrypted history can remain cached until eviction or logout.

Standalone overrides keep their exact base references. Stable event IDs do not
retarget revision pins. Files deletion also recognizes retained imports and
purges their raw files; ordinary file deletion writes a tombstone.
