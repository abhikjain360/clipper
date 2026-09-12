# Scheduler outstanding work and decisions

Maintained entry point for “what is still left to do on the scheduler?” Last
updated 2026-09-12. This is a durable project backlog, not a temporary review
walkthrough. An unchecked item is outstanding, not authorization to implement
it or a requirement to finish it before merging this PR.

The owner's Rust review starts from
[scheduler-rust-review-guide.md](scheduler-rust-review-guide.md). Its Appendix B
lists the product decisions the 2026-09-12 pre-review surfaced; the ones that
touch scheduler behavior are repeated in the last section below.

Update this file when a decision is settled or work lands. Keep detailed design
in the linked documents and test evidence in [scheduler-review.md](scheduler-review.md).
If code and these notes disagree, verify the code and correct the notes.

## Decisions and missing workflows

- [ ] **Resolve overrides when a schedule changes.** Decide what choices to
      offer when timing or recurrence changes: discard overrides, explicitly map
      them to the new definition, or preserve them with an older series. These are
      alternatives to discuss, not agreed behavior. Build the resolution UI and
      corresponding validated write operation. Today the Rust client blocks such
      edits whenever standalone overrides exist. Cosmetic/alarm edits are allowed.
      If an incompatible definition arrives through sync, or the pinned base cannot
      be retrieved, the affected series is skipped with a warning, including its
      alarms. There is **no resolution UI**, despite the error saying “Resolve them”.
      See [plans across edits](scheduler-review.md#plans-across-edits-and-historical-recordings),
      `SyncEngine::update_schedule_item` in `crates/client/src/engine.rs`, and
      `effective_overrides` in `crates/client/src/schedule_context.rs`.
- [ ] **Edit this occurrence / this and future occurrences.** Single-occurrence
      editing has a domain model but no authoring UI. Define future-only edit
      semantics and series splitting; neither is implemented. Current expansion
      uses the latest definition, not a complete historical calendar.
- [ ] **Recorded-time workflows.** Design manual entry, correction, retroactive
      association with a plan, and viewing recorded time against its historical
      plan. These UI workflows are deferred. Decide which plan revision to select
      during retroactive association. Existing linked timers already pin the plan
      and applicable override at start; edits do not rewrite a running recording.
- [ ] **Cross-device timer conflicts.** Decide how users resolve simultaneous
      recordings started on different devices. Local commands are serialized;
      there is no global single-timer transaction.
- [ ] **Storage growth outside replaced imports.** Same-source refresh now
      purges the previous imported batch; recordings remain. The settled flow is in
      [calendar-imports.md](calendar-imports.md). Other immutable history is retained;
      native anchor expiry/pruning still needs an explicit security decision.
- [ ] **Import recovery UI.** Allow safe cancellation of a pending batch that
      cannot finish, and clean raw files orphaned before their manifest was saved.
      Pending batches currently resume on refresh. Overrides referencing a purged
      import need the deferred explicit reattachment workflow.
- [ ] **Connector-specific decisions.** Resolve cross-source duplicate primary
      bindings, whether RSVP writes are supported, and provider recurrence/override
      mapping before building connectors. See the
      [open sub-questions](schedule-plan.md#open-sub-questions). Embedded ICS provider
      overrides already share their imported object's revision.

## Deferred implementation

- [ ] Add the single-occurrence and recorded-time UI above after deciding their
      remaining behavior. Expose the existing historical-plan read in the UI.
- [ ] Add undo/history browsing UI; retained revisions are infrastructure, not
      an implemented undo feature.
- [ ] Expose richer recurrence controls supported by the domain/parser but
      missing from the composer. Preserving an existing recurrence does not provide
      controls for editing it.
- [ ] Build mobile schedule-management UI. Responsive browser QA does not cover
      the separate React Native app.
- [ ] Implement per-client source opt-in and automatic/background refresh.
      Current ICS refresh is manual and native. Android's seven-day alarm mirror
      eventually runs out unless refreshed.
- [ ] Implement Google/Zoho connectors after checking account/OAuth/private-feed
      availability. Outward publishing is explicitly outside this PR; older design
      notes describe future intent, not completed functionality.
- [ ] Consider remaining abnormalarm features: snooze, custom sounds and volume
      ramp, flashlight, skip-next, widget, configurable auto-silence, and missed-alarm
      notification. Scope and priority are not all settled. Current ringing stops
      after a fixed ten minutes.
- [ ] Add bounded native display queries and measure anchor/storage growth.
      SQLite removed the per-tombstone file cost; it did not bound lifetime growth
      or make hydration of all held objects inexpensive.

- [ ] A user action that itself gets a 401 (an upload, a delete) reports the
      error but does not end the session; only the snapshot and WebSocket
      paths do (`end_refused_session_for`, `end_refused_session_for_epoch` in
      `crates/client/src/engine.rs`). Route those through the same helper.
      Noted by the third-pass review on 2026-09-12.
- [ ] `hydrate_ciphertext_cache` in `crates/client/src/engine.rs` replaces the
      memory map without holding the store's `sync` lock. It runs inside
      `finish_auth` before the new session's WebSocket starts, so nothing
      races it today; make it take the lock so that stays true by
      construction.
- [ ] The web client drops the logout and validate response bodies unread, so
      Chrome logs `net::ERR_ABORTED` for both although the server processes
      them. Read the body (or use `keepalive`) to keep network logs clean.
- [ ] `LocalStore::discard_cached_payload` is a no-op on native (SQLite
      cascades the payload with the record) but the shared delete and absence
      paths in `local_store.rs` still call it before every marker write. Move
      the browser payload removal into the browser marker-write
      implementation and drop the call from the shared paths. Noted by the
      second-pass review on 2026-09-12; no behavior change.

## Reliability findings and outstanding verification

- [ ] **Immediate-post-boot Android ringing failure:** investigate and fix the
      media foreground-service refusal during boot-attributed starts. Test both
      locked and unlocked boot paths. This remains unresolved; a successful alarm
      several minutes after reboot does not cover it. See
      [alarm absorption](rn-alarm-absorption.md).
- [ ] Finish owner Rust review, then installed Android release-build QA, per the
      owner's chosen order. Web and responsive mobile-browser UI QA are accepted.
- [ ] Validate physical POCO/HyperOS overnight reliability over multiple nights,
      permission changes, reboot/unlock, and alarm-plan refresh. Emulator results
      do not establish vendor battery-management reliability.
- [ ] Restore a working Android lint run: the latest attempt crashed inside
      Worklets' Kotlin analysis. The alarm build and targeted emulator QA passed.
- [ ] Assess exotic sparse imported recurrence behavior: raw rules resolve from
      the saved ICS snapshot, while upstream iteration limits can produce incomplete
      results. Existing generated history/window bounds also intentionally reject
      overly dense expansion.

## Decisions surfaced by the pre-review (2026-09-12)

Each is described with its alternatives and a recommendation in
[scheduler-rust-review-guide.md](scheduler-rust-review-guide.md#appendix-b-decisions-for-the-owner).
The code does one defensible thing today; none of these blocks the review.

- [ ] A rescheduled override for a position the rule never generates adds an
      occurrence (the provider `RDATE` path). Keep, and validate identities in the
      future override-authoring UI, or restrict to imported events.
- [ ] A cancelled occurrence consumes a `COUNT` slot (RFC 5545). Keep or
      change; either way say so in the composer.
- [ ] An imported `DTSTART` that is not on `BYDAY` is not an occurrence, while
      Google Calendar includes it. Include it for imported events or keep the rule.
- [ ] `COUNT` together with `UNTIL` in an imported rule stays `Imported` instead
      of being rejected.
- [ ] The one-hour `created_at` window now applies to revisions; a device clock
      more than an hour behind cannot edit and gets no distinct error code.
- [ ] A 404 during live materialization drops the local copy of a held object
      until the next reconnect.
- [ ] Logout waits on an in-flight calendar sync (up to the 60 s fetch timeout).
- [ ] Collab writes carry no server ordering key, so a rename can revert locally
      until the next snapshot. Needs the server to return the committed `seq`.
- [ ] Any skipped event still rejects the whole import replacement.
- [ ] Deleting a file appends a tombstone and nothing purges it, so a full
      account cannot free space from the UI. Decide when the client purges.
- [ ] Security items of class C in
      [security-inventory-2026-09-12.md](security-inventory-2026-09-12.md).

## Existing boundaries, not promises of future fixes

Native revision anchors remember accepted history only while the local database
survives. Fresh installs cannot detect replay of an older valid history; skipped
revision chains and unsigned remote deletion events have documented limits.
Browser durable rollback protection is not promised. See
[object-envelopes.md](object-envelopes.md) and
[security-review.md](security-review.md) for the precise guarantees and remaining
security work. This scheduler list does not replace the project security review.

The broad object-kind registry and `AppState` redesign remain out of scope.
Historical decisions and implementation chronology live in
[schedule-plan.md](schedule-plan.md); its original “all resolved” statement only
applies to the initial D1–D11 decisions, not to this backlog.
