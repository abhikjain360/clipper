# Calendar import snapshots

Each changed refresh replaces one source's imported calendar. Recordings,
locally authored schedules and locally authored occurrence overrides are separate
objects and are never deleted by import cleanup. Different sources are independent;
the same meeting or feed imported through two sources is intentionally duplicated.

## Stored representation

- The complete original UTF-8 ICS response is uploaded once as an encrypted File
  object (`calendar-import-<source>-<time>.ics`). It includes provider fields,
  timezone definitions and override components that the normalized model does
  not retain. It can be downloaded from Files. There is no plaintext server copy.
- Each parsed `IngestedEvent` carries `import: ObjectId` plus its provider UID.
  Together they locate the original series in that exact raw snapshot; a provider
  override is further identified by its original recurrence ID. Fields not
  normalized remain in the snapshot, rather than being individually duplicated.
- Supported RRULEs become typed `Cadence` values only when conversion preserves
  every clause. An unsupported recurrence is persisted only as the import and
  provider UID; at runtime the client resolves its rule from that import's raw
  ICS file. This keeps the snapshot as the full original source without
  persisting a recurrence string. A missing raw file never falls back to a
  one-off: the affected event is omitted and a warning is shown. Imported
  events remain read-only even when their cadence is understood.
  The recurrence reference must match its event's import and UID. Snapshot
  resolution accepts only the original file revision, so editing a file cannot
  silently reinterpret the events that reference it.
- Native clients cache complete encrypted raw files needed for rule resolution
  in the local SQLite store, allowing offline expansion after hydration.
  The browser keeps only a bounded in-memory cache and may need to download the
  file again after a reload.
- `CalendarSource.active_import` contains the raw File ID, fetch time and parsed
  event object IDs. A new batch uses new storage IDs derived from the snapshot ID
  and UID. Domain event IDs remain source/UID-derived. Old revision references
  never retarget to a new batch merely because the UID matches.
- Each batch also contains a hash of the normalized events and unsupported
  recurrence rules. Snapshot IDs are replaced with a fixed ID when calculating
  this hash, and events and alarm offsets are sorted. Provider fetch
  timestamps such as DTSTAMP and LAST-MODIFIED are excluded. Titles, descriptions,
  spans, recurrence, provider overrides, status, organizer, attendance and the owner's PARTSTAT and
  alarm offsets are included.
- Parsed events store whether attendees exist and the owner's own PARTSTAT.
  Other attendee addresses and replies remain only in the raw file. Overrides
  store attendance only when they contain ATTENDEE lines; otherwise they use
  the master's attendance. Large attendee lists are not copied into moved instances.
- `pending_imports` records uploads that can be resumed. Concurrent devices can
  stage separate batches. `retired_imports` records the raw file ID and event
  IDs of superseded batches whose irreversible cleanup needs to finish.
- The encrypted source stores `owner_email` and `alarms_on`. For a
  `calendar.google.com/calendar/ical/` or `www.google.com/calendar/ical/` URL,
  the owner is the URL-decoded first
  path segment after `/calendar/ical/`, when it contains an email address. Other
  feeds have an unknown owner. The owner is derived on add and refresh. The
  source's alarm setting defaults to on and is shared across devices.
- The source view exposes `alarms_on` and the active batch's shared `fetched_at`.
  Each device persists its successful fetch completion times locally. The view's
  `checked_at` is the later of that local check time and the shared fetch time.
  “Last synced” labels and hourly refresh limits use `checked_at`. An unchanged
  fetch advances the local check time without writing anything to the server.
  Logout clears the device's local check times. Times more than five minutes
  ahead of the current device's clock make a source due for refresh.

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
Only complete active batches ring. Turning off `alarms_on` silences the entire
source without hiding its events. Native callers change this setting through
`SyncEngine::set_calendar_source_alarms`; `next_alarms` combines imported alarms
with user-authored alarms.

## Refresh and failure behavior

1. Read the current source head and retry cleanup of already-retired batches.
   Finish each saved pending batch from its raw file. A batch whose saved data
   cannot be reproduced is retired through verified cleanup. This includes a
   changed normalized hash, different staged events, unreadable or unparsable
   raw data, and records that exceed the size limit. Network errors, timeouts,
   server failures and lost replies stop the refresh and leave recovery for
   another attempt. Then fetch a new response,
   including for a manual Sync. A recovered batch never substitutes for the new
   response. Record the fetch completion time locally before parsing or uploading.
2. Parse and validate the entire response. Any skipped/unreadable event, duplicate
   UID, invalid feed, oversized event or oversized source manifest rejects the
   replacement, and the active batch stays active. An explicitly valid
   empty calendar is a valid replacement and removes the previous events.
