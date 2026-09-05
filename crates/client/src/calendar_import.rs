//! Stage complete imports before activating them; reclaim only imported data.
use clipper_schedule::ingest::CalendarImport;

use super::*;

type Records = [(String, ScheduleRecord, LocalHead)];

/// A source becomes visible only once every event named by its manifest is local.
/// Sync may deliver the source revision before some of the batch's events.
pub(super) fn ready_sources(records: &Records) -> HashSet<SourceId> {
    let events: HashMap<_, _> = records
        .iter()
        .filter_map(|(id, record, _)| record.as_ingested().map(|event| (id.as_str(), event)))
        .collect();
    records
        .iter()
        .filter_map(|(_, record, _)| record.as_source())
        .filter(|source| {
            source.active_import.as_ref().is_some_and(|batch| {
                let mut seen = HashSet::new();
                batch.events.iter().all(|id| {
                    seen.insert(*id)
                        && events.get(id.to_string().as_str()).is_some_and(|event| {
                            event.source == source.id && event.import == Some(batch.object_id)
                        })
                })
            })
        })
        .map(|source| source.id)
        .collect()
}

impl SyncEngine {
    async fn calendar_source(&self, id: &str) -> Result<(CalendarSource, LocalHead), ClientError> {
        self.local_store
            .schedule_records_with_heads()
            .await?
            .into_iter()
            .find_map(|(object_id, record, head)| {
                (object_id == id)
                    .then(|| record.as_source().cloned().map(|source| (source, head)))
                    .flatten()
            })
            .ok_or_else(|| ClientError::ItemNotFound { id: id.into() })
    }

    async fn save_calendar_source(
        &self,
        id: &str,
        source: &CalendarSource,
        head: LocalHead,
    ) -> Result<LocalHead, ClientError> {
        self.write_schedule_record(
            id,
            ScheduleRecord::Source(Box::new(source.clone())),
            EnvelopePlacement::Revise(head),
        )
        .await?;
        let (saved, new_head) = self.calendar_source(id).await?;
        if saved != *source || new_head.revision != head.revision + 1 {
            return Err(ClientError::InvalidArgument(
                "Calendar changed concurrently; retry".into(),
            ));
        }
        Ok(new_head)
    }

