# Rust review guide for the scheduler PR

How to review the Rust side of PR #1 (`schedule-module`) for logic and
business-logic bugs. Written 2026-09-12 after an AI pre-review pass; the
findings from that pass are listed in [Appendix A](#appendix-a-pre-review-findings-and-status)
with their status. Tick the boxes as you go. Update this file when a decision
in [Appendix B](#appendix-b-decisions-for-the-owner) is settled.

Companion documents:

- [scheduler-review.md](scheduler-review.md): QA evidence and the owner QA sequence.
- [schedule-model-review.md](schedule-model-review.md): the domain model, in prose.
- [object-envelopes.md](object-envelopes.md): the revision-chain contract.
- [calendar-imports.md](calendar-imports.md): the import snapshot contract.
- [scheduler-backlog.md](scheduler-backlog.md): outstanding work and open decisions.
- [rust-diff-map.md](rust-diff-map.md): every changed Rust function, generated from the diff.

## How to read the diff

The branch changes about 16,000 lines of Rust. Three kinds of file:

1. **New files.** Read them whole. All of `crates/schedule`, and on the client
   `calendar_import.rs`, `schedule.rs`, `schedule_context.rs`,
   `local_store/sqlite.rs`, `schedule_integration_tests.rs`.
2. **Large existing files with large diffs.** `crates/server/src/routes/objects.rs`
   (+2,300), `crates/client/src/engine.rs` (+1,600),
   `crates/client/src/local_store.rs` (+1,500). Do not read these whole. Use
   [rust-diff-map.md](rust-diff-map.md): it lists, per file, the functions whose
   bodies changed. Read those functions, in the order the stages below give.
3. **Small diffs in existing files.** `api-types`, `app-types`, `daemon-types`,
   `daemon/handler.rs`, `web-wasm`, `mobile-uniffi`, `core/crypto.rs`. Read the
   diff with `git diff main...HEAD -- <path>`.

About half of the diff is tests. They are listed separately in the diff map.
Read a test only when you doubt the code it covers.

Time budget, at a careful pace: stage 1 half a day, stage 2 one day, stage 3
one day, stage 4 half a day, stage 5 half a day, stage 6 two hours.

## Stage 1: the domain model (`crates/schedule`)

Pure code. No storage, no network, no crypto. Everything else builds on it, so
read it first. About 2,600 lines of source plus 2,900 lines of tests.

Order and what to check:

- [ ] `src/item.rs` (210 lines). The four types and their relationship.
  - `ScheduleItem`, `OccurrenceOverrideData`, `ActualRecord`, `Occurrence`.
  - `RecurrenceId`: floating identity is a wall-clock time, zoned is an
    instant, all-day is a date. This is what makes an override written in
    Berlin match the same occurrence expanded in Tokyo.
  - `PlannedRef` pins a storage revision (`ObjectRevisionRef`: object id,
    revision number, signed body hash) plus the observer zone and the resolved
    span. Ask: is anything a timer needs later missing from this pin?
  - `overrides_compatible_with`: title, reference and alarm edits keep
    overrides valid; span and recurrence edits do not. Ask: is every field
    that affects occurrence identity or timing in the second group?
- [ ] `src/time.rs` (250 lines). `TimedStart`, `ScheduleSpan`, `TimeRange`,
      `resolve_local`.
  - Policy: an ambiguous local time takes the earlier instant; a nonexistent
    local time shifts forward by the offset change; all-day spans are local
    calendar days; ranges are half-open and never empty.
  - `TimeRange` validates on deserialize (`try_from`). `BlockDuration` and
    all-day `days` are `NonZeroU32`. Ask: can any invalid value arrive
    through sync without hitting a validating constructor?
- [ ] `src/recurrence.rs` (500 lines) and `src/recurrence/imported_rule.rs`
      (300 lines).
  - `Recurrence` is `Once`, `Every(Cadence)`, or `Imported { import, uid }`.
    An imported rule that `Cadence` cannot express losslessly is not stored
    as text; it is re-read from the raw ICS snapshot at expansion time.
  - `ValidatedRrule::new`: rejects non-ASCII and control characters, then
    parses with the same DTSTART shape expansion uses. This is the only place
    provider rule text is validated. Read it slowly.
  - `imported_rule::convert`: the allow-list of clauses that become a
    `Cadence`. Ask: can a rule convert and change meaning? (Every clause the
    converter does not understand must keep the rule `Imported`.)
  - `MonthDay`, `NthWeekday`, `WeekdaySet`: validated on deserialize.
- [ ] `src/engine.rs` (550 lines). The expansion.
  - `rule_spans`: builds the DTSTART/RRULE text for the `rrule` crate on
    wall-clock time, skips candidates more than a day before the window
    without resolving them, then resolves the rest through `time.rs`. An
    `UNTIL` given as an instant is applied to the resolved instant; `rrule`
    only sees a loose wall-clock bound (`until_wall_clock`, `rrule_line`).
    Ask: where does each of the two limits (65,535 scanned, 10,000 in
    window) bite, and does exceeding either error rather than truncate?
  - `occurrences`: first loop applies cancellations and reschedules to rule
    positions inside the window; second loop adds rescheduled overrides whose
    rule position is outside the window. The second loop is also how provider
    `RDATE` additions appear, so an override for a position the rule never
    generates is an added occurrence by design. The caller must only pass
    overrides it trusts.
  - `overlapping_occurrences`: widens the window by the longest span so an
    overnight block that started yesterday still shows today.
  - `recurrence_id`: the identity must be the same for every observer.
- [ ] `src/alarm.rs` (95 lines of code). `plan_alarms`: no policy means no
      alarm; `fire_at = start - lead`; passed alarms are dropped; sorted.
- [ ] `src/ingest.rs` (900 lines). ICS parsing. Read with stage 5 instead if
      you prefer to keep imports together.
- [ ] `src/summary.rs`. Display strings only. Skim.

Tests worth reading alongside: `tests/behaviour.rs` (timezone travel,
cancellation, DST, overnight, limits), `tests/adversarial.rs` (DST gaps and
folds, short months, window edges, serde boundary), `tests/corpus.rs`
(comparison against the recurrence library).

Invariants to keep in mind for the whole stage:

- Expansion never truncates silently. A limit is an error.
- Floating and all-day series resolve in the observer's zone; zoned series
  resolve in their own zone.
- A cancelled occurrence still consumes a `COUNT` slot (RFC 5545 behavior).
- `UNTIL` is inclusive. An instant `UNTIL` cuts on the resolved instant; a
  DATE or floating `UNTIL` cuts on the wall clock.

## Stage 2: the revision chain (D6)

This is the security-sensitive part and it touches files and clipboard as
well as schedule. Read the contract first, then the crypto, then the server,
then the client acceptance in stage 3.

- [ ] `docs/object-envelopes.md`. The claims. Every later file must satisfy
      them.
- [ ] `crates/core/src/crypto.rs` diff (about 400 lines, half tests).
  - `object_envelope_body_bytes`, `object_envelope_parent_hash`: the parent
    hash is over the canonical signed body, excluding the signature.
  - `object_aad`: the exhaustive destructure decides which body fields the
    AEAD binds. Nonces, sizes and ciphertext hashes are excluded (they do not
    exist yet when the AAD is built) and are covered by the signature
    instead. The `object_aad` test module proves each bound field changes
    the AAD and each unbound field fails the signature.
  - Signing domains: envelope bodies and device login proofs are signed with
    different prefixes.
- [ ] `crates/api-types/src/lib.rs` diff. `ObjectEnvelopeBody` field order is
      the canonical byte order. `validate_revision_link` and the payload-count
      validators: revision 1 has no parent and is `create`; later revisions have a
      parent and are `revise` or `delete`; a `delete` has no payloads.
- [ ] `crates/server/src/routes/objects.rs`. Read in this order:
  1. `validate_object_envelope` and `validate_envelope_payload`: the
     cross-check between the authenticated request and the signed body. The
     security boundary.
  2. `init_object` (genesis), then `revise_object` and
     `head_revision_for_write`: a revision must name the exact current head
     (number and parent hash); at most one revision of an object is pending
     at a time; the `(object_id, revision)` primary key settles races.
  3. `object_for_upload`, `upload_payload`, `complete_object`,
     `reset_payload_status`: the streamed-upload state machine
     `pending -> uploading -> uploaded -> complete`. Ask: after every failure,
     is there a path back to `pending`?
  4. `advance_object_head`: the only writer of `head_revision`,
     `published_seq` and `deleted_at`. Monotonic. Called exactly once per
     head change, in the same transaction as exactly one `event_log` insert,
     after the transaction already holds the write lock (so `seq` order equals
     commit order).
  5. `get_object_revision`, `load_readable_revision`,
     `download_revision_payload`: historical reads. User-scoped, complete
     revisions only, work behind a tombstone.
  6. `purge_object`: locks the row, sums every revision's payload and
     metadata bytes, deletes the chain, releases quota, commits, then unlinks
     files. No event is emitted.
  7. `list_objects`, `object_list_items`, `ObjectListQuery`: pagination by
     `(published_seq, id)`; collab objects excluded.
- [ ] `crates/server/src/storage_quota.rs`. `revision_cost_bytes` is the one
      definition of what a revision costs (payload plus metadata ciphertext).
      `try_reserve_user_storage`, `charge_user_storage` and
      `release_user_storage` are the only counter writers. `charge_user_storage`
      is for tombstones only: their metadata is capped and charged without a
      limit check, because a tombstone is the only way to purge back under
      the limit. `revision_usage_by_user` gives bytes; `object_usage_by_user` gives
      bytes and count; the orphan sweep must take bytes from the first and the
      count from the second.
- [ ] `crates/server/src/cleanup.rs`. `cleanup_orphan_object_uploads` keys on
      server-stamped `stored_at`, re-applies its predicate on the delete, and
      removes the object row only when nothing was ever published. Ask: can it
      ever remove a completed revision's file?
- [ ] `crates/server/src/migration/m20260908_000005_object_revisions.rs` and
      `m20260908_000004_schedule_objects.rs`. The CHECK constraints restate the
      chain rules at the storage layer. `object_payloads` is keyed by
      `(object_id, revision, payload_id)`. `source_device_id` is
      `ON DELETE SET NULL`. `event_log` forbids `updated` for clipboard.
- [ ] `crates/server/src/state.rs` (`seed_event_seq`, `next_event_seq`),
      `ws.rs` (`get_latest_seq` includes tombstoned objects).

Questions to hold through stage 2:

- Can a client skip a revision number, replace a completed revision, or
  create two heads? (No path was found. Check the reasoning.)
- Is every byte the server retains charged to the quota, and released exactly
  once?
- Does every head change emit exactly one event?

## Stage 3: client acceptance and the local store

- [ ] `crates/client/src/local_store/sqlite.rs` (640 lines, new). Three tables:
      `objects` and `object_payloads` are a cache; `object_anchors` is the
      durable ledger of accepted revisions. `write_record` commits record,
      anchor and payload together. A `Deleted` write drops the object row and
      keeps only the anchor. `read_record` turns an anchor without an object row
      into a `Deleted` marker. Ask: can any path delete an anchor that has a
      chain position?
- [ ] `crates/client/src/local_store.rs`, these functions (use the diff map):
  - `validate_encrypted_revision_advance` and `validate_revision_against_head`:
    the rollback rules. Older revision: reject. Same revision, different
    body: reject. Immediate successor with wrong parent: reject. After an
    event-only delete, a live revision must be at least two steps newer.
  - `apply_delete_inner`: a locally signed tombstone becomes an exact anchor;
    a delete learned from the event stream keeps the previous head as an
    `ObservedDelete` anchor. An anchor must never get weaker.
  - `mark_record_absent`, `discard_unreadable_cache_entry`,
    `mark_object_absent_inner`: content can be dropped; the anchor stays.
  - `persist_collab_present_inner`: a server-visible collab listing must
    never replace an encrypted object's row.
  - `persist_*_present_encrypted_inner` (four near-copies): each takes the
    sync lock, checks the generation, runs the revision check, then the
    "later delete wins" guard, then writes.
  - `schedule_records_with_heads`: reads records and heads under one lock so a
    write derived from a record follows that record's head.
- [ ] `crates/client/src/engine.rs`, acceptance and sync functions:
  - `verify_object_list_item_envelope`: response and signed body must agree;
    signature verified when the device key is available; version checked.
  - `check_revision_advance`, `download_file_bytes`, `retain_downloaded_file`:
    a fetched head goes through the same anchor rules as a listed one and is
    retained. Retention is fenced on the session epoch under the key read
    lock, so a download that finishes after a logout and login is dropped
    rather than written into the next profile.
  - `end_refused_session_for`, `start_generation_for_session`, `ws_loop`,
    `ws_connect`: a 401, a `hello_ack` and the reconnect loop each name the
    generation or session epoch they belong to. A straggler from a replaced
    session is ignored; it never clears the new session or claims a
    generation in its profile.
  - `validate_snapshot_page`, `snapshot_files`, `snapshot_clipboard`,
    `snapshot_schedule`: pages must advance inside a fixed watermark; a
    stale page item is skipped, not fatal, and the sweep at the end still runs.
  - `handle_updated_object_event`, `materialize_object`,
    `handle_deleted_event`: live path. The server's `seq` decides ordering
    between events; the anchor decides what content is acceptable.
  - `finish_auth`, `logout`, `remove_device`: every session-scoped cache is
    cleared or epoch-keyed; the store generation is bumped so a straggler
    from the old session cannot write.
- [ ] `crates/client/src/api_client.rs` diff: `object_revise`,
      `get_object_revision`, `download_object_revision_payload`. Transport only.
      They verify nothing and advance nothing; callers do.
- [ ] `docs/local-store-plan.md`. The reasoning behind the store split.

Questions for stage 3:

- Does a successfully authenticated current read always leave the strongest
  anchor behind?
- Can unsigned information (a `seq`, a collab listing, a 404) discard or lower
  an authenticated anchor?
- After logout and login as another user on the same device, can anything of
  the first user remain visible or be written into the second user's store?

## Stage 4: schedule records and timers on the client

- [ ] `crates/client/src/schedule.rs` (420 lines, new). `ScheduleRecord` is
      the tagged enum inside the payload ciphertext; the server sees only kind
      `schedule`. `occurrence_key` / `parse_occurrence_key` are the only
      stringification of occurrence identity. `ingested_as_series` forces
      `alarm: None`, which is why imported calendars never ring.
- [ ] `crates/client/src/schedule_context.rs` (375 lines, new).
  - `schedule_revision`: an exact historical read. Checks the pin (id,
    revision, body hash), never installs a head. The 64-entry cache is keyed
    by session epoch.
  - `effective_overrides`: provider overrides come from the imported event;
    local overrides are checked for identity, duplicates and base
    compatibility. An incompatible base is an error, and the caller skips the
    series with a warning.
  - `validate_plan_context`: the stale-click defence for timers. The pin
    must equal the current local head; the occurrence must re-expand to the
    same identity and span.
  - `recorded_plan`: the historical detail read for a future UI.
- [ ] `crates/client/src/engine.rs`, schedule functions:
  - `write_schedule_record`, `write_tombstone`: every schedule write is one
    encrypted object with one inline payload, bounded at 256 KiB.
  - `start_actual`, `stop_actual_inner`: serialized by `actual_write`;
    validate twice around the history await; stop the running timer, then
    create; stop clamps the end to at least one second after the start.
  - `update_schedule_item`: the editor's expected revision must match; a
    structural edit is refused while local overrides exist; the series id
    must not change.
  - `expand_schedule`: one series failing to expand adds a warning and never
    blanks the calendar. `next_alarms`: the same, but without a UI warning.
  - `snapshot_schedule`, `decrypt_schedule_object_item`: reconciliation.
- [ ] `crates/client/src/schedule_integration_tests.rs` (1,350 lines). The
      live two-device regression. Read the test names to see what is covered;
      read bodies only for the flows you doubt.

Questions for stage 4:

- Can a stale occurrence view start a timer against a substituted plan?
- Can a running timer be lost by a failure between stop and create?
- Which edits are blocked while overrides exist, and is the error message
  honest about there being no resolution UI?

## Stage 5: calendar import

- [ ] `docs/calendar-imports.md`. The staged-batch contract.
- [ ] `crates/schedule/src/ingest.rs`: `parse_ics`,
      `partition_masters_and_overrides`, `event_from_component`, `span_from`,
      `recurrence_overrides`, `feed_time_from_partial`, `rrule_text`,
      `validate_rrule_numbers` (rejects any numeric clause calcard would
      narrow or rewrite). Limits: 8 MiB feed, 50,000 components, 500,000
      properties, 10,000 overrides per event. Any skipped event rejects the whole
      replacement; the previous calendar stays active.
- [ ] `crates/client/src/calendar_import.rs` (640 lines, new):
  - `sync_calendar_source`: fetch (or resume the pending raw file), parse,
    size-check every record with a probe id, upload the raw snapshot,
    publish `pending_import`, write every event with a deterministic id
    (`uuid v5(snapshot, uid)`), then publish one source revision that
    activates the batch and retires the previous one, then clean up.
  - `cleanup_calendar_imports`, `purge_import_object`: only verified members
    of a retired batch from this source are tombstoned and purged. Recordings,
    local plans and local overrides are never targets.
  - `recurrence_engine`: resolves an `Imported` rule from the raw file at
    revision 1 only, re-checks the head after decrypting, caches four
    engines per session epoch.
  - `ready_sources`: a source whose active batch is not fully present locally
    is hidden with a warning.

Walk the state machine with a crash after each step and with two devices
refreshing the same source. Ask: which batch is active, pending and retired,
and what does the other device show?

## Stage 6: plumbing across process boundaries

Mechanical, but a mismatch here is a runtime failure the Rust tests do not
see. The boundary check found three: a nullable field that was required on
input, wasm returning `undefined` where Tauri returns `null`, and a mobile
stub returning an empty success (Appendix A, P1 to P4).

- [ ] `crates/api-types/src/lib.rs`: request and response types for
      init/revise/complete/purge and the revision reads. Postcard field order is
      part of the canonical bytes; any reordering breaks every signature.
- [ ] `crates/app-types/src/lib.rs`: `ScheduleItemView`, `OccurrenceView`,
      `ActualView`, `AlarmView`, `CalendarSourceView`, `IngestReport`. Every
      `Option` here crosses as `null`, not as an omitted property.
- [ ] `crates/daemon-types/src/protocol.rs` and `crates/daemon/src/handler.rs`:
      the nine schedule commands. Each command has one result shape; void
      commands return an absent result.
- [ ] `crates/web-wasm/src/lib.rs`: the same commands for the browser, plus
      engine replacement after logout or a failed login. Check the
      `serde_wasm_bindgen` serializer setting that maps `None` to `null`, and the
      integer checks on `expected_revision` (JavaScript numbers stop being exact
      at 2^53; a revision that high is not reachable in practice).
- [ ] `crates/mobile-uniffi/src/lib.rs` and `packages/mobile-bridge/src/adapter.ts`:
      alarm plan export and session resume. A shared backend method the native
      API does not offer must reject, never return fabricated data.
- [ ] `packages/shared/src/types.ts`: the TypeScript mirror of the Rust
      types. Check optionality and enum tags against `app-types` and the
      schedule crate's serde attributes (`kind`/`at` for starts, `unit` for
      frequencies, `from`/`day` and `from`/`nth` for ordinals, lowercase weekday
      and month names).
- [ ] `web/src-tauri/src/lib.rs`: Rust snake_case arguments must match
      Tauri's camelCase payloads; temp staging of decrypted bytes is now private
      and randomly named.

## Outside the Rust review

`web/src` and `mobile/src` had a UI QA pass that is accepted. The Android
alarm module (`mobile/modules`, Kotlin) has one open reliability finding
(post-boot foreground-service refusal) listed in the backlog.

## Appendix A: pre-review findings and status

Found by the AI pre-review on 2026-09-12 (five independent reviewers over
different model families, plus adversarial test suites and a parser fuzzer),
then verified by hand against the code. "Fixed" names the commit on this
branch. "Documented" means the behavior was judged correct and the code or
docs now say so. "Decision" points at Appendix B.

### Revision chain and anchors

| #   | Finding                                                                                                                                                                                                                                            | Where                                                                                       | Status                                                                                                                                                                                                          |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A1  | A plaintext collab listing with an encrypted object's id replaced the row and deleted its anchor; a collab sweep then dropped the anchor row, so a replay of revision 1 passed.                                                                    | `local_store.rs` `persist_collab_present_inner`, `sqlite.rs` `write_record`/`forget_object` | Fixed in "Close three anchor rollback gaps in the client store"                                                                                                                                                 |
| A2  | A delayed response to this device's own tombstone replaced a newer anchor with the older tombstone head; only the unsigned `event_seq` was compared.                                                                                               | `local_store.rs` `apply_delete_inner`                                                       | Fixed in "Close three anchor rollback gaps in the client store"                                                                                                                                                 |
| A3  | `download_file_bytes` used the weaker `local_head` check (ignores `Absent`/`ObservedDelete` anchors), never compared the returned id to the requested one, and never retained the fetched revision, so revision 3 then revision 2 both downloaded. | `engine.rs` `download_file_bytes`, `check_revision_advance`                                 | Fixed in "Close three anchor rollback gaps in the client store"                                                                                                                                                 |
| A4  | Only payload bytes were charged to the storage quota; signed tombstones with 64 KiB of metadata each were free forever, and completed history is never swept.                                                                                      | `objects.rs` `init_object`/`revise_object`/`purge_object`, `storage_quota.rs`, `cleanup.rs` | Fixed in "Charge metadata ciphertext bytes to the storage quota"                                                                                                                                                |
| A5  | `docs/object-envelopes.md` described the payload AAD as the meta projection with a different final field; the code uses a separate domain label.                                                                                                   | docs                                                                                        | Fixed in "Domain-separate device signatures, authenticate the device identity record, pin OPAQUE stretching"                                                                                                    |
| A6  | A stale snapshot page item (older than a head that advanced while the page was in flight) errored, aborting the rest of the reconciliation pass and the sweep; deleted objects stayed visible until a later reconnect.                             | `engine.rs` snapshot loops, `local_store.rs` persist paths                                  | Fixed in "Keep reconciliation going past stale pages and fix timer stop, logout fencing and state ordering" and "Harden client request handling and remove duplicate helpers"                                   |
| A7  | Logout did not bump the store generation, so a straggler WebSocket handler could write a marker row for the old user's object into the new user's database.                                                                                        | `engine.rs` `logout`/`finish_auth`                                                          | Fixed in "Keep reconciliation going past stale pages and fix timer stop, logout fencing and state ordering" and "Harden client request handling and remove duplicate helpers"; completed by R1, R4 and R8 below |
| A8  | Two concurrent state publishes could leave the older view on screen until the next mutation.                                                                                                                                                       | `engine.rs` `publish_visible_state`                                                         | Fixed in "Keep reconciliation going past stale pages and fix timer stop, logout fencing and state ordering" and "Harden client request handling and remove duplicate helpers"                                   |
| A9  | Every state publish read and JSON-parsed every stored object from SQLite before checking its kind.                                                                                                                                                 | `local_store.rs` `schedule_items_inner`                                                     | Fixed in "Keep reconciliation going past stale pages and fix timer stop, logout fencing and state ordering" and "Harden client request handling and remove duplicate helpers"                                   |
| A10 | Collab writes carry the doc's creation time as their ordering key, so overlapping renames resolve by HTTP response order until the next snapshot.                                                                                                  | `engine.rs` `materialize_collab`                                                            | Decision B11                                                                                                                                                                                                    |
| A11 | A transient 404 during live materialization drops the local copy of a held object until the next reconnect.                                                                                                                                        | `engine.rs` `materialize_object`                                                            | Decision B7                                                                                                                                                                                                     |

### Server object routes

| #   | Finding                                                                                                                                                                                                | Where                                         | Status                                                                                                                                      |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| S1  | A `revise` on a clipboard object passed the handler and failed the `event_log` CHECK; on the streamed path the pending revision blocked every later write for up to two hours with its bytes reserved. | `objects.rs` `revise_object`                  | Fixed in "Reject clipboard revisions early and harden server cleanup and quota paths"                                                       |
| S2  | Reserving zero bytes and zero objects still ran the quota predicate, so a user above a lowered limit could not tombstone, so could not purge.                                                          | `storage_quota.rs` `try_reserve_user_storage` | Fixed in "Reject clipboard revisions early and harden server cleanup and quota paths"; that fix did not cover real tombstones, see R2 below |
| S3  | A DB failure after the payload file was renamed into place left the payload `uploading` with no reset.                                                                                                 | `objects.rs` `upload_payload`                 | Fixed in "Reject clipboard revisions early and harden server cleanup and quota paths"                                                       |
| S4  | The orphan sweep selected every orphan at once; past SQLite's bind limit it failed on every cycle.                                                                                                     | `cleanup.rs`                                  | Fixed in "Reject clipboard revisions early and harden server cleanup and quota paths"                                                       |
| S5  | Migration 5 rebuilt `event_log` without `idx_event_log_created_at`; hourly event cleanup scanned the table.                                                                                            | migration                                     | Fixed in "Reject clipboard revisions early and harden server cleanup and quota paths"                                                       |
| S6  | One user's failed quota release rolled back the whole sweep, forever.                                                                                                                                  | `cleanup.rs`                                  | Fixed in "Reject clipboard revisions early and harden server cleanup and quota paths"                                                       |
| S7  | The `created_at` acceptance window (one hour) now applies to revisions, so a device clock more than an hour behind cannot edit.                                                                        | `objects.rs` `validate_object_created_at`     | Decision B6                                                                                                                                 |

### Schedule domain

| #   | Finding                                                                                                                                                                                                                        | Where                                 | Status                                                                                                                                                                        |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | A series whose start does not exist in its zone (02:30 on spring-forward day; a date Samoa skipped) was rejected by the rrule crate, so it never expanded; floating and all-day series failed only for observers in that zone. | `engine.rs` `rule_spans`              | Fixed in "Expand recurrences on wall-clock time and resolve every instant through the time policy"; a series that started before a skipped date is R5 below                   |
| D2  | In zones whose DST gap starts at midnight (Havana, Santiago) an occurrence inside the gap was dropped, so every all-day recurring series lost a day.                                                                           | `engine.rs`                           | Fixed in "Expand recurrences on wall-clock time and resolve every instant through the time policy"                                                                            |
| D3  | A floating identity in a DST gap was taken after the shift, so Berlin saw `03:30` and everyone else `02:30`; overrides and timer pins did not match across zones.                                                              | `engine.rs` `recurrence_id`           | Fixed in "Expand recurrences on wall-clock time and resolve every instant through the time policy"                                                                            |
| D4  | An imported rule with a DATE or floating `UNTIL` (the normal all-day recurring event) validated at import and failed on every expansion.                                                                                       | `recurrence.rs` `ValidatedRrule::new` | Fixed in "Expand recurrences on wall-clock time and resolve every instant through the time policy"                                                                            |
| D5  | A non-ASCII `BYDAY` token panicked inside the rrule crate, reachable from a fetched feed.                                                                                                                                      | `recurrence.rs` `ValidatedRrule::new` | Fixed in "Expand recurrences on wall-clock time and resolve every instant through the time policy"                                                                            |
| D6  | `MonthDay` and `NthWeekday` accepted any `u8` on deserialize; `255` silently meant "last day".                                                                                                                                 | `recurrence.rs`                       | Fixed in "Reject rewritten import rules, cap overrides, validate day ordinals on deserialize"                                                                                 |
| D7  | Stopping a timer after the clock moved backwards errored instead of clamping, and no new timer could start.                                                                                                                    | `engine.rs` `stop_actual_inner`       | Fixed in "Keep reconciliation going past stale pages and fix timer stop, logout fencing and state ordering" and "Harden client request handling and remove duplicate helpers" |
| D8  | A rescheduled override for an identity the rule never generates adds an occurrence.                                                                                                                                            | `engine.rs` `occurrences`             | Documented: this is the `RDATE` path; tests renamed to say so. Decision B2                                                                                                    |
| D9  | A cancelled occurrence consumes a `COUNT` slot.                                                                                                                                                                                | `engine.rs`                           | Documented (RFC 5545). Decision B3                                                                                                                                            |
| D10 | An imported `DTSTART` that is not on `BYDAY` is not an occurrence; Google Calendar includes it.                                                                                                                                | `engine.rs`                           | Decision B4                                                                                                                                                                   |
| D11 | Two overrides with one identity resolve by slice order inside the engine.                                                                                                                                                      | `engine.rs`                           | Documented: the client rejects duplicate local overrides and provider overrides are deduplicated at ingest; the engine states the precondition.                               |

### Calendar ingest and import

| #   | Finding                                                                                                                                      | Where                                       | Status                                                                                                                                              |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| I1  | The calcard parser rewrote `INTERVAL=0`, `COUNT=0` and negative values into a valid daily cadence before the converter saw them.             | `ingest.rs` `rrule_text`                    | Fixed in "Reject rewritten import rules, cap overrides, validate day ordinals on deserialize"; values wider than the parser's integers are R6 below |
| I2  | `EXDATE`/`RDATE` values were uncapped; one 8 MiB line produced hundreds of thousands of overrides before the size check.                     | `ingest.rs` `recurrence_overrides`          | Fixed in "Reject rewritten import rules, cap overrides, validate day ordinals on deserialize"                                                       |
| I3  | `parse_ics` and `parse_imported_recurrence_rules` duplicated the master/override split.                                                      | `ingest.rs`                                 | Fixed in "Reject rewritten import rules, cap overrides, validate day ordinals on deserialize"                                                       |
| I4  | Component and property limits are checked after the parser allocated the whole tree; an 8 MiB feed of two-byte properties peaks near 570 MB. | `ingest.rs` `parse_calendar`                | Fixed in "Pre-scan feed size before parsing and tidy small leftovers" (line-count pre-scan)                                                         |
| I5  | `COUNT` and `UNTIL` together stay `Imported` and expand by the library's rule instead of being rejected.                                     | `imported_rule.rs`                          | Decision B5                                                                                                                                         |
| I6  | `VALUE=DATE` with a `TZID`, or a date value carrying a time, is accepted leniently as all-day.                                               | `ingest.rs`                                 | Documented leniency; not changed.                                                                                                                   |
| I7  | Resuming another device's pending batch needed the raw file in the local store before it had been materialized there.                        | `calendar_import.rs` `sync_calendar_source` | Fixed as a side effect of A3: a fetched file head is now retained locally.                                                                          |

### Process boundaries

| #   | Finding                                                                                                                                                                                           | Where                                   | Status                                                                                                  |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| P1  | `ScheduleItem.reference` is `Option` without `#[serde(default)]`, so omitting it on create/update fails to deserialize while the TypeScript type says it may be omitted.                          | `item.rs`                               | Fixed in "Pre-scan feed size before parsing and tidy small leftovers"                                   |
| P2  | `serde_wasm_bindgen::to_value` turns `None` into `undefined` while the Tauri path returns `null`; `share_url`, `source`, `raw_import_file_id` and the optional `AppState` fields differ by shell. | `web-wasm/src/lib.rs`                   | Fixed in "Pre-scan feed size before parsing and tidy small leftovers" (serializer maps missing to null) |
| P3  | Revisions are `u64` in Rust and `number` in TypeScript; a revision above 2^53 cannot round-trip.                                                                                                  | boundaries                              | Documented; not reachable in practice.                                                                  |
| P4  | The mobile adapter implemented `actualsBetween` as an empty success although the native API has no such operation.                                                                                | `packages/mobile-bridge/src/adapter.ts` | Fixed in "Refuse unsupported mobile actuals query and guard download cleanup" (rejects as unsupported)  |

### Local store robustness

| #   | Finding                                                                                                                                              | Where                         | Status                                                                    |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------- | ------------------------------------------------------------------------- |
| L1  | One malformed content row made hydration return an error, so every list was empty at unlock until reconciliation.                                    | `sqlite.rs` `live_records`    | Fixed in "Degrade corrupt local store rows instead of failing every read" |
| L2  | A live row with `NULL` content was a hard error on every read path, so that object could never be refetched, swept or persisted past.                | `sqlite.rs` `read_record`     | Fixed in "Degrade corrupt local store rows instead of failing every read" |
| L3  | One corrupt anchor row (for example a 1-byte parent hash) errored every later event for that object and tore the WebSocket down in a reconnect loop. | `sqlite.rs` `revision_anchor` | Fixed in "Degrade corrupt local store rows instead of failing every read" |

### Second pass: review of the polish commits

After the sixteen polish commits, a reviewer from a different model family
(GPT-6 Astra) read only those commits, checked every "Fixed" row above
against the code, and reported seven defects plus one partial fix. All are
fixed on the branch in the eight commits named below. It found no regression
in signature domains, AAD binding, server user scoping or the OPAQUE pinning.

| #   | Finding                                                                                                                                                                                                                                                                 | Where                                                                      | Status                                                                                |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| R1  | A download that finished after a logout and login wrote the previous account's file record and anchor into the new profile and published its filename into the new session's state.                                                                                     | `engine.rs` `download_file_bytes`, `retain_downloaded_file`                | Fixed in "Drop a file download that finished after its session was replaced"          |
| R2  | A tombstone reserves its 34 bytes of metadata ciphertext, so a user at or above the byte or object limit could not delete and so could not purge. The S2 test passed only because it sent empty metadata.                                                               | `objects.rs` `revise_object`, `storage_quota.rs`                           | Fixed in "Charge tombstones without a quota check so a full account can still delete" |
| R3  | A UTC `UNTIL` was compared on wall clock. Within an hour of a DST change an occurrence was wrongly dropped (fall-back) or kept (spring-forward).                                                                                                                        | `engine.rs` `rrule_line`, `rule_spans`; `recurrence.rs` `until_wall_clock` | Fixed in "Cut instant UNTIL rules on the resolved instant, not the wall clock"        |
| R4  | A delayed 401 from a replaced session's snapshot request signed the new session out, and did so outside the lock every other session change holds.                                                                                                                      | `engine.rs` `start_reconciliation`, `end_refused_session_for`              | Fixed in "Keep a newer session signed in when a refusal for a replaced one arrives"   |
| R5  | Every candidate from DTSTART was resolved before the window filter, so a series that started before a date the zone skipped failed on every later window.                                                                                                               | `engine.rs` `rule_spans`                                                   | Fixed in "Skip candidates before the window before resolving them"                    |
| R6  | The import pre-check missed values wider than calcard's integers (`INTERVAL=65536` became 1, `COUNT=4294967296` became endless) and did not cover the other numeric clauses.                                                                                            | `ingest.rs` `validate_rrule_numbers`                                       | Fixed in "Reject import rules with counts the parser would narrow"                    |
| R7  | The Tauri byte-upload command staged bytes under a name without the extension, so drag-and-drop uploads on desktop lost their filename and MIME type.                                                                                                                   | `web/src-tauri/src/lib.rs` `upload_file_bytes`                             | Fixed in "Stage desktop byte uploads under the user's filename"                       |
| R8  | A WebSocket authenticated as the previous account could claim a fresh generation after `hello_ack` and stream its events into the new profile; the previous login's reconnect loop also kept running and reconnected as the new user, so every event was handled twice. | `engine.rs` `ws_connect`, `ws_loop`, `start_generation_for_session`        | Fixed in "Stop a WebSocket and its reconnect loop when their session ends"            |

The reviewer also noted that the native `discard_cached_payload` is a no-op
that the shared delete paths still call. Left as is; it is listed in the
backlog as a simplification.

## Appendix B: decisions for the owner

None of these block the review. Each names what the code does today, the
alternative, and a recommendation.

- **B1. Override resolution after a structural edit.** Blocked today with an
  error that says "resolve them" though no UI exists. Already in the backlog.
- **B2. An override for a position the rule never generates adds an
  occurrence.** This is how provider `RDATE` additions work. A local override
  authored against a wrong identity would also add one. Recommendation: keep,
  and have the future override-authoring UI validate identities against rule
  positions before writing.
- **B3. A cancelled occurrence consumes a `COUNT` slot.** RFC 5545 behavior.
  Users often expect the opposite. Recommendation: keep; say so in the
  composer when `COUNT` is shown.
- **B4. Imported `DTSTART` off `BYDAY`.** The rrule crate drops it; Google and
  python-dateutil include it as the first occurrence. The event's own time
  summary names a day the calendar never renders. Recommendation: include the
  `DTSTART` instance for imported events, since that matches the provider that
  produced the feed.
- **B5. `COUNT` together with `UNTIL`.** Forbidden by RFC 5545; today the rule
  stays `Imported` and whichever clause the library honors wins.
  Recommendation: reject at ingest for consistency with the other strict
  checks.
- **B6. `created_at` window on revisions.** A device more than an hour behind
  the server cannot create or edit, with no distinct error code.
  Recommendation: keep the window (it bounds abuse of the orphan sweep) but
  return a distinct code so a client can re-sign with server time.
- **B7. A 404 during live materialization.** Today the local copy is dropped
  until the next reconnect. Recommendation: treat 404 as absence only for a
  `created` event; for an `updated` event keep the content and let the next
  snapshot decide.
- **B8. A snapshot page item older than the local head.** Now skipped with a
  warning while keeping the local head and refreshing its generation. The
  alternative (abort the pass) was the previous behavior and was the bug A6.
  Recommendation: keep.
- **B9. Logout waits on an in-flight calendar sync.** `calendar_write` is held
  across a 60 s fetch, so logout can take up to a minute. Recommendation:
  cancel the fetch on logout.
- **B10. Native anchors are retained forever.** Already in the backlog as a
  security decision.
- **B11. Collab writes have no server ordering key.** A rename can revert
  locally until the next snapshot. The fix needs the server to return the
  committed `seq` from the collab create/rename/delete routes.
  Recommendation: do it in a follow-up PR; it is outside the scheduler.
- **B12. Any skipped event rejects the whole import.** A single provider quirk
  (an all-day event whose `DTEND` equals `DTSTART`) rejects the refresh and
  keeps the old calendar. Recommendation: keep strict for now; revisit with
  real feeds.
- **B13. Security items that need a decision.** Listed with class `C` in
  [security-inventory-2026-09-12.md](security-inventory-2026-09-12.md). The
  ones this PR touches: anchor growth, orphaned raw import files, cancelling
  an unfinishable pending batch, schema changes recreating the database.
- **B14. `FLAG_SECURE` on Android.** Blocks screenshots of decrypted content
  and also the user's own screenshots. Not set today.
- **B15. Linux clipboard privacy markers fail open.** Left as is; the macOS
  watcher re-checks, the Linux one does not.
- **B16. Tauri capability allowlist.** Still `core:default`. Needs a run of
  the desktop app to confirm the minimal set.
