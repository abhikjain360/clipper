use clipper_schedule::ingest::ImportWindow;

use super::*;

struct HeldEvent {
    id: ObjectId,
    event: IngestedEvent,
    hash: String,
    owned: bool,
}

pub(in crate::engine) struct CalendarDelta {
    source: CalendarSource,
    events: Vec<(ObjectId, IngestedEvent, String)>,
    removed: Vec<ObjectId>,
    unchanged: u32,
    window: ImportWindow,
}

impl CalendarDelta {
    pub(super) fn is_empty(&self) -> bool {
        self.events.is_empty() && self.removed.is_empty()
    }

    pub(super) fn unchanged(&self) -> u32 {
        self.unchanged
    }
}

fn event_hash(event: &IngestedEvent, rule: Option<&str>) -> Result<String, ClientError> {
    let mut event = event.clone();
    event.import = Some(uuid::Uuid::nil().into());
    event.import_fetched_at = None;
    event.raw_import = None;
    if let clipper_schedule::Recurrence::Imported { import, .. } = &mut event.recurrence {
        *import = uuid::Uuid::nil().into();
    }
    let bytes = serde_json::to_vec(&(event, rule))
        .map_err(|error| ClientError::InvalidArgument(error.to_string()))?;
    Ok(crypto::sha256(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn set_import(event: &mut IngestedEvent, batch: &CalendarImport, anchor: ObjectId) {
    event.import = Some(anchor);
    event.raw_import = (anchor != batch.object_id).then_some(batch.object_id);
    event.import_fetched_at = Some(batch.fetched_at);
    if let clipper_schedule::Recurrence::Imported { import, .. } = &mut event.recurrence {
        *import = batch.object_id;
    }
}

fn remove_events(batch: &mut CalendarImport, removed: &HashSet<ObjectId>) {
    if batch.window.is_some() {
        let mut index = 0;
        batch.events.retain(|id| {
            let keep = !removed.contains(id);
            if !keep {
                batch.uids.remove(index);
                batch.hashes.remove(index);
            } else {
                index += 1;
            }
            keep
        });
    } else {
        batch.events.retain(|id| !removed.contains(id));
    }
}

fn activate_delta(source: &mut CalendarSource, batch: &CalendarImport) {
    let changed: HashSet<_> = batch.events.iter().chain(&batch.removed).copied().collect();
    if let Some(previous) = source.active_import.take() {
        source.retained_imports.push(previous);
    }
    for previous in &source.retained_imports {
        if previous.events.iter().any(|id| changed.contains(id)) {
            source
                .superseded
                .insert(previous.object_id, batch.object_id);
        }
    }
    for previous in &mut source.retained_imports {
        remove_events(previous, &changed);
        previous.removed.clear();
    }
    let mut retained = Vec::new();
    for previous in source.retained_imports.drain(..) {
        if previous.events.is_empty() {
            let mut retired = RetiredImport::from(previous);
            retired.superseded_by = Some(batch.object_id);
            source.superseded.insert(retired.object_id, batch.object_id);
            retired.events.clear();
            if let Some(delta) = &mut retired.delta {
                delta.events.clear();
                delta.uids.clear();
                delta.hashes.clear();
            }
            source.retired_imports.push(retired);
        } else {
            retained.push(previous);
        }
    }
    source.retained_imports = retained;
    source.active_import = Some(batch.clone());
    complete_retirements(source, batch.object_id);
}

pub(super) fn complete_retirements(source: &mut CalendarSource, winner: ObjectId) {
    for retired in &mut source.retired_imports {
        if source.pending_retirements.remove(&retired.object_id) {
            retired.superseded_by = Some(winner);
            source.superseded.insert(retired.object_id, winner);
        }
    }
}

pub(super) fn retire_import(source: &mut CalendarSource, batch: CalendarImport) {
    source.delta_state = true;
    let mut retired = RetiredImport::from(batch);
    retired.superseded_by = source.active_import.as_ref().map(|active| active.object_id);
    if let Some(winner) = retired.superseded_by {
        source.superseded.insert(retired.object_id, winner);
    } else {
        source.pending_retirements.insert(retired.object_id);
    }
    source.retired_imports.push(retired);
}

fn loses(batch: &CalendarImport, source: &CalendarSource) -> bool {
    let latest = chrono::Utc::now() + chrono::TimeDelta::minutes(5);
    batch.fetched_at > latest
        || source.active_import.as_ref().is_some_and(|active| {
            active.object_id != batch.object_id
                && active.fetched_at <= latest
                && (active.fetched_at, uuid::Uuid::from(active.object_id))
                    >= (batch.fetched_at, uuid::Uuid::from(batch.object_id))
        })
}

impl SyncEngine {
    async fn event_in_scope(
        &self,
        event: &IngestedEvent,
        window: &ImportWindow,
    ) -> Result<bool, ClientError> {
        if let Some(overlaps) = event
            .window_check(window)
            .map_err(|error| ClientError::InvalidArgument(error.to_string()))?
        {
            return Ok(overlaps);
        }
        match self.recurrence_engine(&event.resolved_recurrence()).await {
            Ok(engine) => event
                .overlaps(window, &engine)
                .map_err(|error| ClientError::InvalidArgument(error.to_string())),
            Err(error) => {
                warn!(uid = %event.uid, %error, "Calendar recurrence unavailable; reconciling the event conservatively");
                Ok(true)
            }
        }
    }
    #[cfg(test)]
    pub(in crate::engine) async fn sync_calendar_source_in_window(
        &self,
        id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<IngestReport, ClientError> {
        self.run_work(
            None,
            self.sync_calendar_source_inner(id, Some(ImportWindow::around(now))),
        )
        .await
    }

    #[cfg(test)]
    pub(in crate::engine) async fn stage_windowed_calendar_import(
        &self,
        id: &str,
        text: &str,
        fetched_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<CalendarImport, ClientError> {
        let (source, _) = self.read_calendar_source(id).await?;
        let outcome = validated_feed(text, &source)?;
        let delta = self
            .plan_calendar_delta(source, &outcome, &ImportWindow::around(fetched_at))
            .await?;
        self.stage_calendar_delta(id, text, fetched_at, delta).await
    }
    async fn import_outcome(
        &self,
        raw: ObjectId,
        source: &CalendarSource,
    ) -> Result<Option<clipper_schedule::IngestOutcome>, ClientError> {
        let Some(bytes) = self.read_import_snapshot(raw).await? else {
            return Ok(None);
        };
        let text = String::from_utf8(bytes).map_err(|_| CalendarImportError::InvalidText)?;
        Ok(Some(validated_feed(&text, source)?))
    }

    async fn imported_events(
        &self,
        source: &CalendarSource,
    ) -> Result<HashMap<String, HeldEvent>, ClientError> {
        let records: HashMap<_, _> = self
            .local_store
            .schedule_records_with_ids()
            .await
            .into_iter()
            .collect();
        let mut snapshots = HashMap::new();
        let mut held = HashMap::new();
        for batch in source.imports() {
            if batch.window.is_some()
                && (batch.events.len() != batch.uids.len()
                    || batch.events.len() != batch.hashes.len())
            {
                return Err(CalendarImportError::FeedChanged.into());
            }
            for (index, id) in batch.events.iter().enumerate() {
                let current = match records
                    .get(&id.to_string())
                    .and_then(ScheduleRecord::as_ingested)
                {
                    Some(event) => Some(event.clone()),
                    None => self
                        .load_import_event(&id.to_string())
                        .await?
                        .and_then(|record| record.as_ingested().cloned()),
                }
                .filter(|event| {
                    event.source == source.id
                        && event.has_valid_recurrence()
                        && (batch.window.is_none() || batch.uids[index] == event.uid)
                });
                let partial = current.as_ref().is_some_and(|event| {
                    event.snapshot().is_some_and(|raw| {
                        source.superseded.contains_key(&raw)
                            || source
                                .pending_imports
                                .iter()
                                .chain(
                                    source
                                        .retired_imports
                                        .iter()
                                        .filter_map(|retired| retired.delta.as_deref()),
                                )
                                .any(|pending| {
                                    pending.object_id == raw && pending.events.contains(id)
                                })
                    }) && event.snapshot() != Some(batch.object_id)
                });
                let owned = current.as_ref().is_some_and(|event| {
                    event.snapshot() == Some(batch.object_id)
                        || (batch.window.is_none()
                            && event.import == Some(batch.object_id)
                            && !partial)
                });
                let mut stored = current.as_ref().filter(|_| owned).cloned();
                let mut raw = stored
                    .as_ref()
                    .and_then(IngestedEvent::snapshot)
                    .unwrap_or(batch.object_id);
                if (stored.is_none()
                    || stored.as_ref().is_some_and(|event| {
                        matches!(
                            event.recurrence,
                            clipper_schedule::Recurrence::Imported { .. }
                        )
                    }))
                    && !snapshots.contains_key(&raw)
                {
                    snapshots.insert(raw, self.import_outcome(raw, source).await?);
                }
                if stored.is_none()
                    && snapshots.get(&raw).is_none_or(|parsed| parsed.is_none())
                    && partial
                {
                    stored = current;
                    raw = stored
                        .as_ref()
                        .and_then(IngestedEvent::snapshot)
                        .unwrap_or(raw);
                    if stored.as_ref().is_some_and(|event| {
                        matches!(
                            event.recurrence,
                            clipper_schedule::Recurrence::Imported { .. }
                        )
                    }) && !snapshots.contains_key(&raw)
                    {
                        snapshots.insert(raw, self.import_outcome(raw, source).await?);
                    }
                }
                let parsed = snapshots.get(&raw).and_then(Option::as_ref);
                if batch.window.is_some()
                    && raw == batch.object_id
                    && let Some(parsed) = parsed
                    && !batch.content_hash.is_empty()
                    && feed_hash(parsed)? != batch.content_hash
                {
                    return Err(CalendarImportError::FeedChanged.into());
                }
                let event = if let Some(event) = stored {
                    event
                } else {
                    let mut event = parsed
                        .and_then(|outcome| {
                            outcome.events.iter().find(|event| {
                                if batch.window.is_some() {
                                    event.uid == batch.uids[index]
                                } else {
                                    ObjectId::from(uuid::Uuid::new_v5(
                                        &uuid::Uuid::from(batch.object_id),
                                        event.uid.as_bytes(),
                                    )) == *id
                                        || ObjectId::from(event.id) == *id
                                        || source.event_ids.get(&event.id) == Some(id)
                                }
                            })
                        })
                        .cloned()
                        .ok_or(CalendarImportError::EventChanged)?;
                    set_import(
                        &mut event,
                        batch,
                        source.import_anchor.unwrap_or(batch.object_id),
                    );
                    event
                };
                let rule = parsed.and_then(|outcome| {
                    outcome
                        .rules
                        .iter()
                        .find(|(uid, _)| *uid == event.uid)
                        .map(|(_, rule)| rule.as_str())
                });
                let hash = if rule.is_none()
                    && matches!(
                        event.recurrence,
                        clipper_schedule::Recurrence::Imported { .. }
                    ) {
                    String::new()
                } else {
                    event_hash(&event, rule)?
                };
                if held
                    .insert(
                        event.uid.clone(),
                        HeldEvent {
                            id: *id,
                            event,
                            hash,
                            owned,
                        },
                    )
                    .is_some()
                {
                    return Err(CalendarImportError::EventChanged.into());
                }
            }
        }
        Ok(held)
    }

    pub(super) async fn plan_calendar_delta(
        &self,
        mut source: CalendarSource,
        outcome: &clipper_schedule::IngestOutcome,
        window: &ImportWindow,
    ) -> Result<CalendarDelta, ClientError> {
        let held = self.imported_events(&source).await?;
        for event in held.values() {
            if event.id != ObjectId::from(event.event.id) {
                source.event_ids.insert(event.event.id, event.id);
            }
        }
        let mut rules = clipper_schedule::engine::ImportedRuleResolver::new();
        for (uid, rule) in &outcome.rules {
            rules
                .insert(uuid::Uuid::nil().into(), uid, rule)
                .map_err(|error| ClientError::InvalidArgument(error.to_string()))?;
        }
        let engine = RecurrenceEngine::with_imported_rules(rules);
        let mut events = Vec::new();
        let mut unchanged = 0;
        let uids: HashSet<_> = outcome
            .events
            .iter()
            .map(|event| event.uid.as_str())
            .collect();
        for event in &outcome.events {
            let old = held.get(&event.uid);
            let incoming = event
                .overlaps(window, &engine)
                .map_err(|error| ClientError::InvalidArgument(error.to_string()))?;
            let stored = if incoming {
                false
            } else if let Some(old) = old {
                if !old.owned {
                    true
                } else {
                    self.event_in_scope(&old.event, window).await?
                }
            } else {
                false
            };
            if !incoming && !stored {
                continue;
            }
            let rule = outcome
                .rules
                .iter()
                .find(|(uid, _)| *uid == event.uid)
                .map(|(_, rule)| rule.as_str());
            let hash = event_hash(event, rule)?;
            let raw_available = match old
                .filter(|old| {
                    matches!(
                        old.event.recurrence,
                        clipper_schedule::Recurrence::Imported { .. }
                    )
                })
                .and_then(|old| old.event.snapshot())
            {
                Some(import) => self
                    .local_store
                    .import_file_object(&import.to_string())
                    .await?
                    .is_some(),
                _ => true,
            };
            let rule_current = old.is_none_or(|held| match &held.event.recurrence {
                clipper_schedule::Recurrence::Imported { import, .. } => {
                    Some(*import) == held.event.snapshot()
                }
                _ => true,
            });
            if raw_available
                && rule_current
                && old.is_some_and(|held| held.owned && held.hash == hash)
            {
                unchanged += 1;
                continue;
            }
            let id = source
                .event_ids
                .get(&event.id)
                .copied()
                .unwrap_or_else(|| event.id.into());
            events.push((id, event.clone(), hash));
        }
        let mut removed = Vec::new();
        for event in held.values() {
            if uids.contains(event.event.uid.as_str()) {
                continue;
            }
            if self.event_in_scope(&event.event, window).await? {
                removed.push(event.id);
            }
        }
        events.sort_by(|left, right| left.1.uid.cmp(&right.1.uid));
        removed.sort_by_key(|id| uuid::Uuid::from(*id));
        Ok(CalendarDelta {
            source,
            events,
            removed,
            unchanged,
            window: window.clone(),
        })
    }

    pub(in crate::engine) async fn stage_calendar_delta(
        &self,
        id: &str,
        text: &str,
        fetched_at: chrono::DateTime<chrono::Utc>,
        delta: CalendarDelta,
    ) -> Result<CalendarImport, ClientError> {
        let outcome = validated_feed(text, &delta.source)?;
        let mut batch = CalendarImport {
            object_id: uuid::Uuid::nil().into(),
            fetched_at,
            events: delta.events.iter().map(|(id, _, _)| *id).collect(),
            content_hash: feed_hash(&outcome)?,
            window: Some(delta.window),
            uids: delta
                .events
                .iter()
                .map(|(_, event, _)| event.uid.clone())
                .collect(),
            hashes: delta
                .events
                .iter()
                .map(|(_, _, hash)| hash.clone())
                .collect(),
            removed: delta.removed,
        };
        for (_, event, _) in &delta.events {
            let mut event = event.clone();
            set_import(
                &mut event,
                &batch,
                delta
                    .source
                    .import_anchor
                    .or_else(|| {
                        delta
                            .source
                            .active_import
                            .as_ref()
                            .map(|batch| batch.object_id)
                    })
                    .unwrap_or(batch.object_id),
            );
            check_record_size(&ScheduleRecord::Ingested(Box::new(event)))?;
        }
        let mut probe = delta.source.clone();
        probe.delta_state = true;
        probe.import_anchor = probe
            .import_anchor
            .or_else(|| probe.active_import.as_ref().map(|batch| batch.object_id))
            .or(Some(batch.object_id));
        probe.pending_imports.push(batch.clone());
        check_record_size(&ScheduleRecord::Source(Box::new(probe.clone())))?;
        probe.pending_imports.pop();
        activate_delta(&mut probe, &batch);
        check_record_size(&ScheduleRecord::Source(Box::new(probe)))?;
        batch.object_id = self
            .upload_file_bytes(
                &format!(
                    "calendar-import-{}-{}.ics",
                    delta.source.id,
                    fetched_at.timestamp_millis()
                ),
                Some("text/calendar"),
                text.as_bytes(),
            )
            .await?
            .parse()
            .map_err(|source| ClientError::InvalidId {
                kind: "import id",
                source,
            })?;
        loop {
            let (mut source, head) = self.read_calendar_source(id).await?;
            source.delta_state = true;
            source.import_anchor.get_or_insert(batch.object_id);
            let SourceKind::Ics { url } = &source.kind;
            source.owner_email = url::Url::parse(url).ok().and_then(|url| owner_email(&url));
            source.event_ids.extend(delta.source.event_ids.clone());
            source.pending_imports.push(batch.clone());
            match self.save_calendar_source(id, &source, head).await {
                Ok(_) => return Ok(batch),
                Err(ClientError::Api { status: 409, .. }) => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) async fn finish_calendar_delta(
        &self,
        id: &str,
        text: &str,
        staged: &CalendarImport,
    ) -> Result<IngestReport, ClientError> {
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let mut batch = staged.clone();
        let mut report = IngestReport::default();
        loop {
            if self.history_epoch.load(Ordering::SeqCst) != epoch {
                return Err(ClientError::NotAuthenticated);
            }
            let (mut source, head) = self.read_calendar_source(id).await?;
            let Some(pending) = source
                .pending_imports
                .iter()
                .find(|pending| pending.object_id == batch.object_id)
            else {
                report.superseded = source
                    .active_import
                    .as_ref()
                    .is_none_or(|active| active.object_id != batch.object_id);
                if !source.delta_state {
                    break;
                }
                if report.superseded
                    && !source
                        .retired_imports
                        .iter()
                        .any(|retired| retired.object_id == batch.object_id)
                {
                    retire_import(&mut source, batch.clone());
                    match self.save_calendar_source(id, &source, head).await {
                        Ok(_) => {}
                        Err(ClientError::Api { status: 409, .. }) => continue,
                        Err(error) => return Err(error),
                    }
                }
                break;
            };
            if pending.content_hash != staged.content_hash
                || pending.fetched_at != staged.fetched_at
                || pending.window != staged.window
            {
                return Err(CalendarImportError::FeedChanged.into());
            }
            batch = pending.clone();
            let outcome = validated_feed(text, &source)?;
            if feed_hash(&outcome)? != batch.content_hash
                || batch.events.len() != batch.uids.len()
                || batch.events.len() != batch.hashes.len()
            {
                return Err(CalendarImportError::FeedChanged.into());
            }
            report.superseded = loses(&batch, &source);
            for (index, id) in batch.events.iter().enumerate() {
                let event = outcome
                    .events
                    .iter()
                    .find(|event| event.uid == batch.uids[index])
                    .ok_or(CalendarImportError::FeedChanged)?;
                let rule = outcome
                    .rules
                    .iter()
                    .find(|(uid, _)| *uid == event.uid)
                    .map(|(_, rule)| rule.as_str());
                if event_hash(event, rule)? != batch.hashes[index]
                    || source
                        .event_ids
                        .get(&event.id)
                        .copied()
                        .unwrap_or_else(|| event.id.into())
                        != *id
                {
                    return Err(CalendarImportError::EventChanged.into());
                }
            }
            let basis = source.active_import.as_ref().map(|active| active.object_id);
            if !report.superseded {
                let delta = self
                    .plan_calendar_delta(
                        source.clone(),
                        &outcome,
                        batch
                            .window
                            .as_ref()
                            .ok_or(CalendarImportError::FeedChanged)?,
                    )
                    .await?;
                let mut rebased = batch.clone();
                rebased.events = delta.events.iter().map(|(id, _, _)| *id).collect();
                rebased.uids = delta
                    .events
                    .iter()
                    .map(|(_, event, _)| event.uid.clone())
                    .collect();
                rebased.hashes = delta
                    .events
                    .iter()
                    .map(|(_, _, hash)| hash.clone())
                    .collect();
                rebased.removed = delta.removed;
                source.event_ids.extend(delta.source.event_ids);
                if rebased != batch {
                    let pending = source
                        .pending_imports
                        .iter_mut()
                        .find(|pending| pending.object_id == batch.object_id)
                        .ok_or(CalendarImportError::FeedChanged)?;
                    *pending = rebased.clone();
                    match self.save_calendar_source(id, &source, head).await {
                        Ok(_) => {
                            batch = rebased;
                        }
                        Err(ClientError::Api { status: 409, .. }) => continue,
                        Err(error) => return Err(error),
                    }
                }
                report.unchanged = delta.unchanged;
                report.added = 0;
                report.updated = 0;
                let mut rules_loaded = false;
                for (index, event_id) in batch.events.iter().enumerate() {
                    let mut event = outcome
                        .events
                        .iter()
                        .find(|event| event.uid == batch.uids[index])
                        .cloned()
                        .ok_or(CalendarImportError::FeedChanged)?;
                    let rule = outcome
                        .rules
                        .iter()
                        .find(|(uid, _)| *uid == event.uid)
                        .map(|(_, rule)| rule.as_str());
                    if event_hash(&event, rule)? != batch.hashes[index]
                        || source
                            .event_ids
                            .get(&event.id)
                            .copied()
                            .unwrap_or_else(|| event.id.into())
                            != *event_id
                    {
                        return Err(CalendarImportError::EventChanged.into());
                    }
                    set_import(
                        &mut event,
                        &batch,
                        source.import_anchor.unwrap_or(batch.object_id),
                    );
                    let result = async {
                        if !rules_loaded
                            && matches!(
                                event.recurrence,
                                clipper_schedule::Recurrence::Imported { .. }
                            )
                        {
                            if self
                                .local_store
                                .import_file_object(&batch.object_id.to_string())
                                .await?
                                .is_none()
                            {
                                let object =
                                    self.api.get_object(&batch.object_id.to_string()).await?;
                                verify_object_list_item_envelope(&object)?;
                                if object.id != batch.object_id
                                    || object.kind != ObjectKind::File
                                    || object.revision != 1
                                {
                                    return Err(CalendarImportError::FeedChanged.into());
                                }
                                self.check_revision_advance(&object).await?;
                                let key = self.current_encryption_key().await?;
                                self.retain_downloaded_file(&object, &key, epoch).await?;
                            }
                            self.recurrence_engine(&event.resolved_recurrence()).await?;
                            rules_loaded = true;
                        }
                        let new = !source
                            .imports()
                            .any(|previous| previous.events.contains(event_id));
                        self.save_delta_event(event_id, &event, &batch, new, None)
                            .await
                    }
                    .await;
                    match result {
                        Ok(true) => report.updated += 1,
                        Ok(false) => report.added += 1,
                        Err(error) => {
                            let (current, _) = self.read_calendar_source(id).await?;
                            if current
                                .pending_imports
                                .iter()
                                .any(|pending| pending.object_id == batch.object_id)
                                || current
                                    .active_import
                                    .as_ref()
                                    .is_some_and(|active| active.object_id == batch.object_id)
                            {
                                return Err(error);
                            }
                            break;
                        }
                    }
                }
            }
            let (mut source, head) = self.read_calendar_source(id).await?;
            if source.active_import.as_ref().map(|active| active.object_id) != basis
                || source
                    .pending_imports
                    .iter()
                    .find(|pending| pending.object_id == batch.object_id)
                    != Some(&batch)
            {
                continue;
            }
            report.superseded = loses(&batch, &source);
            source
                .pending_imports
                .retain(|pending| pending.object_id != batch.object_id);
            if report.superseded {
                retire_import(&mut source, batch.clone());
            } else {
                activate_delta(&mut source, &batch);
            }
            match self.save_calendar_source(id, &source, head).await {
                Ok(_) => break,
                Err(ClientError::Api { status: 409, .. }) => {}
                Err(error) => return Err(error),
            }
        }
        if !report.superseded {
            report.tombstoned = self.complete_calendar_removals(id, &batch).await?;
        }
        if let Err(error) = self.cleanup_calendar_imports(id).await {
            report
                .skipped
                .push(format!("Calendar import cleanup remains pending: {error}"));
        }
        Ok(report)
    }

    async fn read_delta_event(
        &self,
        id: &str,
    ) -> Result<Option<(IngestedEvent, LocalHead)>, ClientError> {
        if self.load_import_event(id).await?.is_none() {
            return Ok(None);
        }
        self.local_store
            .schedule_records_with_heads()
            .await?
            .into_iter()
            .find(|(object_id, _, _)| object_id == id)
            .map(|(_, record, head)| {
                record
                    .as_ingested()
                    .cloned()
                    .map(|event| (event, head))
                    .ok_or_else(|| CalendarImportError::EventChanged.into())
            })
            .transpose()
    }

    async fn save_delta_event(
        &self,
        id: &ObjectId,
        event: &IngestedEvent,
        batch: &CalendarImport,
        new: bool,
        replaced_import: Option<ObjectId>,
    ) -> Result<bool, ClientError> {
        if new {
            match self
                .write_schedule_record(
                    &id.to_string(),
                    ScheduleRecord::Ingested(Box::new(event.clone())),
                    EnvelopePlacement::Create,
                )
                .await
            {
                Ok(_) => return Ok(false),
                Err(ClientError::Api { status: 409, .. }) => {}
                Err(error) => return Err(error),
            }
        }
        loop {
            let current = self.read_delta_event(&id.to_string()).await?;
            let placement = if let Some((current, head)) = current {
                if current.source != event.source || current.uid != event.uid {
                    return Err(CalendarImportError::EventChanged.into());
                }
                if &current == event {
                    return Ok(true);
                }
                if replaced_import.is_some_and(|import| current.snapshot() != Some(import)) {
                    return Ok(true);
                }
                if replaced_import.is_none()
                    && current.import_fetched_at.is_some_and(|time| {
                        time <= chrono::Utc::now() + chrono::TimeDelta::minutes(5)
                            && (time, current.snapshot().map(uuid::Uuid::from))
                                > (batch.fetched_at, Some(uuid::Uuid::from(batch.object_id)))
                    })
                {
                    return Ok(true);
                }
                EnvelopePlacement::Revise(head)
            } else {
                match self.api.get_object_head(&id.to_string()).await {
                    Ok(head) => {
                        verify_object_head_envelope(&head)?;
                        if head.kind != ObjectKind::Schedule
                            || head.id != *id
                            || head.envelope.body.operation != ObjectEnvelopeOperation::Delete
                        {
                            return Err(CalendarImportError::EventChanged.into());
                        }
                        self.check_revision_advance(&head).await?;
                        EnvelopePlacement::Revise(LocalHead {
                            revision: head.revision,
                            parent_hash: crypto::object_envelope_parent_hash(&head.envelope.body)?,
                        })
                    }
                    Err(ClientError::Api { status: 404, .. }) => EnvelopePlacement::Create,
                    Err(error) => return Err(error),
                }
            };
            let existed = !matches!(placement, EnvelopePlacement::Create);
            match self
                .write_schedule_record(
                    &id.to_string(),
                    ScheduleRecord::Ingested(Box::new(event.clone())),
                    placement,
                )
                .await
            {
                Ok(_) => return Ok(existed),
                Err(ClientError::Api { status: 409, .. }) => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) async fn tombstone_delta_event(
        &self,
        source_id: &str,
        id: &ObjectId,
        batch: &CalendarImport,
    ) -> Result<bool, ClientError> {
        loop {
            let (source, _) = self.read_calendar_source(source_id).await?;
            if source
                .active_import
                .as_ref()
                .is_none_or(|active| active.object_id != batch.object_id)
            {
                return Ok(false);
            }
            let Some((event, head)) = self.read_delta_event(&id.to_string()).await? else {
                return Ok(true);
            };
            if event.source != source.id
                || source.contains_event(&id.to_string(), &event)
                || event.import_fetched_at.is_some_and(|time| {
                    time <= chrono::Utc::now() + chrono::TimeDelta::minutes(5)
                        && (time, event.snapshot().map(uuid::Uuid::from))
                            > (batch.fetched_at, Some(uuid::Uuid::from(batch.object_id)))
                })
            {
                return Ok(false);
            }
            if let Some(window) = &batch.window
                && !self.event_in_scope(&event, window).await?
            {
                return Ok(false);
            }
            match self
                .write_tombstone_at(&id.to_string(), ObjectKind::Schedule, Some(head))
                .await
            {
                Ok((seq, tombstone)) => {
                    let visible = self
                        .local_store
                        .apply_local_tombstone(
                            ObjectKind::Schedule,
                            &id.to_string(),
                            seq,
                            &tombstone,
                            RECENT_CLIPBOARD_LIMIT,
                        )
                        .await?;
                    self.publish_visible_state(visible).await;
                    return Ok(true);
                }
                Err(ClientError::Api { status: 409, .. }) => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) async fn complete_calendar_removals(
        &self,
        source_id: &str,
        batch: &CalendarImport,
    ) -> Result<u32, ClientError> {
        let mut confirmed = HashSet::new();
        for id in &batch.removed {
            if self.tombstone_delta_event(source_id, id, batch).await? {
                confirmed.insert(*id);
            }
        }
        loop {
            let (mut source, head) = self.read_calendar_source(source_id).await?;
            let mut changed = false;
            for retained in &mut source.retained_imports {
                changed |= !retained.removed.is_empty();
                retained.removed.clear();
            }
            if let Some(active) = source
                .active_import
                .as_mut()
                .filter(|active| active.object_id == batch.object_id)
            {
                let count = active.removed.len();
                active.removed.retain(|id| !confirmed.contains(id));
                changed |= active.removed.len() != count;
            }
            if !changed {
                break;
            }
            match self.save_calendar_source(source_id, &source, head).await {
                Ok(_) => break,
                Err(ClientError::Api { status: 409, .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(confirmed.len() as u32)
    }

    pub(super) async fn track_delta_orphan(
        &self,
        source_id: &str,
        id: &str,
        event: &IngestedEvent,
    ) -> Result<(), ClientError> {
        let raw = event.snapshot().ok_or(CalendarImportError::EventChanged)?;
        let fetched_at = event
            .import_fetched_at
            .ok_or(CalendarImportError::EventChanged)?;
        let event_id = id.parse().map_err(|source| ClientError::InvalidId {
            kind: "import event id",
            source,
        })?;
        loop {
            let (mut source, head) = self.read_calendar_source(source_id).await?;
            if !source.superseded.contains_key(&raw)
                || source.contains_event(id, event)
                || source
                    .pending_imports
                    .iter()
                    .any(|batch| batch.object_id == raw)
                || source
                    .retired_imports
                    .iter()
                    .any(|batch| batch.object_id == raw && batch.events.contains(&event_id))
            {
                return Ok(());
            }
            let batch = CalendarImport {
                object_id: raw,
                fetched_at,
                events: vec![event_id],
                content_hash: Vec::new(),
                window: Some(ImportWindow::around(fetched_at)),
                uids: vec![event.uid.clone()],
                hashes: vec![event_hash(event, None)?],
                removed: Vec::new(),
            };
            let mut retired = RetiredImport::from(batch);
            retired.superseded_by = source.superseded.get(&raw).copied();
            source.retired_imports.push(retired);
            match self.save_calendar_source(source_id, &source, head).await {
                Ok(_) => return Ok(()),
                Err(ClientError::Api { status: 409, .. }) => {}
                Err(error) => return Err(error),
            }
        }
    }

    async fn retain_delta_history(
        &self,
        source_id: &str,
        batch: &CalendarImport,
        index: usize,
    ) -> Result<(), ClientError> {
        let id = batch.events[index];
        loop {
            let (mut source, head) = self.read_calendar_source(source_id).await?;
            if source.imports().any(|existing| {
                existing.object_id == batch.object_id && existing.events.contains(&id)
            }) {
                return Ok(());
            }
            if let Some(active) = &mut source.active_import {
                remove_events(active, &HashSet::from([id]));
            }
            for retained in &mut source.retained_imports {
                remove_events(retained, &HashSet::from([id]));
            }
            if let Some(history) = source
                .retained_imports
                .iter_mut()
                .find(|history| history.object_id == batch.object_id)
            {
                history.events.push(id);
                history.uids.push(batch.uids[index].clone());
                history.hashes.push(batch.hashes[index].clone());
                history.removed.clear();
            } else {
                let mut history = batch.clone();
                history.events = vec![id];
                history.uids = vec![batch.uids[index].clone()];
                history.hashes = vec![batch.hashes[index].clone()];
                history.removed.clear();
                source.retained_imports.push(history);
            }
            match self.save_calendar_source(source_id, &source, head).await {
                Ok(_) => return Ok(()),
                Err(ClientError::Api { status: 409, .. }) => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) async fn cleanup_calendar_delta(
        &self,
        source_id: &str,
        batch: &CalendarImport,
    ) -> Result<bool, ClientError> {
        let mut complete = true;
        for (index, id) in batch.events.iter().enumerate() {
            let (source, _) = self.read_calendar_source(source_id).await?;
            let Some(active) = source.active_import.as_ref() else {
                self.purge_import_object(
                    &id.to_string(),
                    ObjectKind::Schedule,
                    source.id,
                    batch.object_id,
                )
                .await?;
                continue;
            };
            let Some((event, _)) = self.read_delta_event(&id.to_string()).await? else {
                continue;
            };
            if event.source != source.id {
                return Err(CalendarImportError::EventChanged.into());
            }
            if event.snapshot() != Some(batch.object_id)
                || (source.contains_event(&id.to_string(), &event)
                    && source
                        .imports()
                        .any(|existing| Some(existing.object_id) == event.snapshot()))
            {
                continue;
            }
            let window = active
                .window
                .clone()
                .unwrap_or_else(|| ImportWindow::around(active.fetched_at));
            if !self.event_in_scope(&event, &window).await? {
                self.retain_delta_history(source_id, batch, index).await?;
                continue;
            }
            if source
                .imports()
                .any(|existing| existing.events.contains(id))
            {
                let held = self.imported_events(&source).await?;
                let expected = held
                    .get(&event.uid)
                    .ok_or(CalendarImportError::EventChanged)?;
                if !source.imports().any(|existing| {
                    Some(existing.object_id) == expected.event.snapshot()
                        && existing.events.contains(id)
                }) {
                    complete = false;
                    continue;
                }
                let mut restored = expected.event.clone();
                restored.import_fetched_at = Some(active.fetched_at);
                self.save_delta_event(id, &restored, active, false, Some(batch.object_id))
                    .await?;
            } else {
                self.tombstone_delta_event(source_id, id, active).await?;
            }
        }
        if !complete {
            return Ok(false);
        }
        let (source, _) = self.read_calendar_source(source_id).await?;
        if !source
            .imports()
            .chain(source.pending_imports.iter())
            .any(|existing| existing.object_id == batch.object_id)
        {
            self.purge_import_object(
                &batch.object_id.to_string(),
                ObjectKind::File,
                source.id,
                batch.object_id,
            )
            .await?;
        }
        Ok(true)
    }
}