    /// Refresh replaces the source's entire imported view. Parse or staging failures
    /// leave the previous view active. Each batch has distinct storage identities;
    /// references from recordings are never reassigned to a replacement event.
    pub async fn sync_calendar_source(&self, object_id: &str) -> Result<IngestReport, ClientError> {
        let _write = self.calendar_write.lock().await;
        self.cleanup_calendar_imports(object_id).await?;
        let (mut source, mut head) = self.calendar_source(object_id).await?;
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let text = if let Some(batch) = &source.pending_import {
            String::from_utf8(
                self.download_file_bytes(&batch.object_id.to_string())
                    .await?,
            )
            .map_err(|_| ClientError::InvalidArgument("Original import is not UTF-8".into()))?
        } else {
            let SourceKind::Ics { url } = &source.kind;
            fetch_calendar_feed(url).await?
        };
        let outcome = parse_calendar_feed(&text, source.id)?;
        let mut uids = HashSet::new();
        if !outcome.events.iter().all(|event| uids.insert(&event.uid)) {
            return Err(ClientError::InvalidArgument(
                "Import contains duplicate event UIDs; previous calendar was kept".into(),
            ));
        }
        if !outcome.skipped.is_empty() {
            return Err(ClientError::InvalidArgument(format!(
                "Import was not replaced: {} event(s) could not be read. {}",
                outcome.skipped.len(),
                outcome
                    .skipped
                    .iter()
                    .take(5)
                    .map(|entry| entry.reason.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }
        if self.history_epoch.load(Ordering::SeqCst) != epoch {
            return Err(ClientError::NotAuthenticated);
        }
        if self.calendar_source(object_id).await?.1 != head {
            return Err(ClientError::InvalidArgument(
                "Calendar changed during refresh; retry".into(),
            ));
        }
        // Check both event records and the largest source manifest before upload.
        // The snapshot id is fixed-width, so a probe id gives the same size bound.
        let probe_id: ObjectId = uuid::Uuid::nil().into();
        for event in &outcome.events {
            let mut event = event.clone();
            event.import = Some(probe_id);
            check_record_size(&ScheduleRecord::Ingested(Box::new(event)))?;
        }
        let probe = CalendarImport {
            object_id: probe_id,
            fetched_at: chrono::Utc::now(),
            events: vec![probe_id; outcome.events.len()],
        };
        let mut probe_source = source.clone();
        probe_source.pending_import = Some(probe.clone());
        check_record_size(&ScheduleRecord::Source(Box::new(probe_source.clone())))?;
        if let Some(old) = probe_source.active_import.replace(probe) {
            probe_source.retired_imports.push(old);
        }
        probe_source.pending_import = None;
        check_record_size(&ScheduleRecord::Source(Box::new(probe_source)))?;

        let batch = if let Some(batch) = source.pending_import.clone() {
            batch
        } else {
            let fetched_at = chrono::Utc::now();
            let raw_id = self
                .upload_file_bytes(
                    &format!(
                        "calendar-import-{}-{}.ics",
                        source.id,
                        fetched_at.timestamp_millis()
                    ),
                    Some("text/calendar"),
                    text.as_bytes(),
                )
                .await?;
            let snapshot_uuid: uuid::Uuid =
                raw_id.parse().map_err(|source| ClientError::InvalidId {
                    kind: "import id",
                    source,
                })?;
            let batch = CalendarImport {
                object_id: snapshot_uuid.into(),
                fetched_at,
                events: outcome
                    .events
                    .iter()
                    .map(|event| uuid::Uuid::new_v5(&snapshot_uuid, event.uid.as_bytes()).into())
                    .collect(),
            };
            source.pending_import = Some(batch.clone());
            match self.save_calendar_source(object_id, &source, head).await {
                Ok(saved) => head = saved,
                Err(error) => {
                    // A lost response may still have committed the source: preserve
                    // the raw file rather than risk deleting an accepted snapshot.
                    return Err(error);
                }
            }
            batch
        };
        let snapshot_id = batch.object_id;
        let snapshot_uuid: uuid::Uuid = snapshot_id.into();
        let events: Vec<_> = outcome
            .events
            .into_iter()
            .map(|mut event| {
                event.import = Some(snapshot_id);
                let id: ObjectId = uuid::Uuid::new_v5(&snapshot_uuid, event.uid.as_bytes()).into();
                (id, event)
            })
            .collect();
        if events.iter().map(|(id, _)| *id).collect::<Vec<_>>() != batch.events {
            return Err(ClientError::InvalidArgument(
                "Pending import does not match its original feed".into(),
            ));
        }
        let records = self.local_store.schedule_records_with_heads().await?;
        for (id, event) in &events {
            if self.history_epoch.load(Ordering::SeqCst) != epoch {
                return Err(ClientError::NotAuthenticated);
            }
            if let Some((_, record, _)) = records
                .iter()
                .find(|(object_id, _, _)| object_id == &id.to_string())
            {
                if record.as_ingested() != Some(event) {
                    return Err(ClientError::InvalidArgument(
                        "Staged event differs from its original import".into(),
                    ));
                }
                continue;
            }
            if let Some(current) = self.load_import_event(&id.to_string()).await? {
                if current.as_ingested() != Some(event) {
                    return Err(ClientError::InvalidArgument(
                        "Staged event differs from its original import".into(),
                    ));
                }
            } else if let Err(error) = self
                .write_schedule_record(
                    &id.to_string(),
                    ScheduleRecord::Ingested(Box::new(event.clone())),
                    EnvelopePlacement::Create,
                )
                .await
            {
                // Another device can resume the same batch. Accept its write only
                // after authenticating and comparing the complete event.
                if !matches!(&error, ClientError::Api { status: 409, .. })
                    || self
                        .load_import_event(&id.to_string())
                        .await?
                        .as_ref()
                        .and_then(|record| record.as_ingested())
                        != Some(event)
                {
                    return Err(error);
                }
            }
        }
        if self.history_epoch.load(Ordering::SeqCst) != epoch {
            return Err(ClientError::NotAuthenticated);
        }
        source.pending_import = None;
        let previous = source.active_import.replace(batch);
        let removed = previous.as_ref().map_or(0, |entry| entry.events.len());
        if let Some(previous) = previous {
            source.retired_imports.push(previous);
        }
        self.save_calendar_source(object_id, &source, head).await?;
        let mut report = IngestReport {
            added: events.len() as u32,
            tombstoned: removed as u32,
            ..Default::default()
        };
        if let Err(error) = self.cleanup_calendar_imports(object_id).await {
            report.skipped.push(format!(
                "New import is active; previous import cleanup remains pending: {error}"
            ));
        }
        Ok(report)
    }

    async fn load_import_event(&self, id: &str) -> Result<Option<ScheduleRecord>, ClientError> {
        let item = match self.api.get_object(id).await {
            Ok(item) => item,
            Err(ClientError::Api { status: 404, .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        if item.id.to_string() != id || item.kind != ObjectKind::Schedule {
            return Err(ClientError::InvalidArgument(
                "Import event identity mismatch".into(),
            ));
        }
        let key = self.current_encryption_key().await?;
        let (record, encrypted) = self
            .decrypt_schedule_object_item(&self.api, &item, &key)
            .await?;
        if record.as_ingested().is_none() {
            return Err(ClientError::InvalidArgument(
                "Import target is not an imported event".into(),
            ));
        }
        let visible = self
            .local_store
            .persist_local_schedule_present_encrypted(
                StoredObjectIdentity {
                    object_id: id,
                    created_at: &item.created_at,
                    source_device_id: &item.source_device_id.to_string(),
                },
                record.clone(),
                &encrypted,
                item.created_seq,
                item.created_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?;
        self.publish_visible_state(visible).await;
        Ok(Some(record))
    }

    /// Verify even tombstoned targets before purging. A missing historical revision
    /// means the object is already absent; it is not permission to delete another kind.
    async fn purge_import_object(
        &self,
        id: &str,
        kind: ObjectKind,
        source: SourceId,
        batch: ObjectId,
    ) -> Result<(), ClientError> {
        let historical = match self.api.get_object_revision(id, 1).await {
            Ok(item) => item,
            Err(ClientError::Api { status: 404, .. }) => return Ok(()),
            Err(error) => return Err(error),
        };
        verify_object_list_item_envelope(&historical)?;
        if historical.id.to_string() != id || historical.kind != kind {
            return Err(ClientError::InvalidArgument(
                "Import cleanup identity mismatch".into(),
            ));
        }
        if kind == ObjectKind::Schedule {
            let pin = clipper_schedule::ObjectRevisionRef {
                object_id: historical.id,
                revision: 1,
                body_hash: crypto::object_envelope_parent_hash(&historical.envelope.body)?,
            };
            let record = self.schedule_revision(pin).await?;
            if !record.as_ingested().is_some_and(|event| {
                event.source == source
                    && event.import == Some(batch)
                    && ObjectId::from(uuid::Uuid::new_v5(
                        &uuid::Uuid::from(batch),
                        event.uid.as_bytes(),
                    ))
                    .to_string()
                        == id
            }) {
                return Err(ClientError::InvalidArgument(
                    "Import cleanup target is not an event from this batch".into(),
                ));
            }
        } else {
            let key = self.current_encryption_key().await?;
            let meta = decrypt_file_meta_bytes(
                &historical.meta_nonce,
                &historical.meta_ciphertext,
                &key,
                &historical.envelope.body,
            )?;
            if id != batch.to_string()
                || !meta
                    .filename
                    .starts_with(&format!("calendar-import-{source}-"))
            {
                return Err(ClientError::InvalidArgument(
                    "Import cleanup target is not this source's raw feed".into(),
                ));
            }
        }
        match self.api.get_object(id).await {
            Ok(item) => {
                verify_object_list_item_envelope(&item)?;
                if item.id.to_string() != id || item.kind != kind {
                    return Err(ClientError::InvalidArgument(
                        "Import cleanup identity mismatch".into(),
                    ));
                }
                if kind == ObjectKind::Schedule {
                    self.load_import_event(id).await?;
                }
                // File heads are hydrated by ordinary object sync. Missing heads
                // defer cleanup rather than inventing a chain position.
                let (seq, head) = self.write_tombstone(id, kind).await?;
                let visible = self
                    .local_store
                    .apply_local_tombstone(kind, id, seq, head, RECENT_CLIPBOARD_LIMIT)
                    .await?;
                self.publish_visible_state(visible).await;
            }
            Err(ClientError::Api { status: 404, .. }) => {}
            Err(error) => return Err(error),
        }
        match self.api.delete_object(id).await {
            Ok(response) => {
                let visible = self
                    .local_store
                    .apply_local_delete(kind, id, response.deleted_seq, RECENT_CLIPBOARD_LIMIT)
                    .await?;
                self.publish_visible_state(visible).await;
            }
            Err(ClientError::Api { status: 404, .. }) => {}
            Err(error) => return Err(error),
        }
        self.schedule_history
            .lock()
            .await
            .retain(|(_, pin), _| pin.object_id.to_string() != id);
        Ok(())
    }

    async fn cleanup_calendar_imports(&self, id: &str) -> Result<(), ClientError> {
        let (mut source, head) = self.calendar_source(id).await?;
        if source.retired_imports.is_empty() {
            return Ok(());
        }
        let records = self.local_store.schedule_records_with_heads().await?;
        for batch in &source.retired_imports {
            if source
                .active_import
                .as_ref()
                .is_some_and(|active| active.object_id == batch.object_id)
            {
                return Err(ClientError::InvalidArgument(
                    "Active import cannot also be retired".into(),
                ));
            }
            for event_id in &batch.events {
                let event_id = event_id.to_string();
                if let Some((_, record, _)) = records.iter().find(|(id, _, _)| id == &event_id)
                    && !record.as_ingested().is_some_and(|event| {
                        event.source == source.id && event.import == Some(batch.object_id)
                    })
                {
                    return Err(ClientError::InvalidArgument(
                        "Import cleanup target is not an event from this batch".into(),
                    ));
                }
                self.purge_import_object(
                    &event_id,
                    ObjectKind::Schedule,
                    source.id,
                    batch.object_id,
                )
                .await?;
            }
            self.purge_import_object(
                &batch.object_id.to_string(),
                ObjectKind::File,
                source.id,
                batch.object_id,
            )
            .await?;
        }
        source.retired_imports.clear();
        self.save_calendar_source(id, &source, head).await?;
        Ok(())
    }

    /// Source removal first hides its imported view, then purges only its batches.
    /// Actual records and locally authored plans/overrides are never cleanup targets.
    pub(super) async fn remove_calendar_imports(&self, id: &str) -> Result<(), ClientError> {
        let (mut source, head) = match self.calendar_source(id).await {
            Ok(value) => value,
            Err(ClientError::ItemNotFound { .. }) => return Ok(()),
            Err(error) => return Err(error),
        };
        if source.pending_import.is_some() {
            return Err(ClientError::InvalidArgument(
                "Finish the pending calendar import before removing this source".into(),
            ));
        }
        if let Some(batch) = source.active_import.take() {
            source.retired_imports.push(batch);
            self.save_calendar_source(id, &source, head).await?;
        }
        self.cleanup_calendar_imports(id).await
    }

    pub(super) async fn is_import_file(&self, id: &str) -> Result<bool, ClientError> {
        Ok(self
            .local_store
            .schedule_records_with_ids()
            .await
            .iter()
            .filter_map(|(_, record)| record.as_source())
            .any(|source| {
                source
                    .active_import
                    .iter()
                    .chain(source.retired_imports.iter())
                    .any(|batch| batch.object_id.to_string() == id)
            }))
    }
}

fn check_record_size(record: &ScheduleRecord) -> Result<(), ClientError> {
    let bytes = serde_json::to_vec(record)
        .map_err(|error| ClientError::InvalidArgument(error.to_string()))?;
    if bytes.len() + 128 > MAX_SCHEDULE_PAYLOAD_CIPHERTEXT_BYTES as usize {
        return Err(ClientError::InvalidArgument(
            "Import exceeds the schedule record/manifest size limit; previous calendar was kept"
                .into(),
        ));
    }
    Ok(())
}
