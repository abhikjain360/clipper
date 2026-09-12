//! An instant `UNTIL` cuts on the instant, not on a wall clock.
//!
//! Expansion walks wall-clock time, so around a DST change the wall-clock
//! comparison and the instant comparison disagree. A DATE `UNTIL` keeps
//! wall-clock meaning and covers its whole day.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use clipper_schedule::{
    BlockDuration, Cadence, Expansion, Frequency, ImportedRuleResolver, Recurrence, RecurrenceEnd,
    RecurrenceEngine, RruleEngine, ScheduleItem, ScheduleItemId, ScheduleSpan, TimeRange,
    TimedStart,
};

fn local(text: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(text, "%Y%m%dT%H%M%S").expect("valid local datetime")
}

fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0)
        .single()
        .expect("unambiguous UTC instant")
}

fn berlin_span(start: &str) -> ScheduleSpan {
    ScheduleSpan::Timed {
        start: TimedStart::Zoned {
            local: local(start),
            zone: Tz::Europe__Berlin,
        },
        duration: BlockDuration::from_minutes(30).expect("non-zero"),
    }
}

fn cadence_item(start: &str, end: RecurrenceEnd) -> ScheduleItem {
    ScheduleItem {
        id: ScheduleItemId::new(),
        title: "until".to_string(),
        span: berlin_span(start),
        recurrence: Recurrence::Every(Cadence::each(Frequency::Daily).ending(end)),
        reference: None,
        alarm: None,
    }
}

fn imported_item(start: &str, rule: &str) -> (ScheduleItem, ImportedRuleResolver) {
    let import = clipper_api_types::ObjectId::from(uuid::Uuid::from_u128(0x5150));
    let uid = "until@example.com";
    let mut rules = ImportedRuleResolver::new();
    rules.insert(import, uid, rule).expect("the rule is valid");
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "until".to_string(),
        span: berlin_span(start),
        recurrence: Recurrence::Imported {
            import,
            uid: uid.to_string(),
        },
        reference: None,
        alarm: None,
    };
    (item, rules)
}

fn expand(
    item: &ScheduleItem,
    expansion: &Expansion,
) -> Result<Vec<DateTime<Utc>>, clipper_schedule::EngineError> {
    Ok(RruleEngine::new()
        .occurrences(item, &[], expansion)?
        .into_iter()
        .map(|occurrence| occurrence.span.start())
        .collect())
}

fn expand_imported(
    item: &ScheduleItem,
    rules: ImportedRuleResolver,
    expansion: &Expansion,
) -> Result<Vec<DateTime<Utc>>, clipper_schedule::EngineError> {
    Ok(RruleEngine::with_imported_rules(rules)
        .occurrences(item, &[], expansion)?
        .into_iter()
        .map(|occurrence| occurrence.span.start())
        .collect())
}

fn fall_back_expansion() -> Expansion {
    Expansion {
        window: TimeRange::new(utc(2026, 10, 23, 0, 0), utc(2026, 10, 28, 0, 0))
            .expect("non-empty window"),
        observer: Tz::Europe__Berlin,
    }
}

fn spring_forward_expansion() -> Expansion {
    Expansion {
        window: TimeRange::new(utc(2026, 3, 27, 0, 0), utc(2026, 3, 31, 0, 0))
            .expect("non-empty window"),
        observer: Tz::Europe__Berlin,
    }
}

/// Oct 25 02:30 Berlin is in the fall-back fold and takes the earlier instant,
/// 00:30Z, which is before the 01:15Z cutoff, so Oct 25 stays in.
#[test]
fn instant_until_includes_fold_day_before_the_cutoff() {
    let item = cadence_item(
        "20261024T023000",
        RecurrenceEnd::On(utc(2026, 10, 25, 1, 15)),
    );
    let starts = expand(&item, &fall_back_expansion()).expect("expands");
    assert_eq!(
        starts,
        vec![utc(2026, 10, 24, 0, 30), utc(2026, 10, 25, 0, 30)]
    );
}

/// The same fold case through an imported rule string.
#[test]
fn imported_instant_until_includes_fold_day_before_the_cutoff() {
    let (item, rules) = imported_item("20261024T023000", "FREQ=DAILY;UNTIL=20261025T011500Z");
    let starts = expand_imported(&item, rules, &fall_back_expansion()).expect("expands");
    assert_eq!(
        starts,
        vec![utc(2026, 10, 24, 0, 30), utc(2026, 10, 25, 0, 30)]
    );
}

/// Mar 29 02:30 Berlin is in the spring-forward gap and shifts to 03:30 CEST,
/// 01:30Z, which is after the 01:15Z cutoff, so Mar 29 stays out.
#[test]
fn instant_until_excludes_gap_day_after_the_cutoff() {
    let item = cadence_item(
        "20260328T023000",
        RecurrenceEnd::On(utc(2026, 3, 29, 1, 15)),
    );
    let starts = expand(&item, &spring_forward_expansion()).expect("expands");
    assert_eq!(starts, vec![utc(2026, 3, 28, 1, 30)]);
}

/// The same gap case through an imported rule string.
#[test]
fn imported_instant_until_excludes_gap_day_after_the_cutoff() {
    let (item, rules) = imported_item("20260328T023000", "FREQ=DAILY;UNTIL=20260329T011500Z");
    let starts = expand_imported(&item, rules, &spring_forward_expansion()).expect("expands");
    assert_eq!(starts, vec![utc(2026, 3, 28, 1, 30)]);
}

/// A DATE `UNTIL` keeps wall-clock meaning: the whole last day counts, gap
/// shift included.
#[test]
fn date_until_includes_the_whole_last_day() {
    let (item, rules) = imported_item("20260328T023000", "FREQ=DAILY;UNTIL=20260329");
    let starts = expand_imported(&item, rules, &spring_forward_expansion()).expect("expands");
    assert_eq!(
        starts,
        vec![utc(2026, 3, 28, 1, 30), utc(2026, 3, 29, 1, 30)]
    );
}
