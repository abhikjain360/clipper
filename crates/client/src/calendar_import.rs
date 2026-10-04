//! Staging and activating calendar imports.
//!
//! A batch becomes active only once it is complete. Cleanup reclaims imported
//! data only, never a plan or a recording the user wrote.

use clipper_schedule::ingest::{CalendarImport, RetiredImport};

use super::*;
use crate::api_client::CalendarImportError;

type Records = [(String, ScheduleRecord, LocalHead)];

pub(super) struct CachedImportRules {
    epoch: u64,
    import: ObjectId,
    head: LocalHead,
    engine: RecurrenceEngine,
}

const MAX_IMPORT_BYTES: i64 = 8 * 1024 * 1024;

impl SyncEngine {
    /// An engine that can expand this recurrence, reading an imported rule
    /// back out of its authenticated snapshot.
    ///
    /// Reads live metadata even on a cache hit, so a deleted snapshot stops
    /// resolving.
    pub(super) async fn recurrence_engine(
        &self,
        recurrence: &clipper_schedule::Recurrence,
    ) -> Result<RecurrenceEngine, ClientError> {
        let clipper_schedule::Recurrence::Imported { import, .. } = recurrence else {
            return Ok(RecurrenceEngine::new());
        };
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let id = import.to_string();
        let object = self.local_store.import_file_object(&id).await?.ok_or_else(|| {
            ClientError::InvalidArgument(
                "Original calendar import unavailable; unsupported recurrence cannot be expanded".into(),
            )
        })?;
        let head = LocalHead {
            revision: object.envelope.body.revision,
            parent_hash: crypto::object_envelope_parent_hash(&object.envelope.body)?,
        };
        if head.revision != 1 {
            return Err(CalendarImportError::InvalidFeed(
                "The original calendar import was modified; refusing to reinterpret its events"
                    .into(),
            )
            .into());
        }
        {
            let cache = self.import_rules.lock().await;
            if let Some(entry) = cache
                .iter()
                .find(|entry| entry.epoch == epoch && entry.import == *import && entry.head == head)
            {
                if self.history_epoch.load(Ordering::SeqCst) != epoch {
                    return Err(ClientError::NotAuthenticated);
                }
                return Ok(entry.engine.clone());
            }
        }
        let [payload] = object.payloads.as_slice() else {
            return Err(ClientError::InvalidArgument(
                "Import must have one payload".into(),
            ));
        };
        check_payload_ciphertext_size(payload, MAX_IMPORT_BYTES + 16)?;
        #[cfg(not(target_family = "wasm"))]
        let cached = self.local_store.import_file_ciphertext(&id, head).await?;
        #[cfg(target_family = "wasm")]
        let cached: Option<Vec<u8>> = None;
        let ciphertext = match cached {
            Some(bytes) if verify_payload_hash(payload, &bytes).is_ok() => bytes,
            _ => {
                let api = self.api_for_session(epoch).await?;
                api.download_object_payload(&id, &payload.id.to_string(), payload.ciphertext_size)
                    .await?
            }
        };
        verify_payload_hash(payload, &ciphertext)?;
        // Hold the key read lock across the cache writes, so authentication
        // cannot switch the local profile while this import resolves.
        let key_guard = self.encryption_key.read().await;
        if self.history_epoch.load(Ordering::SeqCst) != epoch {
            return Err(ClientError::NotAuthenticated);
        }
        let key = key_guard.as_ref().ok_or(ClientError::NotAuthenticated)?;
        let plaintext = decrypt_file_blob_bytes(
            &payload.nonce,
            &ciphertext,
            key,
            &object.envelope.body,
            payload.id,
        )?;
        let text = std::str::from_utf8(&plaintext).map_err(|_| CalendarImportError::InvalidText)?;
        let rules = clipper_schedule::parse_imported_recurrence_rules(text, *import)
            .map_err(ClientError::CalendarFeed)?;
        let engine = RecurrenceEngine::with_imported_rules(rules);
        #[cfg(not(target_family = "wasm"))]
        self.local_store
            .cache_import_file_ciphertext(&id, head, &ciphertext)
            .await?;
        let current = self
            .local_store
            .import_file_object(&id)
            .await?
            .ok_or_else(|| {
                ClientError::InvalidArgument("Original calendar import was deleted".into())
            })?;
        if crypto::object_envelope_parent_hash(&current.envelope.body)? != head.parent_hash {
            return Err(ClientError::InvalidArgument(
                "Original calendar import changed during resolution".into(),
            ));
        }
        let mut cache = self.import_rules.lock().await;
        cache.retain(|entry| entry.epoch == epoch && entry.import != *import);
        while cache.len() >= 4 {
            cache.pop_front();
        }
        cache.push_back(CachedImportRules {
            epoch,
            import: *import,
            head,
            engine: engine.clone(),
        });
        Ok(engine)
    }
}

