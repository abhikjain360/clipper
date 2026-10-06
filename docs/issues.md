# Known issues and open decisions

Every known bug, security issue and undecided product question, one entry
each. An entry stays here until it has a decision: fix, accept, or drop. Fixed
entries keep their commit so the fix can be checked.

Each entry has:

- **Status:** open, fixing, fixed (with the commit), or decided.
- **Severity:** high, medium or low, judged by how likely it is and what it
  costs the user. "On main" means `main` had the same defect before this
  branch.
- **Decision:** what to do about it. "Fix (Claude)" means Claude chose to fix
  it without asking; reverse it if you disagree.

## Security

### 1. A malicious server can swap the revision a device accepted, through its own delete reply

- **Status:** fixing
- **Severity:** high (needs a malicious server). Not on main.
- **Where:** `crates/client/src/local_store.rs`, `apply_delete_inner`.
- **What happens:** when this device deletes an object, it waits for the
  server's reply. If an unsigned "deleted" event with the same sequence number
  arrived first, a shortcut upgrades the stored anchor to the device's own
  tombstone after comparing only revision numbers, not body hashes. A server
  that first delivered another device's competing revision 2 can replace that
  accepted revision 2 with the tombstone's, and then get revision 3 built on
  it accepted. The server cannot forge content, but it can choose which of the
  user's genuine versions this device follows.
- **Decision:** fix (Claude).

### 2. Two downloads running at once can accept two different bodies for one revision

- **Status:** fixing
- **Severity:** high (needs a malicious server). Not on main.
- **Where:** `crates/client/src/engine.rs`, `retain_downloaded_file`.
- **What happens:** when a download finishes, retention skips the anchor check
  if the device already holds that revision number or a newer one. It never
  compares hashes, so a second download of a different revision 2 is
  accepted, and an older revision is accepted after a concurrent advance.
- **Decision:** fix (Claude).

### 3. A download that finishes after logout still returns the old account's file

- **Status:** fixing
- **Severity:** high. Not on main (this return path is new).
- **Where:** `crates/client/src/engine.rs`, `retain_downloaded_file` and
  `download_file_bytes`.
- **What happens:** account A starts a large download, logs out, and B logs
  in. Retention notices the session changed and skips saving the record, but
  reports success, so the decrypted bytes reach the caller. On desktop they are
  written to the folder A chose.
- **Decision:** fix (Claude): fail with the session-ended error.

### 4. A clipboard push spanning a login uses two accounts' credentials

- **Status:** fixing
- **Severity:** medium. On main.
- **Where:** `crates/client/src/engine.rs`, `send_clipboard_payload`.
- **What happens:** the push reads A's data key, then after a logout and login
  signs with B's device key and sends B's token. B's account gets an object
  that only A's key opens.
- **Decision:** fix (Claude): read every credential at once and refuse to
  send if the session changed.

### 5. A screen update built for one account can be shown to the next

- **Status:** fixing
- **Severity:** low in practice. On main.
- **Where:** `crates/client/src/engine.rs`, `publish_visible_state`.
- **What happens:** a sync task builds the list of visible items for account
  A. If it stalls before showing it, across a logout and B's whole login, it
  shows A's items on B's screen until B's data finishes loading. Two reviewers
  reproduced it by holding the task by hand; ordinary scheduling does not
  stall a task for the length of a login.
- **Decision:** fix (Claude): tag each update with its session and drop
  updates from an ended one.

### 6. Logging in installs the new token before the old session is stopped

- **Status:** fixing
- **Severity:** low in practice. On main.
- **Where:** `crates/client/src/engine.rs`, `resume_with_platform`;
  `crates/client/src/api_client.rs`, `login_finish`.
- **What happens:** an in-flight sync from account A sends B's token and
  stores B's collab doc titles and share links in A's local store. It needs a
  login or resume while A is still signed in, which the web and mobile apps do
  not offer today.
- **Decision:** fix (Claude): stop the old session before installing the new
  token.

### 7. A file listed as clipboard is installed as a clipboard item

- **Status:** open (queued for the next client fix pass)
- **Severity:** low (needs a malicious server). On main.
- **Where:** `crates/client/src/engine.rs`, `snapshot_clipboard` and the other
  snapshot loops.
- **What happens:** the snapshot asks the server for clipboard items but never
  checks each item's kind. A server can list the user's `notes.txt` there; the
  file leaves Files and appears in clipboard history.
- **Decision:** fix (Claude): check the kind in every snapshot loop.

