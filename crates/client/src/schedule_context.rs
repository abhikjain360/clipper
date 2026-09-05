//! Revision-aware schedule reads. Historical reads never advance a sync head.
use clipper_schedule::{ObjectRevisionRef, OccurrenceOverrideData, PlannedRef};

use super::*;

pub(super) type ScheduleSnapshot = Vec<(String, ScheduleRecord, LocalHead)>;

pub(super) fn revision_ref(id: &str, head: LocalHead) -> Result<ObjectRevisionRef, ClientError> {
    Ok(ObjectRevisionRef {
        object_id: id.parse().map_err(|source| ClientError::InvalidId {
            kind: "schedule object id",
            source,
        })?,
        revision: head.revision,
        body_hash: head.parent_hash,
    })
}

pub(super) fn series(record: &ScheduleRecord) -> Option<Cow<'_, ScheduleItem>> {
    match record {
        ScheduleRecord::Item(item) => Some(Cow::Borrowed(item)),
        ScheduleRecord::Ingested(event) => Some(Cow::Owned(ingested_as_series(event))),
        _ => None,
    }
}

fn invalid(message: &str) -> ClientError {
    ClientError::InvalidArgument(message.into())
}

pub(super) fn verify_pin(item: &ObjectListItem, pin: ObjectRevisionRef) -> Result<(), ClientError> {
    verify_object_list_item_envelope(item)?;
    if item.id != pin.object_id
        || item.revision != pin.revision
        || item.kind != ObjectKind::Schedule
        || crypto::object_envelope_parent_hash(&item.envelope.body)? != pin.body_hash
    {
        return Err(invalid(
            "The historical schedule revision does not match its recorded reference",
        ));
    }
    Ok(())
}

impl SyncEngine {
    /// Load exactly the accepted historical content, without treating it as a
    /// candidate current head. The cache is bounded, memory-only and session-scoped.
    pub(super) async fn schedule_revision(
        &self,
        pin: ObjectRevisionRef,
    ) -> Result<ScheduleRecord, ClientError> {
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let encryption_key = self.current_encryption_key().await?;
        if let Some(record) = self
            .schedule_history
            .lock()
            .await
            .get(&(epoch, pin))
            .cloned()
        {
            if self.history_epoch.load(Ordering::SeqCst) != epoch {
                return Err(ClientError::NotAuthenticated);
            }
            return Ok(record);
        }
        let current = self.local_store.schedule_records_with_heads().await?;
        let record = if let Some((_, record, _)) = current.iter().find(|(id, _, head)| {
            id == &pin.object_id.to_string()
                && head.revision == pin.revision
                && head.parent_hash == pin.body_hash
        }) {
            record.clone()
        } else {
            let item = self
                .api
                .get_object_revision(&pin.object_id.to_string(), pin.revision)
                .await?;
            verify_pin(&item, pin)?;
            let meta = decrypt_schedule_meta(
                &item.meta_nonce,
                &item.meta_ciphertext,
                &encryption_key,
                &item.envelope.body,
            )?;
            let payload = single_payload(&item)?;
            check_payload_ciphertext_size(payload, MAX_SCHEDULE_PAYLOAD_CIPHERTEXT_BYTES)?;
            let ciphertext = self
                .api
                .download_object_revision_payload(
                    &pin.object_id.to_string(),
                    pin.revision,
                    &payload.id.to_string(),
                    payload.ciphertext_size,
                )
                .await?;
            verify_payload_hash(payload, &ciphertext)?;
            let record = decrypt_schedule_payload(
                &payload.nonce,
                &ciphertext,
                &encryption_key,
                &item.envelope.body,
                payload.id,
            )?;
            if record.kind() != meta.record {
                return Err(invalid("Historical schedule metadata and payload disagree"));
            }
            record
        };
        // Do not repopulate decrypted history after logout or an account change.
        let active_key = self.encryption_key.read().await;
        if self.history_epoch.load(Ordering::SeqCst) != epoch
            || active_key.as_deref() != Some(&*encryption_key)
        {
            return Err(ClientError::NotAuthenticated);
        }
        let mut cache = self.schedule_history.lock().await;
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert((epoch, pin), record.clone());
        Ok(record)
    }

