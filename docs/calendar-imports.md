# Calendar import snapshots

Each successful refresh replaces one source's imported calendar. Recordings,
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
- `pending_import` records an upload that can be resumed. `retired_imports`
  records superseded batches whose irreversible cleanup needs to finish.

## Refresh and failure behavior

1. Retry cleanup of already-retired batches. If a pending batch exists, resume
   from its saved raw file; otherwise fetch a new response.
2. Parse and validate the entire response. Any skipped/unreadable event, duplicate
   UID, invalid feed, oversized event or oversized source manifest rejects the
   replacement. The previously active calendar stays active. An explicitly valid
   empty calendar is a valid replacement and removes the previous events.
3. Upload the raw file and publish the pending manifest. Upload/verify every event
   in the pending batch. Deterministic object IDs allow resuming accepted writes
   after a lost response; an existing event must match the complete expected data.
4. Publish one source revision that activates the complete batch, clears pending,
   and retires the previous batch. Revision conflicts fail rather than overwriting
   another device's source changes. Other devices can receive records out of order:
   they hide an incomplete active batch and show a warning until it is complete.
5. Tombstone and permanently purge the previous imported event objects and raw
   file. Only verified imported targets from that source/batch qualify. Cleanup
   is retried on the next refresh if it fails. There is no server transaction
   covering all objects; the active manifest provides visibility gating.

Refresh and source/file deletion are serialized locally with authentication
changes, so a batch cannot cross an account switch. Concurrent devices may resume
one pending batch; they cannot activate competing source revisions silently.
Even identical successful refreshes currently replace the previous batch. There
is no cross-source or same-source content deduplication policy.

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
in the recording. Previously decrypted history may remain cached on another device
until eviction/logout; purge does not erase another device's memory.

Standalone overrides retain their old base references. They are not automatically
reattached to a new imported batch. A resolution/reattachment UI remains deferred.
Deleting a raw file through Files also purges it when it is identified as an import
by a locally available source manifest; ordinary file deletion remains a tombstone.

## Remaining recovery limits

A pending import must finish before its source or raw file can be removed through
these commands. Cancellation/recovery UI for a permanently unfinishable pending
batch is still needed. The calendar's previous active batch remains usable while
an upload is pending.

A crash or ambiguous network failure between raw-file upload and publishing the
pending manifest can leave an unreferenced file in Files. It does not activate
any events. Automatic orphan-file cleanup is not implemented. Cleanup on a device
missing the raw file's accepted head waits for normal file sync rather than
inventing a revision anchor.

This format intentionally does not migrate abandoned development imports. Rebuild
clients and regenerate calendar sources when testing the cutover.
