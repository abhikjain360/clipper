use chrono::{NaiveDateTime, TimeDelta};
use clipper_schedule::{
    BlockDuration, OccurrenceOverrideData, OverrideChange, OverrideId, RecurrenceId, TimedStart,
};

use super::*;

enum Change {
    Cancel,
    Move {
        to: NaiveDateTime,
        duration: Option<BlockDuration>,
    },
    Restore,
}

impl SyncEngine {
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
        let recurrence_id = crate::schedule::parse_occurrence_key(occurrence_key)
            .ok_or_else(|| invalid("Invalid occurrence key"))?;
        let _write = self.calendar_write.lock().await;
        self.credentials_for_session(epoch).await?;
        let records = self.local_store.schedule_records_with_heads().await?;
        let (_, record, head) = records
            .iter()
            .find(|(id, _, _)| id == object_id)
            .ok_or_else(|| ClientError::ItemNotFound {
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
            for (id, _, _) in existing {
                self.tombstone_schedule_object(id).await?;
            }
            return Ok(());
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
        let entry = OccurrenceOverride {
            base,
            override_data: OccurrenceOverrideData {
                id: existing
                    .first()
                    .map(|(_, entry, _)| entry.override_data.id)
                    .unwrap_or_else(OverrideId::new),
                item: item.id,
                recurrence_id,
                change,
            },
        };
        let (id, placement) = match existing.first() {
            Some((id, _, head)) => ((*id).clone(), EnvelopePlacement::Revise(**head)),
            None => (uuid::Uuid::now_v7().to_string(), EnvelopePlacement::Create),
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