    pub(super) async fn effective_overrides(
        &self,
        item: &ScheduleItem,
        pin: ObjectRevisionRef,
        record: &ScheduleRecord,
        records: &ScheduleSnapshot,
    ) -> Result<Vec<(OccurrenceOverrideData, Option<ObjectRevisionRef>)>, ClientError> {
        let mut entries: Vec<_> = match record {
            ScheduleRecord::Ingested(event) => event
                .overrides
                .iter()
                .cloned()
                .map(|entry| (entry, None))
                .collect(),
            _ => Vec::new(),
        };
        let mut local_keys = HashSet::new();
        for (id, candidate, head) in records {
            let ScheduleRecord::Override(entry) = candidate else {
                continue;
            };
            if entry.base.object_id != pin.object_id {
                continue;
            }
            if entry.override_data.item != item.id {
                return Err(invalid(
                    "An override references a different schedule identity",
                ));
            }
            if !local_keys.insert(entry.override_data.recurrence_id) {
                return Err(invalid(
                    "Multiple local overrides target the same occurrence",
                ));
            }
            if entry.base != pin {
                let base = self.schedule_revision(entry.base).await?;
                if !series(&base).is_some_and(|base| base.overrides_compatible_with(item)) {
                    return Err(invalid(
                        "A schedule changed its timing or recurrence; its existing overrides need review",
                    ));
                }
            }
            entries.retain(|(old, _)| old.recurrence_id != entry.override_data.recurrence_id);
            entries.push((entry.override_data.clone(), Some(revision_ref(id, *head)?)));
        }
        Ok(entries)
    }

    /// Validate against one coherent local snapshot. A stale or fabricated UI
    /// context must not stop the existing timer or silently select another plan.
    pub(super) async fn validate_plan_context(
        &self,
        planned: &PlannedRef,
        records: &ScheduleSnapshot,
    ) -> Result<(), ClientError> {
        let (id, record, head) = records
            .iter()
            .find(|(id, _, _)| id == &planned.schedule.object_id.to_string())
            .ok_or_else(|| invalid("This plan is unavailable; refresh the calendar"))?;
        if revision_ref(id, *head)? != planned.schedule {
            return Err(invalid(
                "This plan changed; refresh the calendar before starting its timer",
            ));
        }
        if matches!(record, ScheduleRecord::Ingested(event) if event.status == IngestedStatus::Cancelled)
        {
            return Err(invalid("This occurrence has been cancelled"));
        }
        if let ScheduleRecord::Ingested(event) = record
            && !records.iter().any(|(_, record, _)| {
                record
                    .as_source()
                    .is_some_and(|source| source.id == event.source)
            })
        {
            return Err(invalid("This calendar source has been removed"));
        }
        let item =
            series(record).ok_or_else(|| invalid("The reference is not a schedule definition"))?;
        if item.id != planned.item {
            return Err(invalid("The occurrence belongs to a different schedule"));
        }
        let effective = self
            .effective_overrides(&item, planned.schedule, record, records)
            .await?;
        let applied = effective
            .iter()
            .find(|(entry, _)| entry.recurrence_id == planned.recurrence_id);
        if applied.and_then(|(_, pin)| *pin) != planned.override_revision {
            return Err(invalid(
                "This occurrence's override changed; refresh the calendar",
            ));
        }
        let overrides: Vec<_> = effective.into_iter().map(|(entry, _)| entry).collect();
        let expansion = Expansion {
            window: Window::new(planned.span.start, planned.span.end)
                .map_err(|e| invalid(&e.to_string()))?,
            observer: planned.observer,
        };
        let occurrences = RruleEngine::new()
            .occurrences(&item, &overrides, &expansion)
            .map_err(|e| invalid(&e.to_string()))?;
        if !occurrences
            .iter()
            .any(|entry| entry.recurrence_id == planned.recurrence_id && entry.span == planned.span)
        {
            return Err(invalid(
                "This occurrence no longer matches the displayed plan",
            ));
        }
        Ok(())
    }

