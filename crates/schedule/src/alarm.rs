//! Turning occurrences into alarms.
//!
//! The division of labour with the Android side is deliberate: this crate
//! decides *when* an alarm should ring, and the platform decides *how* to make
//! it ring. Android never recomputes a recurrence — it receives concrete
//! instants and registers each as a one-shot exact alarm.
//!
//! Keeping recurrence expansion here gives calendar views and platform alarms
//! the same occurrence times, including timezone and DST handling.

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::item::{Occurrence, RecurrenceId, ScheduleItem, ScheduleItemId};

/// When a block should raise an alarm.
///
/// Absent means silent, which is the default: a planner is mostly a record of
/// intent, and most blocks should not wake anyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlarmPolicy {
    /// Minutes before the occurrence starts. Zero rings at the start.
    pub minutes_before: u32,
}

impl AlarmPolicy {
    /// Rings exactly when the block begins.
    pub fn at_start() -> Self {
        Self { minutes_before: 0 }
    }

    pub fn minutes_before(minutes: u32) -> Self {
        Self {
            minutes_before: minutes,
        }
    }

    fn lead(self) -> TimeDelta {
        TimeDelta::minutes(i64::from(self.minutes_before))
    }
}

/// One alarm the platform should register.
///
/// Carries everything the ring screen needs, because it has to work before the
/// device is unlocked — at which point nothing can be looked up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedAlarm {
    pub item: ScheduleItemId,
    /// Which occurrence this belongs to, so a dismissal can be recorded against
    /// the right one.
    pub recurrence_id: RecurrenceId,
    /// What the ring screen shows. Denormalized on purpose: before unlock there
    /// is no encrypted store to read a title from.
    pub label: String,
    /// When the alarm rings.
    pub fire_at: DateTime<Utc>,
    /// When the block itself starts, which differs from `fire_at` whenever the
    /// policy has a lead time.
    pub occurrence_start: DateTime<Utc>,
}

/// Alarms for `item`'s occurrences that have not already passed, soonest first.
///
/// `now` is a parameter rather than read from the clock so this stays pure and
/// testable, and so a caller can plan a window deliberately.
pub fn plan_alarms(
    item: &ScheduleItem,
    occurrences: &[Occurrence],
    now: DateTime<Utc>,
) -> Vec<PlannedAlarm> {
    let Some(policy) = item.alarm else {
        return Vec::new();
    };
    let mut planned: Vec<PlannedAlarm> = occurrences
        .iter()
        .filter(|occurrence| occurrence.item == item.id)
        .filter_map(|occurrence| {
            let fire_at = occurrence.span.start() - policy.lead();
            // An alarm whose moment has passed is not rescheduled. Ringing late
            // for something that already started is noise, not a reminder.
            (fire_at > now).then_some(PlannedAlarm {
                item: occurrence.item,
                recurrence_id: occurrence.recurrence_id,
                label: item.title.clone(),
                fire_at,
                occurrence_start: occurrence.span.start(),
            })
        })
        .collect();
    planned.sort_by_key(|alarm| alarm.fire_at);
    planned
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDateTime, TimeZone};
    use chrono_tz::Tz;

    use super::*;
    use crate::{
        TimeRange,
        engine::{Expansion, RecurrenceEngine, RruleEngine},
        recurrence::{Cadence, Frequency, Recurrence, WeekdaySet},
        time::{BlockDuration, ScheduleSpan, TimedStart},
    };

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0)
            .single()
            .expect("unambiguous")
    }

    fn gym(alarm: Option<AlarmPolicy>) -> ScheduleItem {
        ScheduleItem {
            id: ScheduleItemId::new(),
            title: "Gym".to_string(),
            span: ScheduleSpan::Timed {
                start: TimedStart::Floating(
                    NaiveDateTime::parse_from_str("20260907T070000", "%Y%m%dT%H%M%S")
                        .expect("valid"),
                ),
                duration: BlockDuration::from_minutes(45).expect("non-zero"),
            },
            recurrence: Recurrence::Every(Cadence::each(Frequency::Weekly {
                weekdays: WeekdaySet::weekdays(),
            })),
            reference: None,
            alarm,
        }
    }

    fn week_of(item: &ScheduleItem) -> Vec<Occurrence> {
        RruleEngine::new()
            .occurrences(
                item,
                &[],
                &Expansion {
                    window: TimeRange::new(utc(2026, 9, 7, 0, 0), utc(2026, 9, 14, 0, 0))
                        .expect("window"),
                    observer: Tz::UTC,
                },
            )
            .expect("expansion")
    }

    #[test]
    fn a_block_without_a_policy_raises_nothing() {
        let item = gym(None);
        assert!(plan_alarms(&item, &week_of(&item), utc(2026, 9, 6, 0, 0)).is_empty());
    }

    #[test]
    fn an_alarm_rings_at_the_start_by_default() {
        let item = gym(Some(AlarmPolicy::at_start()));
        let planned = plan_alarms(&item, &week_of(&item), utc(2026, 9, 6, 0, 0));
        assert_eq!(planned.len(), 5, "Monday to Friday");
        assert_eq!(planned[0].fire_at, utc(2026, 9, 7, 7, 0));
        assert_eq!(planned[0].occurrence_start, utc(2026, 9, 7, 7, 0));
        assert_eq!(planned[0].label, "Gym");
    }

    #[test]
    fn a_lead_time_moves_the_ring_earlier_but_not_the_block() {
        let item = gym(Some(AlarmPolicy::minutes_before(15)));
        let planned = plan_alarms(&item, &week_of(&item), utc(2026, 9, 6, 0, 0));
        assert_eq!(planned[0].fire_at, utc(2026, 9, 7, 6, 45));
        assert_eq!(
            planned[0].occurrence_start,
            utc(2026, 9, 7, 7, 0),
            "the block itself does not move"
        );
    }

    /// Ringing for something that already started is noise, not a reminder.
    #[test]
    fn alarms_already_past_are_dropped() {
        let item = gym(Some(AlarmPolicy::at_start()));
        let planned = plan_alarms(&item, &week_of(&item), utc(2026, 9, 9, 12, 0));
        assert_eq!(planned.len(), 2, "Thursday and Friday remain");
        assert_eq!(planned[0].fire_at, utc(2026, 9, 10, 7, 0));
    }

    #[test]
    fn alarms_come_back_soonest_first() {
        let item = gym(Some(AlarmPolicy::at_start()));
        let planned = plan_alarms(&item, &week_of(&item), utc(2026, 9, 6, 0, 0));
        let mut sorted = planned.clone();
        sorted.sort_by_key(|alarm| alarm.fire_at);
        assert_eq!(planned, sorted);
    }

    /// The label travels with the alarm because the ring screen may run before
    /// the device is unlocked, when nothing can be decrypted.
    #[test]
    fn the_label_is_carried_not_looked_up() {
        let mut item = gym(Some(AlarmPolicy::at_start()));
        item.title = "Take medication".to_string();
        let planned = plan_alarms(&item, &week_of(&item), utc(2026, 9, 6, 0, 0));
        assert!(planned.iter().all(|alarm| alarm.label == "Take medication"));
    }
}
