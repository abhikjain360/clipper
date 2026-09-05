//! The recurrence bake-off corpus, kept as permanent tests.
//!
//! These thirteen cadences come from abnormalarm's `NextOccurrenceTest`, the
//! alarm app whose behaviour Clipper has to reproduce. They were used to choose
//! between `rrule` and `calcard`: both engines pass all thirteen, and they
//! diverge only at a DST gap, which is the fourteenth case below.
//!
//! Do not delete a case because it looks redundant with another. `monthday_31`
//! and `monthly_31_skip` differ only in an explicit `INTERVAL=1`, and that is
//! exactly the kind of difference an engine swap breaks.

use chrono::{Month, NaiveDateTime, TimeDelta, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use clipper_schedule::{
    BlockDuration, Cadence, Frequency, MonthDay, MonthlyRule, NthWeekday, Recurrence,
    RecurrenceEnd, RecurrenceEngine, RruleEngine, ScheduleItem, ScheduleItemId, ScheduleSpan,
    TimedStart, WeekdaySet,
};

/// Five years is generous enough for the longest gap in the corpus (a leap-day
/// yearly rule jumping 2026 to 2028) and small enough to stay well inside the
/// engine's candidate ceiling.
const LOOKAHEAD_DAYS: i64 = 365 * 5;

fn item(dtstart: &str, recurrence: Recurrence) -> ScheduleItem {
    let local = NaiveDateTime::parse_from_str(dtstart, "%Y%m%dT%H%M%S").expect("valid DTSTART");
    ScheduleItem {
        id: ScheduleItemId::new(),
        title: "corpus".to_string(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local,
                zone: Tz::UTC,
            },
            duration: BlockDuration::from_minutes(30).expect("30 is non-zero"),
        },
        recurrence,
        reference: None,
    }
}

/// The first occurrence strictly after `after`, formatted as the corpus writes
/// it, or `None` if the series is exhausted.
fn next_after(item: &ScheduleItem, after: (i32, u32, u32, u32, u32)) -> Option<String> {
    let after = Utc
        .with_ymd_and_hms(after.0, after.1, after.2, after.3, after.4, 0)
        .single()
        .expect("unambiguous UTC instant");
    RruleEngine::new()
        .next_after(item, &[], after, TimeDelta::days(LOOKAHEAD_DAYS), Tz::UTC)
        .expect("expansion succeeds")
        .map(|occurrence| occurrence.span.start.format("%Y-%m-%dT%H:%M").to_string())
}

fn daily(interval: u32) -> Recurrence {
    Recurrence::Every(Cadence::every(Frequency::Daily, interval).expect("non-zero interval"))
}

fn monthly(rule: MonthlyRule, interval: u32) -> Recurrence {
    Recurrence::Every(
        Cadence::every(Frequency::Monthly(rule), interval).expect("non-zero interval"),
    )
}

#[test]
fn every_2_days() {
    let item = item("20260610T080000", daily(2));
    assert_eq!(
        next_after(&item, (2026, 6, 12, 10, 0)).as_deref(),
        Some("2026-06-14T08:00")
    );
}

#[test]
fn weekdays_mwf() {
    let item = item(
        "20260601T070000",
        Recurrence::Every(Cadence::each(Frequency::Weekly {
            weekdays: WeekdaySet::new(&[Weekday::Mon, Weekday::Wed, Weekday::Fri])
                .expect("non-empty"),
            week_start: Weekday::Mon,
        })),
    );
    assert_eq!(
        next_after(&item, (2026, 6, 12, 10, 0)).as_deref(),
        Some("2026-06-15T07:00")
    );
}

#[test]
fn every_2_weeks_tue() {
    let item = item(
        "20260609T070000",
        Recurrence::Every(
            Cadence::every(
                Frequency::Weekly {
                    weekdays: WeekdaySet::just(Weekday::Tue),
                    week_start: Weekday::Mon,
                },
                2,
            )
            .expect("non-zero interval"),
        ),
    );
    assert_eq!(
        next_after(&item, (2026, 6, 12, 10, 0)).as_deref(),
        Some("2026-06-23T07:00")
    );
}

#[test]
fn monthday_31() {
    let item = item(
        "20260131T080000",
        monthly(
            MonthlyRule::OnDay(MonthDay::from_start(31).expect("in range")),
            1,
        ),
    );
    assert_eq!(
        next_after(&item, (2026, 2, 1, 0, 0)).as_deref(),
        Some("2026-03-31T08:00")
    );
}

#[test]
fn second_tuesday() {
    let item = item(
        "20260113T070000",
        monthly(
            MonthlyRule::OnWeekday {
                nth: NthWeekday::from_start(2).expect("in range"),
                weekday: Weekday::Tue,
            },
            1,
        ),
    );
    assert_eq!(
        next_after(&item, (2026, 6, 12, 10, 0)).as_deref(),
        Some("2026-07-14T07:00")
    );
}

