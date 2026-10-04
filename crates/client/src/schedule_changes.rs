use chrono::{NaiveDateTime, TimeDelta};
use clipper_schedule::{
    BlockDuration, OccurrenceOverrideData, OverrideChange, OverrideId, RecurrenceId, TimedStart,
};

use super::*;

#[derive(Clone, Copy)]
enum Change {
    Cancel,
    Move {
        to: NaiveDateTime,
        duration: Option<BlockDuration>,
    },
    Restore,
}

impl SyncEngine {
    pub(super) async fn remove_occurrence_overrides(
        &self,
        object_id: &str,
        occurrence_key: Option<&str>,
    ) -> Result<(), ClientError> {
        let object_id: ObjectId = object_id.parse().map_err(|source| ClientError::InvalidId {
            kind: "schedule object id",
            source,
        })?;
        loop {
            let records = self.local_store.schedule_records_with_ids().await;
            let ids: Vec<_> = records
                .into_iter()
                .filter_map(|(id, record)| match record {
                    ScheduleRecord::Override(entry)
                        if entry.base.object_id == object_id
                            && occurrence_key.is_none_or(|key| {
                                crate::schedule::occurrence_key(&entry.override_data.recurrence_id)
                                    == key
                            }) =>
                    {
                        Some(id)
                    }
                    _ => None,
                })
                .collect();
            if ids.is_empty() {
                return Ok(());
            }
            for id in ids {
                match self.tombstone_override(&id).await {
                    Ok(()) | Err(ClientError::Api { status: 409, .. }) => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }

    async fn tombstone_override(&self, id: &str) -> Result<(), ClientError> {
        match self.tombstone_schedule_object(id).await {
            Err(error @ ClientError::Api { status: 400, .. })
                if matches!(&error, ClientError::Api { error, .. }
                    if error.code == ApiErrorCode::ObjectDeleteUnsupported) =>
            {
                let epoch = self.history_epoch.load(Ordering::SeqCst);
                let credentials = self.credentials_for_session(epoch).await?;
                let head = credentials.api.get_object_head(id).await?;
                if head.id.to_string() != id
                    || head.kind != ObjectKind::Schedule
                    || head.envelope.body.operation != ObjectEnvelopeOperation::Delete
                {
                    return Err(error);
                }
                verify_object_head_envelope(&head)?;
                self.accept_recovered_head(
                    epoch,
                    &credentials.api,
                    &credentials.encryption_key,
                    &head,
                )
                .await
            }
            result => result,
        }
    }

    pub async fn cancel_occurrence(
        &self,
        object_id: &str,
        occurrence_key: &str,
    ) -> Result<(), ClientError> {
        self.run_work(
            Some("Cancelling an occurrence".into()),
            self.change_occurrence(object_id, occurrence_key, Change::Cancel),
        )
        .await
    }

    pub async fn move_occurrence(
        &self,
        object_id: &str,
        occurrence_key: &str,
        to: NaiveDateTime,
        duration: Option<BlockDuration>,
    ) -> Result<(), ClientError> {
        self.run_work(
            Some("Moving an occurrence".into()),
            self.change_occurrence(object_id, occurrence_key, Change::Move { to, duration }),
        )
        .await
    }

    pub async fn restore_occurrence(
        &self,
        object_id: &str,
        occurrence_key: &str,
    ) -> Result<(), ClientError> {
        self.run_work(
            Some("Restoring an occurrence".into()),
            self.change_occurrence(object_id, occurrence_key, Change::Restore),
        )
        .await
    }

    async fn change_occurrence(
        &self,
        object_id: &str,
        occurrence_key: &str,
        change: Change,
    ) -> Result<(), ClientError> {
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let _write = self.calendar_write.lock().await;
        loop {
            match self
                .change_occurrence_inner(epoch, object_id, occurrence_key, change)
                .await
            {
                Err(ClientError::Api { status: 409, .. }) => {}
                result => return result,
            }
        }
    }

    async fn change_occurrence_inner(
        &self,
        epoch: u64,
        object_id: &str,
        occurrence_key: &str,
        change: Change,
    ) -> Result<(), ClientError> {
        let recurrence_id = crate::schedule::parse_occurrence_key(occurrence_key)
            .ok_or_else(|| invalid("Invalid occurrence key"))?;
        self.credentials_for_session(epoch).await?;
        let records = self.local_store.schedule_records_with_heads().await?;
        let stored = records.iter().find(|(id, _, _)| id == object_id);
        if matches!(change, Change::Restore) && stored.is_none() {
            return self
                .remove_occurrence_overrides(object_id, Some(occurrence_key))
                .await;
        }
        let (_, record, head) = stored.ok_or_else(|| ClientError::ItemNotFound {
            id: object_id.into(),
        })?;
        let ScheduleRecord::Item(item) = record else {
            return Err(invalid(
                "Occurrence changes require a locally authored schedule item",
            ));
        };
        let base = revision_ref(object_id, *head)?;
        let existing: Vec<_> = records
            .iter()
            .filter_map(|(id, record, head)| match record {
                ScheduleRecord::Override(entry)
                    if entry.base.object_id == base.object_id
                        && crate::schedule::occurrence_key(&entry.override_data.recurrence_id)
                            == occurrence_key =>
                {
                    Some((id, entry, head))
                }
                _ => None,
            })
            .collect();
        if matches!(change, Change::Restore) && !existing.is_empty() {
            return self
                .remove_occurrence_overrides(object_id, Some(occurrence_key))
                .await;
        }
        let start = match recurrence_id {
            RecurrenceId::Floating(local) => TimedStart::Floating(local)
                .resolve(chrono_tz::UTC)
                .map_err(|error| invalid(&error.to_string()))?,
            RecurrenceId::Instant(instant) => instant,
            RecurrenceId::Date(date) => TimedStart::Floating(
                date.and_hms_opt(0, 0, 0)
                    .ok_or_else(|| invalid("Invalid occurrence date"))?,
            )
            .resolve(chrono_tz::UTC)
            .map_err(|error| invalid(&error.to_string()))?,
        };
        let end = start
            .checked_add_signed(TimeDelta::seconds(1))
            .ok_or_else(|| invalid("Occurrence date is out of range"))?;
        let window = TimeRange::new(start, end).map_err(|error| invalid(&error.to_string()))?;
        let occurrences = self
            .recurrence_engine(&item.recurrence)
            .await?
            .occurrences(
                item,
                &[],
                &Expansion {
                    window,
                    observer: chrono_tz::UTC,
                },
            )
            .map_err(|error| invalid(&error.to_string()))?;
        let recurrence_id = occurrences
            .iter()
            .find(|entry| crate::schedule::occurrence_key(&entry.recurrence_id) == occurrence_key)
            .ok_or_else(|| invalid("The occurrence key does not belong to this schedule item"))?
            .recurrence_id;
        if matches!(change, Change::Restore) {
            return Ok(());
        }
        self.effective_overrides(item, base, record, &records)
            .await?;
        let change = match change {
            Change::Cancel => OverrideChange::Cancelled,
            Change::Move { to, duration } => {
                let ScheduleSpan::Timed {
                    start,
                    duration: original_duration,
                } = &item.span
                else {
                    return Err(invalid("Moving an occurrence requires a timed block"));
                };
                let current_duration = existing
                    .first()
                    .and_then(|(_, entry, _)| match &entry.override_data.change {
                        OverrideChange::Rescheduled(ScheduleSpan::Timed { duration, .. }) => {
                            Some(*duration)
                        }
                        _ => None,
                    })
                    .unwrap_or(*original_duration);
                let start = match start {
                    TimedStart::Floating(_) => TimedStart::Floating(to),
                    TimedStart::Zoned { zone, .. } => TimedStart::Zoned {
                        local: to,
                        zone: *zone,
                    },
                };
                let span = ScheduleSpan::Timed {
                    start,
                    duration: duration.unwrap_or(current_duration),
                };
                span.resolve(chrono_tz::UTC)
                    .map_err(|error| invalid(&error.to_string()))?;
                OverrideChange::Rescheduled(span)
            }
            Change::Restore => unreachable!(),
        };
        const NAMESPACE: uuid::Uuid =
            uuid::Uuid::from_u128(0xc73e_4452_1ef7_561c_98b5_a71b_386c_65d8);
        let override_id = uuid::Uuid::new_v5(
            &NAMESPACE,
            format!(
                "{}:{}",
                base.object_id,
                crate::schedule::occurrence_key(&recurrence_id)
            )
            .as_bytes(),
        );
        let id = override_id.to_string();
        for (other, _, _) in existing.iter().filter(|(other, _, _)| **other != id) {
            self.tombstone_override(other).await?;
        }
        let entry = OccurrenceOverride {
            base,
            override_data: OccurrenceOverrideData {
                id: OverrideId(override_id),
                item: item.id,
                recurrence_id,
                change,
            },
        };
        let head = match records.iter().find(|(object_id, _, _)| object_id == &id) {
            Some((_, _, head)) => Some(*head),
            None => self.local_store.local_head(&id).await?,
        };
        let placement = match head {
            Some(head) => EnvelopePlacement::Revise(head),
            None => EnvelopePlacement::Create,
        };
        self.write_schedule_record_for_session(
            epoch,
            &id,
            ScheduleRecord::Override(Box::new(entry)),
            placement,
        )
        .await?;
        Ok(())
    }
}

fn invalid(message: &str) -> ClientError {
    ClientError::InvalidArgument(message.into())
}
