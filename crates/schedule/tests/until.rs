//! An instant `UNTIL` cuts on the instant, not on a wall clock.
//!
//! Expansion walks wall-clock time, so around a DST change the wall-clock
//! comparison and the instant comparison disagree. A DATE `UNTIL` keeps
//! wall-clock meaning and covers its whole day.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use clipper_schedule::{
    BlockDuration, Cadence, Expansion, Frequency, ImportedRuleResolver, Recurrence, RecurrenceEnd,
    RecurrenceEngine, RecurrenceId, RruleEngine, ScheduleItem, ScheduleItemId, ScheduleSpan,
    TimeRange, TimedStart,
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

fn local_midnight_window(zone: Tz, from: (i32, u32, u32), to: (i32, u32, u32)) -> TimeRange {
    let midnight = |(year, month, day): (i32, u32, u32)| {
        zone.with_ymd_and_hms(year, month, day, 0, 0, 0)
            .single()
            .expect("this local midnight exists")
            .with_timezone(&Utc)
    };
    TimeRange::new(midnight(from), midnight(to)).expect("non-empty window")
}

/// A candidate admitted only by the UNTIL slack that cannot resolve is past
/// the cutoff, so it is skipped. Samoa skipped 2011-12-30 entirely: the cutoff
/// 2011-12-30T09:59Z reads as 23:59 on the 29th in Apia, the scan bound admits
/// the 30th, and the all-day span on the 30th has no length. Only the 29th
/// remains.
#[test]
fn until_slack_skips_an_unresolvable_day_past_the_cutoff() {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "apia".to_string(),
        span: ScheduleSpan::AllDay {
            start: NaiveDate::from_ymd_opt(2011, 12, 28).expect("valid date"),
            days: std::num::NonZeroU32::new(1).expect("non-zero"),
        },
        recurrence: Recurrence::Every(
            Cadence::each(Frequency::Daily).ending(RecurrenceEnd::On(utc(2011, 12, 30, 9, 59))),
        ),
        reference: None,
        alarm: None,
    };
    let expansion = Expansion {
        window: local_midnight_window(Tz::Pacific__Apia, (2011, 12, 29), (2011, 12, 31)),
        observer: Tz::Pacific__Apia,
    };
    let out = RruleEngine::new()
        .occurrences(&item, &[], &expansion)
        .expect("the skipped day past the cutoff must not fail the window");
    let ids: Vec<_> = out.iter().map(|o| o.recurrence_id).collect();
    assert_eq!(
        ids,
        vec![RecurrenceId::Date(
            NaiveDate::from_ymd_opt(2011, 12, 29).expect("valid date")
        )],
        "only December 29 resolves before the cutoff"
    );
    assert_eq!(out[0].span.start(), utc(2011, 12, 29, 10, 0));
}

/// A resolved candidate is judged on the instant, never on the wall clock.
/// October 25 02:45 Berlin resolves to the first fold, 00:45Z, which is before
/// the 01:30Z cutoff, so it stays in although its wall clock is later than the
/// cutoff's wall clock of 02:30.
#[test]
fn resolved_fold_candidate_keeps_instant_cutoff_despite_later_wall_clock() {
    let item = cadence_item(
        "20261024T024500",
        RecurrenceEnd::On(utc(2026, 10, 25, 1, 30)),
    );
    let starts = expand(&item, &fall_back_expansion()).expect("expands");
    assert_eq!(
        starts,
        vec![utc(2026, 10, 24, 0, 45), utc(2026, 10, 25, 0, 45)],
        "the October 25 fold resolves before the cutoff and must be kept"
    );
}