#[test]
fn last_friday() {
    let item = item(
        "20260130T070000",
        monthly(
            MonthlyRule::OnWeekday {
                nth: NthWeekday::last(),
                weekday: Weekday::Fri,
            },
            1,
        ),
    );
    assert_eq!(
        next_after(&item, (2026, 6, 12, 10, 0)).as_deref(),
        Some("2026-06-26T07:00")
    );
}

#[test]
fn every_3_months_15() {
    let item = item(
        "20260115T080000",
        monthly(
            MonthlyRule::OnDay(MonthDay::from_start(15).expect("in range")),
            3,
        ),
    );
    assert_eq!(
        next_after(&item, (2026, 6, 12, 10, 0)).as_deref(),
        Some("2026-07-15T08:00")
    );
}

/// Same rule as `monthday_31`, reached by the explicit-interval path. The two
/// generate different RRULE text and must not diverge.
#[test]
fn monthly_31_skip() {
    let item = item(
        "20260131T080000",
        monthly(
            MonthlyRule::OnDay(MonthDay::from_start(31).expect("in range")),
            1,
        ),
    );
    assert_eq!(
        next_after(&item, (2026, 2, 1, 0, 0)).as_deref(),
        Some("2026-03-31T08:00")
    );
}

#[test]
fn yearly_feb29() {
    let item = item(
        "20240229T080000",
        Recurrence::Every(Cadence::each(Frequency::Yearly {
            month: Month::February,
            day: MonthDay::from_start(29).expect("in range"),
        })),
    );
    assert_eq!(
        next_after(&item, (2026, 3, 1, 0, 0)).as_deref(),
        Some("2028-02-29T08:00")
    );
}

#[test]
fn last_day_month() {
    let item = item(
        "20260131T080000",
        monthly(
            MonthlyRule::OnDay(MonthDay::from_end(1).expect("in range")),
            1,
        ),
    );
    assert_eq!(
        next_after(&item, (2026, 6, 12, 10, 0)).as_deref(),
        Some("2026-06-30T08:00")
    );
}

#[test]
fn second_last_day() {
    let item = item(
        "20260130T080000",
        monthly(
            MonthlyRule::OnDay(MonthDay::from_end(2).expect("in range")),
            1,
        ),
    );
    assert_eq!(
        next_after(&item, (2027, 2, 1, 0, 0)).as_deref(),
        Some("2027-02-27T08:00")
    );
}

#[test]
fn count_3_exhausted() {
    let item = item(
        "20260101T080000",
        Recurrence::Every(
            Cadence::each(Frequency::Daily).ending(RecurrenceEnd::after(3).expect("non-zero")),
        ),
    );
    assert_eq!(next_after(&item, (2026, 6, 12, 10, 0)), None);
}

#[test]
fn until_past() {
    let until = Utc
        .with_ymd_and_hms(2026, 6, 12, 0, 0, 0)
        .single()
        .expect("unambiguous");
    let item = item(
        "20260105T080000",
        Recurrence::Every(
            Cadence::each(Frequency::Weekly {
                weekdays: WeekdaySet::just(Weekday::Mon),
                week_start: Weekday::Mon,
            })
            .ending(RecurrenceEnd::On(until)),
        ),
    );
    assert_eq!(next_after(&item, (2026, 6, 12, 10, 0)), None);
}

/// Case 14. 2026-03-29 02:30 does not exist in Europe/Berlin — the clocks jump
/// 02:00 to 03:00.
///
/// `rrule` shifts the occurrence forward to 03:30, which is what `java.time`
/// does and therefore what abnormalarm has been doing on this device for
/// months. `calcard` instead emits the nonexistent 02:30 as a floating time.
/// This test is the reason `rrule` was chosen; if it ever starts failing, the
/// alarm path has changed behaviour and the engine choice needs revisiting.
#[test]
fn dst_gap_shifts_forward_like_java() {
    let local =
        NaiveDateTime::parse_from_str("20260101T023000", "%Y%m%dT%H%M%S").expect("valid DTSTART");
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "dst gap".to_string(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local,
                zone: Tz::Europe__Berlin,
            },
            duration: BlockDuration::from_minutes(30).expect("30 is non-zero"),
        },
        recurrence: daily(1),
        reference: None,
    };

    let after = Tz::Europe__Berlin
        .with_ymd_and_hms(2026, 3, 28, 12, 0, 0)
        .single()
        .expect("unambiguous")
        .with_timezone(&Utc);

    let next = RruleEngine::new()
        .next_after(&item, &[], after, TimeDelta::days(7), Tz::Europe__Berlin)
        .expect("expansion succeeds")
        .expect("a daily rule has an occurrence within a week");

    assert_eq!(
        next.span
            .start
            .with_timezone(&Tz::Europe__Berlin)
            .format("%Y-%m-%dT%H:%M")
            .to_string(),
        "2026-03-29T03:30",
        "the gap occurrence must shift forward, not vanish or stay at 02:30"
    );
}
