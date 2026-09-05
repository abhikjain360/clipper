# Scheduler outstanding work and decisions

Maintained entry point for “what is still left to do on the scheduler?” Last
updated 2026-09-10. This is a durable project backlog, not a temporary review
walkthrough. An unchecked item is outstanding, not authorization to implement
it or a requirement to finish it before merging this PR.

Update this file when a decision is settled or work lands. Keep detailed design
in the linked documents and test evidence in [scheduler-review.md](scheduler-review.md).
If code and these notes disagree, verify the code and correct the notes.

## Decisions and missing workflows

- [ ] **Resolve exceptions when a schedule changes.** Decide what choices to
  offer when timing or recurrence changes: discard exceptions, explicitly map
  them to the new definition, or preserve them with an older series. These are
  alternatives to discuss, not agreed behavior. Build the resolution UI and
  corresponding validated write operation. Today the Rust client blocks such
  edits whenever standalone overrides exist. Cosmetic/alarm edits are allowed.
  If an incompatible definition arrives through sync, or the pinned base cannot
  be retrieved, the affected series is skipped with a warning, including its
  alarms. There is **no resolution UI**, despite the error saying “Resolve them”.
  See [plans across edits](scheduler-review.md#plans-across-edits-and-historical-recordings),
  `SyncEngine::update_schedule_item` in `crates/client/src/engine.rs`, and
  `effective_exceptions` in `crates/client/src/schedule_context.rs`.
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
- [ ] **Ingest and storage growth.** Choose an ingest horizon and practical
  storage/retention limits for feed content and accumulated revisions. Retaining
  immutable history is already agreed; do not silently replace that decision
  with pruning. Native anchor retention/expiry is a separate security tradeoff
  that also needs an explicit decision before any pruning.
- [ ] **Connector-specific decisions.** Resolve cross-source duplicate primary
  bindings, whether RSVP writes are supported, and provider recurrence/exception
  mapping before building connectors. See the
  [open sub-questions](schedule-plan.md#open-sub-questions). Embedded ICS provider
  exceptions already share their imported object's revision.

## Deferred implementation

- [ ] Add the single-occurrence and recorded-time UI above after deciding their
  remaining behavior. Expose the existing historical-plan read in the UI.
- [ ] Add undo/history browsing UI; retained revisions are infrastructure, not
  an implemented undo feature.
- [ ] Expose richer recurrence controls supported by the domain/parser but
  missing from the composer. Align the shared TypeScript recurrence union with
  Rust's `Raw` variant; safe preservation of an existing rule is not full UI support.
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
- [ ] Assess exotic sparse raw recurrence behavior if supporting such rules:
  upstream iteration limits can produce incomplete results. Existing generated
  history/window bounds also intentionally reject overly dense expansion.

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