    pub(super) async fn actual_title(&self, actual: &clipper_schedule::ActualRecord) -> String {
        let Some(planned) = actual.planned else {
            return UNPLANNED_TITLE.into();
        };
        match self.schedule_revision(planned.schedule).await {
            Ok(record) => record
                .planned_title()
                .filter(|(id, _)| *id == planned.item)
                .map(|(_, title)| title.to_string())
                .unwrap_or_else(|| "Historical plan unavailable".into()),
            Err(error) => {
                warn!(%error, "Could not load the actual's historical plan");
                "Historical plan unavailable".into()
            }
        }
    }

    /// Retrieve the immutable definition and override for an actual. The
    /// captured resolved span remains authoritative across travel/tzdb changes.
    pub async fn recorded_plan(
        &self,
        actual_id: &str,
    ) -> Result<Option<crate::schedule::RecordedPlan>, ClientError> {
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let records = self.local_store.schedule_records_with_ids().await;
        let actual = records
            .iter()
            .find_map(|(id, record)| match record {
                ScheduleRecord::Actual(actual) if id == actual_id => Some(actual),
                _ => None,
            })
            .ok_or_else(|| invalid("Actual record not found"))?;
        let Some(planned) = actual.planned else {
            return Ok(None);
        };
        let record = self.schedule_revision(planned.schedule).await?;
        let item = series(&record)
            .ok_or_else(|| invalid("Historical reference is not a schedule"))?
            .into_owned();
        if item.id != planned.item {
            return Err(invalid("Historical schedule identity mismatch"));
        }
        let override_data = if let Some(pin) = planned.override_revision {
            let ScheduleRecord::Override(entry) = self.schedule_revision(pin).await? else {
                return Err(invalid("Historical override reference is not an override"));
            };
            let base = self.schedule_revision(entry.base).await?;
            if entry.base.object_id != planned.schedule.object_id
                || !series(&base).is_some_and(|base| base.overrides_compatible_with(&item))
                || entry.override_data.item != planned.item
                || entry.override_data.recurrence_id != planned.recurrence_id
            {
                return Err(invalid(
                    "Historical override does not apply to the recorded plan",
                ));
            }
            Some(entry.override_data)
        } else if let ScheduleRecord::Ingested(event) = record {
            event
                .overrides
                .into_iter()
                .find(|entry| entry.recurrence_id == planned.recurrence_id)
        } else {
            None
        };
        if self.history_epoch.load(Ordering::SeqCst) != epoch {
            return Err(ClientError::NotAuthenticated);
        }
        Ok(Some(crate::schedule::RecordedPlan {
            item,
            override_data,
            context: planned,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cached_history_cannot_cross_a_session_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:1", dir.path());
        *engine.encryption_key.write().await = Some(Zeroizing::new([1; 32]));
        let pin = ObjectRevisionRef {
            object_id: uuid::Uuid::new_v4().into(),
            revision: 1,
            body_hash: [2; 32],
        };
        let record = ScheduleRecord::Source(Box::new(CalendarSource {
            id: SourceId::new(),
            name: "First account's private calendar".into(),
            kind: SourceKind::Ics {
                url: "https://example.invalid/private".into(),
            },
            enabled: true,
        }));
        engine
            .schedule_history
            .lock()
            .await
            .insert((0, pin), record);
        assert_eq!(
            engine
                .schedule_revision(pin)
                .await
                .unwrap()
                .as_source()
                .unwrap()
                .name,
            "First account's private calendar"
        );
        // Even if an in-flight old request left a cache entry behind, the next
        // session cannot retrieve it. No API token is installed in this test.
        engine.history_epoch.fetch_add(1, Ordering::SeqCst);
        *engine.encryption_key.write().await = Some(Zeroizing::new([3; 32]));
        assert!(matches!(
            engine.schedule_revision(pin).await,
            Err(ClientError::NotAuthenticated)
        ));
    }
}