3. Compare the normalized result with the active batch. If it matches and the
   active raw file is available, return an `IngestReport` with `feed_unchanged`
   true and `unchanged` equal to the event count. This also identifies an
   unchanged empty calendar. No raw file, event object or source revision is
   written for the unchanged response. Recovery and retired-batch cleanup are
   separate work and can still write. If the active raw file was deleted, a
   fresh snapshot restores it even when the events match.
4. Upload the raw file and append the batch to the pending manifest. Source
   conflicts are retried against the authenticated current head, retaining all
   other pending batches and source settings. Upload/verify every event
   in the pending batch. Deterministic object IDs allow resuming accepted writes
   after a lost response; an existing event must match the complete expected data.
5. Publish one source revision that removes this pending batch and compares its
   fetch time with the active batch. A later fetch activates and retires the
   previous batch. An earlier fetch is retired without replacing the active
   batch, and its report sets `superseded`. Equal fetch times are ordered by
   raw snapshot UUID, so devices make the same choice. Source conflicts cause
   another authenticated read and comparison; the existing source settings and
   other pending/retired batches are retained. Other devices can receive records out of order:
   they hide an incomplete active batch and show a warning until it is complete.
   A fetch time more than five minutes ahead of this device's clock cannot beat
   its fresh replacement. An unchanged response still writes nothing to the
   server, including when the active batch's fetch time is in the future.
6. Tombstone and permanently purge retired imported event objects and raw
   file. Only verified imported targets from that source/batch qualify. Cleanup
   is retried on the next refresh if it fails. There is no server transaction
   covering all objects; the active manifest controls visibility. Concurrent
   cleanup retries competing tombstones and removes only the batch entries it
   finished, retaining newer entries and event IDs added during cleanup.
   A missing retired payload is already purged. A signed tombstone identical
   to the retained anchor is already deleted. These outcomes complete cleanup;
   they do not weaken rollback, body identity or parent-link checks. Source
   reads overtaken by a newer authenticated head retry before updating the
   manifest.

A device uploading a batch can discover that another device has finished and
retired it. If the batch is still active, upload errors remain errors. If it
has been superseded, the uploader restores its retired entry before cleanup,
including when another device already removed that entry. Late event writes
are then purged through the same verified cleanup path. On refresh, hydrated
imported events whose batch is absent from the current source are registered
for cleanup. Their source and snapshot IDs survive in the encrypted events,
so a crash before restoring the retired entry does not lose those targets.

Refresh and source/file deletion are serialized locally with authentication
changes, so a batch cannot cross an account switch. Concurrent devices may resume
the same pending batch or stage independent batches. Fetch times use each
device's UTC clock; differences within the five-minute tolerance still affect
ordering. An unchanged check updates no shared ordering state. A slowly uploaded
older feed can therefore activate after another device's unchanged check until
the next refresh. Nothing deduplicates
events across sources. Duplicate master UIDs within one feed are rejected.

Pending batches must finish or be retired before their source or raw files can be removed.
An import cannot be cancelled. The previous active batch stays in use while the pending
one uploads. A crash or an ambiguous network failure between the raw-file upload
and publishing the pending manifest leaves a raw file that no manifest
references; it activates no events, and nothing removes it. Cleanup verifies the
original event's source, snapshot and UID without requiring the full event
format to remain readable. It uses authenticated current heads for tombstones,
while retaining revision checks. Recovery for unreferenced raw files is open; see
`docs/issues.md`, entry 92.

The raw feed is limited to 8 MiB. Parsed records and source manifests must fit the
existing 256 KiB encrypted schedule-record limit; they are checked before staging.
A very large feed can therefore be rejected even if its raw bytes fit. This is a
bounded manifest, not an unlimited calendar database.

## Deletion and historical references

The calendar UI explains consequences and asks for confirmation:

- **Delete original feed:** permanently purges only the active raw File. One-off
  events, events with a stored `Cadence`, and recordings remain. Events whose
  unsupported recurrence needs the raw file are omitted and the UI reports a
  warning; they are never treated as one-off events. The source and event
  references are unchanged.
- **Replace import:** permanently purges the previous batch's raw file and parsed
  event objects after the replacement is active.
- **Remove calendar:** purges that source's imported batches, then tombstones the
  source. Other sources and all recordings/local plans/local overrides remain.

A recording pins the exact imported plan revision it originally used. After that
plan is purged, historical-plan lookup reports unavailable instead of substituting
an event from a newer import. Captured recording times and planned bounds remain
in the recording. History that another device already decrypted can stay cached
there until eviction or logout; purge does not erase another device's memory.

Standalone overrides keep their old base references. They are not reattached to
a new imported batch, and there is no UI to reattach them.
Deleting a raw file through Files also purges it when a locally available source
manifest identifies it as an import; ordinary file deletion writes a tombstone.