/// The sources whose whole active batch is present locally.
///
/// Sync can deliver a source revision before some of its events. Such a source
/// stays hidden until the rest arrive.
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
                            event.source == source.id && event.belongs_to_import(batch.object_id)
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
        self.local_head(id).await
    }

    pub async fn sync_calendar_source(&self, object_id: &str) -> Result<IngestReport, ClientError> {
        self.run_work(
            Some("Syncing a calendar".into()),
            self.sync_calendar_source_inner(object_id),
        )
        .await
    }

    async fn sync_calendar_source_inner(
        &self,
        object_id: &str,
    ) -> Result<IngestReport, ClientError> {
        self.set_calendar_work_label("Syncing", object_id).await;
        let _write = self.calendar_write.lock().await;
        self.read_calendar_source(object_id).await?;
        self.cleanup_calendar_imports(object_id).await?;
        let (source, _) = self.calendar_source(object_id).await?;
        for batch in source.pending_imports {
            let result = async {
                let bytes = self
                    .download_file_bytes(&batch.object_id.to_string())
                    .await?;
                let text =
                    String::from_utf8(bytes).map_err(|_| CalendarImportError::InvalidText)?;
                self.finish_calendar_import(object_id, &text, &batch).await
            }
            .await;
            if let Err(error) = result {
                if !matches!(
                    error,
                    ClientError::CalendarFeed(_)
                        | ClientError::CalendarImport(_)
                        | ClientError::Crypto(_)
                        | ClientError::PayloadTooLarge { .. }
                        | ClientError::Api {
                            status: 404 | 413,
                            ..
                        }
                ) {
                    return Err(error);
                }
                warn!(source_id = %object_id, batch_id = %batch.object_id, %error, "Retiring unreadable calendar import");
                self.retire_pending_import(object_id, &batch).await?;
            }
        }
        self.cleanup_calendar_imports(object_id).await?;
        let (source, _) = self.read_calendar_source(object_id).await?;
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let SourceKind::Ics { url } = &source.kind;
        let text = fetch_calendar_feed(url).await?;
        let fetched_at = chrono::Utc::now();
        let visible = self
            .local_store
            .record_calendar_check(object_id, fetched_at, RECENT_CLIPBOARD_LIMIT)
            .await?;
        self.publish_visible_state(visible).await;
        let outcome = validated_feed(&text, &source)?;
        let content_hash = feed_hash(&outcome)?;
        let (source, _) = self.read_calendar_source(object_id).await?;
        if self.history_epoch.load(Ordering::SeqCst) != epoch {
            return Err(ClientError::NotAuthenticated);
        }
        if source
            .active_import
            .as_ref()
            .is_some_and(|batch| batch.content_hash == content_hash)
            && self
                .local_store
                .import_file_object(
                    &source
                        .active_import
                        .as_ref()
                        .expect("active import")
                        .object_id
                        .to_string(),
                )
                .await?
                .is_some()
        {
            return Ok(IngestReport {
                unchanged: outcome.events.len() as u32,
                feed_unchanged: true,
                ..Default::default()
            });
        }
        let batch = self
            .stage_calendar_import(object_id, &text, fetched_at)
            .await?;
        self.finish_calendar_import(object_id, &text, &batch).await
    }

    async fn retire_pending_import(
        &self,
        id: &str,
        batch: &CalendarImport,
    ) -> Result<(), ClientError> {
        loop {
            let (mut source, head) = self.read_calendar_source(id).await?;
            if !source
                .pending_imports
                .iter()
                .any(|pending| pending.object_id == batch.object_id)
            {
                return Ok(());
            }
            source
                .pending_imports
                .retain(|pending| pending.object_id != batch.object_id);
            if source
                .active_import
                .as_ref()
                .is_none_or(|active| active.object_id != batch.object_id)
                && !source
                    .retired_imports
                    .iter()
                    .any(|retired| retired.object_id == batch.object_id)
            {
                source.retired_imports.push(batch.clone().into());
            }
            match self.save_calendar_source(id, &source, head).await {
                Ok(_) => return Ok(()),
                Err(ClientError::Api { status: 409, .. }) => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) async fn stage_calendar_import(
        &self,
        object_id: &str,
        text: &str,
        fetched_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<CalendarImport, ClientError> {
        let (mut source, mut head) = self.read_calendar_source(object_id).await?;
        let outcome = validated_feed(text, &source)?;
        let content_hash = feed_hash(&outcome)?;
        let SourceKind::Ics { url } = &source.kind;
        source.owner_email = url::Url::parse(url).ok().and_then(|url| owner_email(&url));
        let probe_id: ObjectId = uuid::Uuid::nil().into();
        for event in &outcome.events {
            check_record_size(&ScheduleRecord::Ingested(Box::new(event.clone())))?;
        }
        let probe = CalendarImport {
            object_id: probe_id,
            fetched_at,
            events: vec![probe_id; outcome.events.len()],
            content_hash: content_hash.clone(),
        };
        let mut probe_source = source.clone();
        probe_source.pending_imports.push(probe.clone());
        check_record_size(&ScheduleRecord::Source(Box::new(probe_source.clone())))?;
        probe_source.pending_imports.pop();
        if let Some(old) = probe_source.active_import.replace(probe) {
            probe_source.retired_imports.push(old.into());
        }
        check_record_size(&ScheduleRecord::Source(Box::new(probe_source)))?;
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
            content_hash,
        };
        loop {
            if source
                .pending_imports
                .iter()
                .any(|pending| pending.object_id == batch.object_id)
            {
                break;
            }
            let SourceKind::Ics { url } = &source.kind;
            source.owner_email = url::Url::parse(url).ok().and_then(|url| owner_email(&url));
            source.pending_imports.push(batch.clone());
            match self.save_calendar_source(object_id, &source, head).await {
                Ok(_) => break,
                Err(ClientError::Api { status: 409, .. }) => {
                    (source, head) = self.read_calendar_source(object_id).await?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(batch)
    }

    pub(super) async fn finish_calendar_import(
        &self,
        object_id: &str,
        text: &str,
        batch: &CalendarImport,
    ) -> Result<IngestReport, ClientError> {
        let (source, _) = self.read_calendar_source(object_id).await?;
        if !source
            .pending_imports
            .iter()
            .any(|pending| pending == batch)
        {
            return Ok(IngestReport {
                superseded: source.active_import.as_ref() != Some(batch),
                ..Default::default()
            });
        }
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let outcome = validated_feed(text, &source)?;
        if feed_hash(&outcome)? != batch.content_hash {
            return Err(CalendarImportError::FeedChanged.into());
        }
        let snapshot_id = batch.object_id;
        let snapshot_uuid: uuid::Uuid = snapshot_id.into();
        let events: Vec<_> = outcome
            .events
            .into_iter()
            .map(|mut event| {
                event.import = Some(snapshot_id);
                if let clipper_schedule::Recurrence::Imported { import, .. } = &mut event.recurrence
                {
                    *import = snapshot_id;
                }
                let id: ObjectId = uuid::Uuid::new_v5(&snapshot_uuid, event.uid.as_bytes()).into();
                (id, event)
            })
            .collect();
        if events.iter().map(|(id, _)| *id).collect::<Vec<_>>() != batch.events {
            return Err(CalendarImportError::FeedChanged.into());
        }
        for (id, event) in &events {
            check_record_size(&ScheduleRecord::Ingested(Box::new(event.clone())))?;
            if self.history_epoch.load(Ordering::SeqCst) != epoch {
                return Err(ClientError::NotAuthenticated);
            }
            if let Err(error) = self.save_import_event(id, event).await {
                let (current, _) = self.read_calendar_source(object_id).await?;
                if current
                    .pending_imports
                    .iter()
                    .any(|pending| pending == batch)
                    || current.active_import.as_ref() == Some(batch)
                {
                    return Err(error);
                }
                break;
            }
        }
        if let Some((_, event)) = events.iter().find(|(_, event)| {
            matches!(
                event.recurrence,
                clipper_schedule::Recurrence::Imported { .. }
            )
        }) && let Err(error) = self.recurrence_engine(&event.recurrence).await
        {
            let (current, _) = self.read_calendar_source(object_id).await?;
            if current
                .pending_imports
                .iter()
                .any(|pending| pending == batch)
                || current.active_import.as_ref() == Some(batch)
            {
                return Err(error);
            }
        }
        let (mut source, mut head) = self.read_calendar_source(object_id).await?;
        let mut report;
        loop {
            if self.history_epoch.load(Ordering::SeqCst) != epoch {
                return Err(ClientError::NotAuthenticated);
            }
            if !source
                .pending_imports
                .iter()
                .any(|pending| pending == batch)
            {
                report = IngestReport {
                    superseded: source.active_import.as_ref() != Some(batch),
                    ..Default::default()
                };
                if !report.superseded
                    || source
                        .retired_imports
                        .iter()
                        .any(|retired| retired.object_id == batch.object_id)
                {
                    break;
                }
                source.retired_imports.push(batch.clone().into());
            } else {
                source
                    .pending_imports
                    .retain(|pending| pending.object_id != batch.object_id);
                let latest = chrono::Utc::now() + chrono::TimeDelta::minutes(5);
                let loses = batch.fetched_at > latest
                    || source.active_import.as_ref().is_some_and(|active| {
                        active.fetched_at <= latest
                            && (active.fetched_at, uuid::Uuid::from(active.object_id))
                                >= (batch.fetched_at, uuid::Uuid::from(batch.object_id))
                    });
                report = IngestReport {
                    superseded: loses,
                    ..Default::default()
                };
                if loses {
                    source.retired_imports.push(batch.clone().into());
                } else {
                    report.added = events.len() as u32;
                    if let Some(previous) = source.active_import.replace(batch.clone()) {
                        report.tombstoned = previous.events.len() as u32;
                        source.retired_imports.push(previous.into());
                    }
                }
            }
            match self.save_calendar_source(object_id, &source, head).await {
                Ok(_) => break,
                Err(ClientError::Api { status: 409, .. }) => {
                    (source, head) = self.read_calendar_source(object_id).await?;
                }
                Err(error) => return Err(error),
            }
        }
        if let Err(error) = self.cleanup_calendar_imports(object_id).await {
            report
                .skipped
                .push(format!("Calendar import cleanup remains pending: {error}"));
        }
        Ok(report)
    }

    async fn save_import_event(
        &self,
        id: &ObjectId,
        event: &IngestedEvent,
    ) -> Result<(), ClientError> {
        if let Some(current) = self.load_import_event(&id.to_string()).await? {
            if current.as_ingested() != Some(event) {
                return Err(CalendarImportError::EventChanged.into());
            }
        } else if let Err(error) = self
            .write_schedule_record(
                &id.to_string(),
                ScheduleRecord::Ingested(Box::new(event.clone())),
                EnvelopePlacement::Create,
            )
            .await
        {
            if matches!(&error, ClientError::Api { status: 409, .. })
                && let Some(current) = self.load_import_event(&id.to_string()).await?
            {
                if current.as_ingested() == Some(event) {
                    return Ok(());
                }
                return Err(CalendarImportError::EventChanged.into());
            }
            return Err(error);
        }
        Ok(())
    }

    pub(super) async fn read_calendar_source(
        &self,
        id: &str,
    ) -> Result<(CalendarSource, LocalHead), ClientError> {
        self.load_calendar_record(id, true)
            .await?
            .ok_or_else(|| ClientError::ItemNotFound { id: id.into() })?;
        self.calendar_source(id).await
    }

    pub async fn set_calendar_source_alarms(
        &self,
        id: &str,
        alarms_on: bool,
    ) -> Result<(), ClientError> {
        self.run_work(None, self.set_calendar_source_alarms_inner(id, alarms_on))
            .await
    }

    async fn set_calendar_source_alarms_inner(
        &self,
        id: &str,
        alarms_on: bool,
    ) -> Result<(), ClientError> {
        let _write = self.calendar_write.lock().await;
        loop {
            let (mut source, head) = self.read_calendar_source(id).await?;
            if source.alarms_on == alarms_on {
                return Ok(());
            }
            source.alarms_on = alarms_on;
            match self.save_calendar_source(id, &source, head).await {
                Ok(_) => return Ok(()),
                Err(ClientError::Api { status: 409, .. }) => {}
                Err(error) => return Err(error),
            }
        }
    }

    async fn load_import_event(&self, id: &str) -> Result<Option<ScheduleRecord>, ClientError> {
        self.load_calendar_record(id, false).await
    }

    async fn load_calendar_record(
        &self,
        id: &str,
        source: bool,
    ) -> Result<Option<ScheduleRecord>, ClientError> {
        loop {
            let item = match self.api.get_object(id).await {
                Ok(item) => item,
                Err(ClientError::Api { status: 404, .. }) => return Ok(None),
                Err(error) => return Err(error),
            };
            let result = self.load_calendar_record_once(id, source, &item).await;
            let Err(error) = &result else {
                return result;
            };
            if !matches!(
                error,
                ClientError::RevisionRejected(_) | ClientError::Api { status: 404, .. }
            ) {
                return result;
            }
            let head = match self.api.get_object_head(id).await {
                Ok(head) => head,
                Err(ClientError::Api { status: 404, .. }) => return Ok(None),
                Err(error) => return Err(error),
            };
            verify_object_head_envelope(&head)?;
            if head.id.to_string() != id || head.kind != ObjectKind::Schedule {
                return Err(ClientError::InvalidArgument(
                    "Calendar object identity mismatch".into(),
                ));
            }
            self.check_revision_advance(&head).await?;
            if head.envelope.body.operation == ObjectEnvelopeOperation::Delete {
                return Ok(None);
            }
            if head.revision <= item.revision {
                return result;
            }
        }
    }

    async fn load_calendar_record_once(
        &self,
        id: &str,
        source: bool,
        item: &ObjectListItem,
    ) -> Result<Option<ScheduleRecord>, ClientError> {
        if item.id.to_string() != id || item.kind != ObjectKind::Schedule {
            return Err(ClientError::InvalidArgument(
                "Import event identity mismatch".into(),
            ));
        }
        let key = self.current_encryption_key().await?;
        let (record, encrypted) = self
            .decrypt_schedule_object_item(&self.api, item, &key)
            .await?;
        if (source && record.as_source().is_none()) || (!source && record.as_ingested().is_none()) {
            return Err(ClientError::InvalidArgument(
                "Calendar object has the wrong record kind".into(),
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

    /// Verifies the target before purging it, tombstoned or not. A missing
    /// historical revision means the object is already gone. It is not
    /// permission to delete an object of some other kind.
    pub(super) async fn purge_import_object(
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
            let key = self.current_encryption_key().await?;
            let meta = decrypt_schedule_meta(
                &historical.meta_nonce,
                &historical.meta_ciphertext,
                &key,
                &historical.envelope.body,
            )?;
            let payload = single_payload(&historical)?;
            check_payload_ciphertext_size(payload, MAX_SCHEDULE_PAYLOAD_CIPHERTEXT_BYTES)?;
            let ciphertext = match self
                .api
                .download_object_revision_payload(
                    id,
                    1,
                    &payload.id.to_string(),
                    payload.ciphertext_size,
                )
                .await
            {
                Ok(ciphertext) => ciphertext,
                Err(ClientError::Api { status: 404, .. }) => return Ok(()),
                Err(error) => return Err(error),
            };
            verify_payload_hash(payload, &ciphertext)?;
            let aad = crypto::object_payload_aad(&historical.envelope.body, payload.id)?;
            let plaintext = crypto::decrypt(&key, &payload.nonce, &ciphertext, &aad)?;
            #[derive(serde::Deserialize)]
            struct ImportedEvent {
                record: String,
                source: SourceId,
                import: ObjectId,
                uid: String,
            }
            let event: ImportedEvent = serde_json::from_slice(&plaintext)
                .map_err(|error| ClientError::InvalidArgument(error.to_string()))?;
            if !(meta.record == ScheduleRecordKind::Ingested
                && event.record == "ingested"
                && event.source == source
                && event.import == batch
                && ObjectId::from(uuid::Uuid::new_v5(
                    &uuid::Uuid::from(batch),
                    event.uid.as_bytes(),
                ))
                .to_string()
                    == id)
            {
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
        loop {
            let item = match self.api.get_object(id).await {
                Ok(item) => item,
                Err(ClientError::Api { status: 404, .. }) => break,
                Err(error) => return Err(error),
            };
            verify_object_list_item_envelope(&item)?;
            if item.id.to_string() != id || item.kind != kind {
                return Err(ClientError::InvalidArgument(
                    "Import cleanup identity mismatch".into(),
                ));
            }
            match self.check_revision_advance(&item).await {
                Ok(()) => {}
                Err(error @ ClientError::RevisionRejected(_)) => {
                    let head = match self.api.get_object_head(id).await {
                        Ok(head) => head,
                        Err(ClientError::Api { status: 404, .. }) => break,
                        Err(error) => return Err(error),
                    };
                    verify_object_head_envelope(&head)?;
                    if head.id.to_string() != id
                        || head.kind != kind
                        || head.envelope.body.operation != ObjectEnvelopeOperation::Delete
                    {
                        return Err(error);
                    }
                    self.check_revision_advance(&head).await?;
                    break;
                }
                Err(error) => return Err(error),
            }
            let head = LocalHead {
                revision: item.envelope.body.revision,
                parent_hash: crypto::object_envelope_parent_hash(&item.envelope.body)?,
            };
            match self.write_tombstone_at(id, kind, Some(head)).await {
                Ok((seq, tombstone)) => {
                    let visible = self
                        .local_store
                        .apply_local_tombstone(kind, id, seq, &tombstone, RECENT_CLIPBOARD_LIMIT)
                        .await?;
                    self.publish_visible_state(visible).await;
                    break;
                }
                Err(ClientError::Api { status: 409, .. }) => continue,
                Err(ClientError::Api { status: 404, .. }) => break,
                Err(error) => return Err(error),
            }
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

    async fn track_retired_events(&self, id: &str) -> Result<(), ClientError> {
        let records = self.local_store.schedule_records_with_ids().await;
        let (mut source, mut head) = self.read_calendar_source(id).await?;
        loop {
            let mut changed = false;
            for (event_id, record) in &records {
                let Some(event) = record.as_ingested() else {
                    continue;
                };
                let Some(batch_id) = event.import else {
                    continue;
                };
                if event.source != source.id
                    || source
                        .active_import
                        .as_ref()
                        .is_some_and(|batch| batch.object_id == batch_id)
                    || source
                        .pending_imports
                        .iter()
                        .any(|batch| batch.object_id == batch_id)
                    || source.retired_imports.iter().any(|batch| {
                        batch.object_id == batch_id
                            && batch
                                .events
                                .iter()
                                .any(|event| event.to_string() == *event_id)
                    })
                {
                    continue;
                }
                let expected_id: ObjectId =
                    uuid::Uuid::new_v5(&uuid::Uuid::from(batch_id), event.uid.as_bytes()).into();
                if expected_id.to_string() != *event_id {
                    return Err(ClientError::InvalidArgument(
                        "Import cleanup event identity mismatch".into(),
                    ));
                }
                match self.api.get_object_revision(event_id, 1).await {
                    Ok(item) => verify_object_list_item_envelope(&item)?,
                    Err(ClientError::Api { status: 404, .. }) => continue,
                    Err(error) => return Err(error),
                }
                if let Some(batch) = source
                    .retired_imports
                    .iter_mut()
                    .find(|batch| batch.object_id == batch_id)
                {
                    batch.events.push(expected_id);
                } else {
                    source.retired_imports.push(RetiredImport {
                        object_id: batch_id,
                        events: vec![expected_id],
                    });
                }
                changed = true;
            }
            if !changed {
                return Ok(());
            }
            match self.save_calendar_source(id, &source, head).await {
                Ok(_) => return Ok(()),
                Err(ClientError::Api { status: 409, .. }) => {
                    (source, head) = self.read_calendar_source(id).await?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn cleanup_calendar_imports(&self, id: &str) -> Result<(), ClientError> {
        self.track_retired_events(id).await?;
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
                || source
                    .pending_imports
                    .iter()
                    .any(|pending| pending.object_id == batch.object_id)
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
        let cleaned = source.retired_imports.clone();
        let mut head = head;
        loop {
            source
                .retired_imports
                .retain(|batch| !cleaned.contains(batch));
            match self.save_calendar_source(id, &source, head).await {
                Ok(_) => break,
                Err(ClientError::Api { status: 409, .. }) => {
                    (source, head) = self.read_calendar_source(id).await?;
                    if !source
                        .retired_imports
                        .iter()
                        .any(|batch| cleaned.contains(batch))
                    {
                        break;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Hides the source's imported view first, then purges its batches.
    /// Actual records and locally authored plans and overrides are never
    /// cleanup targets.
    pub(super) async fn remove_calendar_imports(&self, id: &str) -> Result<(), ClientError> {
        let (mut source, head) = match self.calendar_source(id).await {
            Ok(value) => value,
            Err(ClientError::ItemNotFound { .. }) => return Ok(()),
            Err(error) => return Err(error),
        };
        if !source.pending_imports.is_empty() {
            return Err(ClientError::InvalidArgument(
                "Finish the pending calendar import before removing this source".into(),
            ));
        }
        if let Some(batch) = source.active_import.take() {
            source.retired_imports.push(batch.into());
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
                    .map(|batch| batch.object_id)
                    .chain(source.retired_imports.iter().map(|batch| batch.object_id))
                    .any(|batch_id| batch_id.to_string() == id)
            }))
    }
}

pub(super) fn owner_email(url: &url::Url) -> Option<String> {
    if !matches!(url.host_str()?, "calendar.google.com" | "www.google.com") {
        return None;
    }
    let path = url.path().strip_prefix("/calendar/ical/")?;
    let segment = path.split('/').next()?;
    let encoded = format!("email={}", segment.replace('+', "%2B"));
    let (_, email) = url::form_urlencoded::parse(encoded.as_bytes()).next()?;
    email.contains('@').then(|| email.to_ascii_lowercase())
}

fn validated_feed(
    text: &str,
    source: &CalendarSource,
) -> Result<clipper_schedule::IngestOutcome, ClientError> {
    let SourceKind::Ics { url } = &source.kind;
    let owner = url::Url::parse(url).ok().and_then(|url| owner_email(&url));
    let outcome = parse_calendar_feed(text, source.id, uuid::Uuid::nil().into(), owner.as_deref())?;
    let mut uids = HashSet::new();
    if !outcome.events.iter().all(|event| uids.insert(&event.uid)) {
        return Err(CalendarImportError::InvalidFeed(
            "Import contains duplicate event UIDs".into(),
        )
        .into());
    }
    if !outcome.skipped.is_empty() {
        return Err(CalendarImportError::InvalidFeed(format!(
            "Import was not replaced: {} event(s) could not be read. {}",
            outcome.skipped.len(),
            outcome
                .skipped
                .iter()
                .take(5)
                .map(|entry| entry.reason.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        ))
        .into());
    }
    Ok(outcome)
}

fn feed_hash(outcome: &clipper_schedule::IngestOutcome) -> Result<Vec<u8>, ClientError> {
    let mut events: Vec<_> = outcome.events.iter().collect();
    events.sort_by(|a, b| a.uid.cmp(&b.uid));
    let bytes = serde_json::to_vec(&(events, &outcome.rules))
        .map_err(|error| ClientError::InvalidArgument(error.to_string()))?;
    Ok(crypto::sha256(&bytes).to_vec())
}

fn check_record_size(record: &ScheduleRecord) -> Result<(), ClientError> {
    let bytes = serde_json::to_vec(record)
        .map_err(|error| ClientError::InvalidArgument(error.to_string()))?;
    if bytes.len() + 128 > MAX_SCHEDULE_PAYLOAD_CIPHERTEXT_BYTES as usize {
        return Err(CalendarImportError::RecordTooLarge.into());
    }
    Ok(())
}