### 8. A crafted RRULE name still gets past the numeric check

- **Status:** fixing
- **Severity:** medium. Not on main.
- **Where:** `crates/schedule/src/ingest.rs`, `validate_rrule_numbers`.
- **What happens:** the check ends a property name at `;` or `:`, but the
  parser also ends it at `,` or `=`. `RRULE,X:FREQ=DAILY;INTERVAL=65538`
  imports as every 2 days, and `COUNT=4294967296` as a series that never ends.
- **Decision:** fix (Claude).

### 9. A feed of empty comma-separated values costs about 470 MB to parse

- **Status:** fixing
- **Severity:** medium. Not on main.
- **Where:** `crates/schedule/src/ingest.rs`, the pre-scan in
  `parse_calendar`.
- **What happens:** an 8 MiB feed of lines like `CATEGORIES:,,,,` allocates
  one value per comma. The import is accepted, so every client, including the
  browser, re-parses it.
- **Decision:** fix (Claude): cap the number of values before parsing.

### 10. A synced item with an extreme date crashes the calendar

- **Status:** fixing
- **Severity:** low (only a record signed by one of the user's own devices).
  Not on main.
- **Where:** `crates/schedule/src/time.rs`, `resolve_local`;
  `crates/schedule/src/recurrence.rs`, `until_scan_bound`.
- **What happens:** a date in year -262,143 or +262,142 makes expansion
  panic. On the web a panic kills the app, on every load.
- **Decision:** fix (Claude): refuse dates outside years 1 to 9999 and use
  checked arithmetic.

### 11. The Android alarm receiver logs the alarm's title in plaintext

- **Status:** decided
- **Severity:** low. Not on main.
- **Where:** `mobile/modules/clipper-alarm/.../AlarmReceiver.kt`, `onReceive`.
- **What happens:** each fired alarm writes its title to logcat. Logout
  cannot remove it; it stays until the log rotates or the phone reboots.
- **Decision:** accepted as a known issue for now.

## Bugs

### 12. Migration 5 broke collab docs on servers upgraded from main

- **Status:** fixed in `3b57f4c`
- **Severity:** high. Not on main.
- **What happened:** collab docs carried through the migration got their id
  as 36 bytes of text instead of a 16-byte UUID, so listing them failed and
  renaming or deleting them returned 404.
- **Decision:** fix (Claude).

### 13. The desktop app froze while a calendar synced

- **Status:** fixed in `6a3ec21`
- **Severity:** medium. The serial loop was on main; this branch added the
  first long command.
- **What happened:** the daemon handled one connection's requests one at a
  time, and the desktop app uses one connection, so stopping a timer waited
  for a sync of up to a minute or more.
- **Decision:** fix (Claude): each request runs on its own task, at most 8 at
  once per connection.

### 14. The desktop app could lose a reply from the daemon and wait forever

- **Status:** fixed in `6a3ec21`
- **Severity:** medium. On main.
- **What happened:** sending a command while a large state update was arriving
  threw away the part of the line already read. If the lost line was a reply,
  the command never returned.
- **Decision:** fix (Claude).

### 15. A state change made while no screen is waiting is lost

- **Status:** fixing
- **Severity:** medium. On main.
- **Where:** `crates/client/src/engine.rs`, `bump_version`.
- **What happens:** the change notice is dropped when nothing is listening at
  that moment, so the web or mobile screen stays stale until the next change.
- **Decision:** fix (Claude).

### 16. A lost reply to a timer write leaves the timer stuck or duplicated

- **Status:** open (queued for the next client fix pass)
- **Severity:** medium. Not on main.
- **Where:** `crates/client/src/engine.rs`, `start_actual`,
  `stop_actual_inner`, `write_schedule_record_for_session`.
- **What happens:** the server never echoes a device's own events, and the
  client does not re-read an object after a 409 or a dropped connection. If a
  stop's reply is lost, the timer shows as running and every start or stop
  fails with 409 until Refresh. If a start's reply is lost and the user
  retries, two timers run on the server and this device knows one.
- **Decision:** fix (Claude): after a 409 or a dropped reply on its own write,
  fetch that object's current head.

### 17. Each retry of a failed calendar sync uploads another copy of the feed

- **Status:** open (queued for the next client fix pass)
- **Severity:** low. Not on main.
- **Where:** `crates/client/src/calendar_import.rs`, `sync_calendar_source`.
- **What happens:** after a lost reply, each Sync uploads a new raw feed file
  (up to 8 MiB, shown in Files, charged to quota), then fails with 409.
  Nothing removes these files.
- **Decision:** fix (Claude): fixed by 16, plus removing the file uploaded for
  an attempt that got a 409.

### 18. One damaged anchor row stops cleanup for that kind on every pass

- **Status:** open (queued for the next client fix pass)
- **Severity:** medium. Not on main.
- **Where:** `crates/client/src/local_store/sqlite.rs`, `forget_object`.
- **What happens:** reads skip a damaged anchor, but cleanup refuses to
  forget a row that has one, and that error aborts the sweep for the whole
  kind. Objects of that kind deleted on other devices are never removed here.
- **Decision:** fix (Claude).

### 19. A listed file that cannot be decrypted is hidden by cleanup

- **Status:** open (queued for the next client fix pass)
- **Severity:** low. On main.
- **Where:** `crates/client/src/engine.rs`, `snapshot_files`.
- **What happens:** clipboard and schedule mark such an item as seen and keep
  the copy this device holds; files do not, so cleanup hides a readable older
  copy.
- **Decision:** fix (Claude).

### 20. Events after a second calendar block in a feed are dropped silently

- **Status:** fixing
- **Severity:** medium. Not on main.
- **Where:** `crates/schedule/src/ingest.rs`, `parse_calendar`.
- **What happens:** the parser stops at the first closing `END`, so a feed
  with two `VCALENDAR` blocks, or a stray `END:VEVENT`, imports only what came
  before it and reports nothing skipped.
- **Decision:** fix (Claude).

### 21. Two occurrences share one identity inside a daylight-saving gap

- **Status:** fixing
- **Severity:** low. Not on main.
- **Where:** `crates/schedule/src/engine.rs`, `rule_spans`.
- **What happens:** an imported hourly rule across a gap produces two
  occurrences at the same instant, so cancelling one cancels both.
- **Decision:** fix (Claude): keep the first, as RFC 5545 says.

### 22. Windows fixed-offset zone names are refused

- **Status:** fixing
- **Severity:** low. Not on main.
- **Where:** `crates/schedule/src/ingest.rs`, `feed_time_from_partial`.
- **What happens:** Outlook's `TZID=UTC-11` and similar exact ids are refused
  with the guessed "(UTC+05:30)" names, so the whole import is rejected.
- **Decision:** fix (Claude).

### 23. A revise that arrives after a purge and re-create skips revision numbers

- **Status:** fixing
- **Severity:** low (only the same account can cause it). Not on main.
- **Where:** `crates/server/src/routes/objects.rs`, `revise_object`,
  `advance_object_head`.
- **What happens:** the server checks the head outside the transaction, so a
  delayed revision 4 can land on a freshly re-created object whose head is 1.
- **Decision:** fix (Claude): the head may only move from n-1 to n.

### 24. The web grid gets one day wrong where daylight saving skips midnight

- **Status:** decided
- **Severity:** low. Not on main.
- **Where:** `web/src/schedule-layout.ts`, `dayBounds`;
  `web/src/calendar-view.ts`.
- **What happens:** in zones such as Santiago and Cairo, on that day the
  column runs from 01:00 to 01:00 the next day, so an early block shows on two
  days.
- **Decision:** accepted as a known issue for now.

## Product decisions

### 25. A changed occurrence without its series rejects the whole import

- **Status:** open
- **Where:** `crates/schedule/src/ingest.rs`, orphaned `RECURRENCE-ID`
  events.
- **What happens:** when a user is invited to one occurrence of someone else's
  series, the provider's feed holds that occurrence without its series. Today
  the import of that whole calendar is rejected.
- **Recommendation:** import it as a one-off event.
- **Decision:**

## Code and features

### 26. `UnparseableRule` carried its reason as a string

- **Status:** fixed in `a3d0ee4`
- **Decision:** fix (owner): one typed variant per reason.

### 27. `RecurrenceEngine` was a trait with one implementation

- **Status:** fixed in `d404d58`
- **Decision:** fix (owner): inherent methods on a `RecurrenceEngine` struct.

### 28. The web form cannot set "every N days, weeks or months"

- **Status:** fixing (built, Chrome QA pending)
- **Decision:** build (owner).

## Docs

### 29. Doc claims the code did not satisfy

- **Status:** fixed in `6e1059b` and `479ceaf`
- **What happened:** the resource-limits doc, the schedule model review, the
  Android alarm doc, the backlog and a migration comment each stated something
  the code does not do.
- **Decision:** fix (Claude).
