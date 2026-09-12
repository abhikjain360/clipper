//! Adversarial coverage for the scheduler domain crate.
//!
//! Each test targets a spot where the documented contract is easy to break:
//! DST gaps and folds, short months and leap days, override identity across
//! timezone travel, window edges, expansion limits, alarm planning, and the
//! serde boundary. Tests assert the documented behaviour; a failure names a
//! spot where the code and the docs disagree.

use std::num::NonZeroU32;

use chrono::{DateTime, Month, NaiveDate, NaiveDateTime, TimeDelta, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use clipper_schedule::{
    AlarmPolicy, BlockDuration, Cadence, EngineError, Expansion, Frequency, ImportedRuleResolver,
    MonthDay, MonthlyRule, NthWeekday, Occurrence, OccurrenceOrigin, OccurrenceOverrideData,
    OverrideChange, OverrideId, Recurrence, RecurrenceEnd, RecurrenceEngine, RecurrenceId,
    RruleEngine, ScheduleItem, ScheduleItemId, ScheduleSpan, SourceId, TimeError, TimeRange,
    TimedStart, WeekdaySet, parse_ics, parse_imported_recurrence_rules, plan_alarms,
};

fn local(text: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(text, "%Y%m%dT%H%M%S").expect("valid local datetime")
}

fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0)
        .single()
        .expect("unambiguous UTC instant")
}

fn window(from: DateTime<Utc>, to: DateTime<Utc>) -> TimeRange {
    TimeRange::new(from, to).expect("non-empty window")
}

fn expansion(from: DateTime<Utc>, to: DateTime<Utc>, observer: Tz) -> Expansion {
    Expansion {
        window: window(from, to),
        observer,
    }
}

/// A window running from local midnight to local midnight in `zone`.
fn local_midnight_window(zone: Tz, from: (i32, u32, u32), to: (i32, u32, u32)) -> TimeRange {
    let midnight = |(year, month, day): (i32, u32, u32)| {
        zone.with_ymd_and_hms(year, month, day, 0, 0, 0)
            .single()
            .expect("this local midnight exists")
            .with_timezone(&Utc)
    };
    window(midnight(from), midnight(to))
}

fn timed_item(start: TimedStart, recurrence: Recurrence) -> ScheduleItem {
    ScheduleItem {
        id: ScheduleItemId::new(),
        title: "adversarial".to_string(),
        span: ScheduleSpan::Timed {
            start,
            duration: BlockDuration::from_minutes(30).expect("30 is non-zero"),
        },
        recurrence,
        reference: None,
        alarm: None,
    }
}

fn daily() -> Recurrence {
    Recurrence::Every(Cadence::each(Frequency::Daily))
}

fn expand(
    item: &ScheduleItem,
    overrides: &[OccurrenceOverrideData],
    expansion: &Expansion,
) -> Result<Vec<Occurrence>, EngineError> {
    RruleEngine::new().occurrences(item, overrides, expansion)
}

fn in_zone(instant: DateTime<Utc>, zone: Tz) -> String {
    instant
        .with_timezone(&zone)
        .format("%Y-%m-%d %H:%M %Z")
        .to_string()
}

// ---------------------------------------------------------------------------
// Daylight saving
// ---------------------------------------------------------------------------

/// Floating 02:30 on Berlin's spring-forward day (2026-03-29, 02:00 -> 03:00).
/// The documented policy shifts a nonexistent time forward by the gap.
#[test]
fn floating_0230_spring_forward_berlin() {
    let span = ScheduleSpan::Timed {
        start: TimedStart::Floating(local("20260329T023000")),
        duration: BlockDuration::from_minutes(30).expect("non-zero"),
    };
    let resolved = span.resolve(Tz::Europe__Berlin).expect("resolves");
    assert_eq!(
        in_zone(resolved.start(), Tz::Europe__Berlin),
        "2026-03-29 03:30 CEST"
    );
}

/// Floating 02:30 on New York's spring-forward day (2026-03-08, 02:00 -> 03:00).
#[test]
fn floating_0230_spring_forward_new_york() {
    let span = ScheduleSpan::Timed {
        start: TimedStart::Floating(local("20260308T023000")),
        duration: BlockDuration::from_minutes(30).expect("non-zero"),
    };
    let resolved = span.resolve(Tz::America__New_York).expect("resolves");
    assert_eq!(
        in_zone(resolved.start(), Tz::America__New_York),
        "2026-03-08 03:30 EDT"
    );
}

/// Zoned 02:30 on Berlin's fall-back day (2026-10-25) is ambiguous.
/// The documented policy takes the earlier instant: 02:30 CEST = 00:30 UTC.
#[test]
fn zoned_0230_fall_back_berlin_takes_earlier_instant() {
    let start = TimedStart::Zoned {
        local: local("20261025T023000"),
        zone: Tz::Europe__Berlin,
    };
    let resolved = start.resolve(Tz::UTC).expect("resolves");
    assert_eq!(
        resolved,
        utc(2026, 10, 25, 0, 30),
        "ambiguous 02:30 must resolve to the earlier instant (CEST, +2)"
    );
}

/// Zoned 01:30 on New York's fall-back day (2026-11-01) is ambiguous.
/// Earlier instant is 01:30 EDT = 05:30 UTC.
#[test]
fn zoned_0130_fall_back_new_york_takes_earlier_instant() {
    let start = TimedStart::Zoned {
        local: local("20261101T013000"),
        zone: Tz::America__New_York,
    };
    let resolved = start.resolve(Tz::UTC).expect("resolves");
    assert_eq!(
        resolved,
        utc(2026, 11, 1, 5, 30),
        "ambiguous 01:30 must resolve to the earlier instant (EDT, -4)"
    );
}

/// A single all-day block on Berlin's fall-back day lasts 25 elapsed hours.
#[test]
fn all_day_on_fall_back_day_is_25_hours() {
    let span = ScheduleSpan::AllDay {
        start: NaiveDate::from_ymd_opt(2026, 10, 25).expect("valid date"),
        days: NonZeroU32::new(1).expect("non-zero"),
    };
    let resolved = span.resolve(Tz::Europe__Berlin).expect("resolves");
    assert_eq!(
        (resolved.end() - resolved.start()).num_hours(),
        25,
        "25 October 2026 is a 25-hour day in Berlin"
    );
}

/// Two all-day blocks over the fall-back weekend last 24 + 25 = 49 hours.
#[test]
fn two_all_day_blocks_over_fall_back_are_49_hours() {
    let span = ScheduleSpan::AllDay {
        start: NaiveDate::from_ymd_opt(2026, 10, 24).expect("valid date"),
        days: NonZeroU32::new(2).expect("non-zero"),
    };
    let resolved = span.resolve(Tz::Europe__Berlin).expect("resolves");
    assert_eq!(
        (resolved.end() - resolved.start()).num_hours(),
        49,
        "24 Oct is 24h and 25 Oct is 25h in Berlin"
    );
}

/// A zoned weekly series keeps its wall-clock time across spring-forward:
/// Monday 09:00 Berlin is 08:00 UTC before the transition, 07:00 UTC after.
#[test]
fn weekly_zoned_series_keeps_wall_clock_across_spring_forward() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260323T090000"),
            zone: Tz::Europe__Berlin,
        },
        Recurrence::Every(Cadence::each(Frequency::Weekly {
            weekdays: WeekdaySet::just(Weekday::Mon),
        })),
    );
    let out = expand(
        &item,
        &[],
        &expansion(
            utc(2026, 3, 22, 0, 0),
            utc(2026, 4, 1, 0, 0),
            Tz::Europe__Berlin,
        ),
    )
    .expect("expands");
    assert_eq!(out.len(), 2);
    assert_eq!(
        in_zone(out[0].span.start(), Tz::Europe__Berlin),
        "2026-03-23 09:00 CET"
    );
    assert_eq!(
        in_zone(out[1].span.start(), Tz::Europe__Berlin),
        "2026-03-30 09:00 CEST"
    );
    assert_eq!(out[0].span.start(), utc(2026, 3, 23, 8, 0));
    assert_eq!(out[1].span.start(), utc(2026, 3, 30, 7, 0));
}

/// A zoned weekly series keeps its wall-clock time across fall-back.
#[test]
fn weekly_zoned_series_keeps_wall_clock_across_fall_back() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20261019T090000"),
            zone: Tz::Europe__Berlin,
        },
        Recurrence::Every(Cadence::each(Frequency::Weekly {
            weekdays: WeekdaySet::just(Weekday::Mon),
        })),
    );
    let out = expand(
        &item,
        &[],
        &expansion(
            utc(2026, 10, 18, 0, 0),
            utc(2026, 10, 28, 0, 0),
            Tz::Europe__Berlin,
        ),
    )
    .expect("expands");
    assert_eq!(out.len(), 2);
    assert_eq!(
        in_zone(out[0].span.start(), Tz::Europe__Berlin),
        "2026-10-19 09:00 CEST"
    );
    assert_eq!(
        in_zone(out[1].span.start(), Tz::Europe__Berlin),
        "2026-10-26 09:00 CET"
    );
    assert_eq!(out[0].span.start(), utc(2026, 10, 19, 7, 0));
    assert_eq!(out[1].span.start(), utc(2026, 10, 26, 8, 0));
}

/// A daily series whose DTSTART itself falls inside the spring-forward gap.
/// The gap-day occurrence must still appear, shifted forward like any other
/// gap occurrence, rather than vanishing or erroring the whole expansion.
#[test]
fn series_starting_inside_the_gap_still_emits_the_gap_day() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260329T023000"),
            zone: Tz::Europe__Berlin,
        },
        daily(),
    );
    let out = expand(
        &item,
        &[],
        &expansion(
            utc(2026, 3, 28, 0, 0),
            utc(2026, 3, 31, 0, 0),
            Tz::Europe__Berlin,
        ),
    )
    .expect("expansion succeeds");
    let starts: Vec<String> = out
        .iter()
        .map(|o| in_zone(o.span.start(), Tz::Europe__Berlin))
        .collect();
    assert_eq!(
        starts,
        vec!["2026-03-29 03:30 CEST", "2026-03-30 02:30 CEST"],
        "the gap day must shift forward to 03:30, not disappear"
    );
}

/// A floating series that starts inside a gap expands everywhere. The rule is
/// walked on wall-clock time, so the observer's zone cannot poison DTSTART, and
/// the gap day keeps the same wall-clock identity in every zone.
#[test]
fn floating_series_starting_in_gap_expands_in_every_zone() {
    let item = timed_item(TimedStart::Floating(local("20260329T023000")), daily());
    let wide = expansion(utc(2026, 3, 28, 0, 0), utc(2026, 3, 31, 0, 0), Tz::UTC);
    let gap_day = RecurrenceId::Floating(local("20260329T023000"));

    let tokyo = expand(
        &item,
        &[],
        &Expansion {
            window: wide.window,
            observer: Tz::Asia__Tokyo,
        },
    )
    .expect("Tokyo has no gap at 02:30 on Mar 29");
    assert_eq!(tokyo.len(), 3);
    assert_eq!(tokyo[0].recurrence_id, gap_day);

    let berlin = expand(
        &item,
        &[],
        &Expansion {
            window: wide.window,
            observer: Tz::Europe__Berlin,
        },
    )
    .expect("Berlin must not drop the series it lives in");
    assert_eq!(berlin[0].recurrence_id, gap_day);
    assert_eq!(
        in_zone(berlin[0].span.start(), Tz::Europe__Berlin),
        "2026-03-29 03:30 CEST",
        "the gap day shifts forward for the Berlin observer only"
    );
}

/// Samoa's 2011-12-30 never happened in Pacific/Apia: the clock jumped from
/// 2011-12-29 to 2011-12-31. A floating series still expands there, and the
/// skipped date's occurrence lands on the next valid instant while keeping its
/// own wall-clock identity.
#[test]
fn floating_series_on_a_skipped_date_shifts_to_the_next_valid_instant() {
    let item = timed_item(TimedStart::Floating(local("20111229T120000")), daily());
    let out = expand(
        &item,
        &[],
        &expansion(
            utc(2011, 12, 29, 0, 0),
            utc(2012, 1, 2, 0, 0),
            Tz::Pacific__Apia,
        ),
    )
    .expect("the skipped date must not break the series");
    let ids: Vec<_> = out.iter().map(|o| o.recurrence_id).collect();
    assert_eq!(
        ids,
        vec![
            RecurrenceId::Floating(local("20111229T120000")),
            RecurrenceId::Floating(local("20111230T120000")),
            RecurrenceId::Floating(local("20111231T120000")),
            RecurrenceId::Floating(local("20120101T120000")),
            RecurrenceId::Floating(local("20120102T120000")),
        ],
        "every wall-clock day is still an occurrence, skipped date included"
    );
    assert_eq!(
        out[1].span.start(),
        out[2].span.start(),
        "the skipped date shifts onto the first instant that exists, 12:00 on \
         the 31st"
    );
    assert_eq!(
        in_zone(out[1].span.start(), Tz::Pacific__Apia),
        "2011-12-31 12:00 +14"
    );
}

/// The one shape the skipped civil date still breaks: an all-day occurrence on
/// it has no length. Both of its local midnights resolve to the same instant,
/// and a zero-length interval is not representable, so the expansion reports
/// that rather than inventing a day. Changing it is a `time.rs` policy
/// decision, not an engine one.
#[test]
fn all_day_series_on_a_skipped_date_reports_a_zero_length_day() {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "apia".to_string(),
        span: ScheduleSpan::AllDay {
            start: NaiveDate::from_ymd_opt(2011, 12, 30).expect("valid date"),
            days: NonZeroU32::new(1).expect("non-zero"),
        },
        recurrence: daily(),
        reference: None,
        alarm: None,
    };
    let result = expand(
        &item,
        &[],
        &expansion(
            utc(2011, 12, 29, 0, 0),
            utc(2012, 1, 2, 0, 0),
            Tz::Pacific__Apia,
        ),
    );
    assert!(
        matches!(
            result,
            Err(EngineError::Time(TimeError::InvalidRange { .. }))
        ),
        "expected the zero-length civil day to surface, got {result:?}"
    );
}

/// A series that started before a skipped civil date still expands a later
/// window. Candidates before the window are skipped before resolving, so the
/// unresolvable Dec 30 all-day span never fails an expansion that does not
/// include it.
#[test]
fn series_starting_before_a_skipped_date_expands_a_later_window() {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "apia".to_string(),
        span: ScheduleSpan::AllDay {
            start: NaiveDate::from_ymd_opt(2011, 12, 29).expect("valid date"),
            days: NonZeroU32::new(1).expect("non-zero"),
        },
        recurrence: daily(),
        reference: None,
        alarm: None,
    };
    let out = expand(
        &item,
        &[],
        &expansion(
            utc(2012, 1, 2, 0, 0),
            utc(2012, 1, 3, 0, 0),
            Tz::Pacific__Apia,
        ),
    )
    .expect("candidates before the window must not fail the expansion");
    let ids: Vec<_> = out.iter().map(|o| o.recurrence_id).collect();
    assert_eq!(
        ids,
        vec![RecurrenceId::Date(
            NaiveDate::from_ymd_opt(2012, 1, 3).expect("valid date")
        )],
        "a Jan 3 all-day span starts Jan 2 10:00Z, inside the window"
    );
}

/// The pre-filter keeps a candidate dated before the window when it resolves
/// into it: a floating 23:30 in UTC-10 is 09:30Z the next day.
#[test]
fn pre_filter_keeps_a_day_before_candidate_resolving_into_the_window() {
    let item = timed_item(TimedStart::Floating(local("20260601T233000")), daily());
    let out = expand(
        &item,
        &[],
        &expansion(
            utc(2026, 6, 11, 0, 0),
            utc(2026, 6, 12, 0, 0),
            Tz::Pacific__Tahiti,
        ),
    )
    .expect("expands");
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].recurrence_id,
        RecurrenceId::Floating(local("20260610T233000")),
        "the wall clock is the day before the window in UTC terms"
    );
    assert_eq!(out[0].span.start(), utc(2026, 6, 11, 9, 30));
}

/// Candidates on or past a day after the window end never resolve into it, so
/// they are skipped before resolving. An all-day series over Samoa's skipped
/// December 30 with a window ending December 29 keeps the earlier days instead
/// of failing on the skipped one.
#[test]
fn pre_filter_skips_a_day_after_candidates_without_resolving_them() {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "apia".to_string(),
        span: ScheduleSpan::AllDay {
            start: NaiveDate::from_ymd_opt(2011, 12, 27).expect("valid date"),
            days: NonZeroU32::new(1).expect("non-zero"),
        },
        recurrence: daily(),
        reference: None,
        alarm: None,
    };
    let out = expand(
        &item,
        &[],
        &Expansion {
            window: local_midnight_window(Tz::Pacific__Apia, (2011, 12, 27), (2011, 12, 29)),
            observer: Tz::Pacific__Apia,
        },
    )
    .expect("candidates after the window must not fail the expansion");
    let ids: Vec<_> = out.iter().map(|o| o.recurrence_id).collect();
    assert_eq!(
        ids,
        vec![
            RecurrenceId::Date(NaiveDate::from_ymd_opt(2011, 12, 27).expect("valid date")),
            RecurrenceId::Date(NaiveDate::from_ymd_opt(2011, 12, 28).expect("valid date")),
        ],
        "December 27 and 28 resolve before the skip; December 30 is never resolved"
    );
}

/// Santiago and Havana spring forward at 00:00, so local midnight itself is the
/// missing hour on the transition day. An all-day series must still produce
/// every date: the midnight shifts forward and the day is 23 hours long.
#[test]
fn all_day_series_keeps_every_date_across_a_midnight_gap() {
    for (zone, from, to) in [
        (Tz::America__Santiago, (2022, 9, 8), (2022, 9, 14)),
        (Tz::America__Havana, (2026, 3, 5), (2026, 3, 11)),
    ] {
        let first = NaiveDate::from_ymd_opt(from.0, from.1, from.2).expect("valid date");
        let item = ScheduleItem {
            id: ScheduleItemId::new(),
            title: "every day".to_string(),
            span: ScheduleSpan::AllDay {
                start: first,
                days: NonZeroU32::new(1).expect("non-zero"),
            },
            recurrence: daily(),
            reference: None,
            alarm: None,
        };
        let out = expand(
            &item,
            &[],
            &Expansion {
                window: local_midnight_window(zone, from, to),
                observer: zone,
            },
        )
        .expect("expands");
        let expected: Vec<RecurrenceId> = (0..6)
            .map(|offset| RecurrenceId::Date(first + chrono::Days::new(offset)))
            .collect();
        assert_eq!(
            out.iter().map(|o| o.recurrence_id).collect::<Vec<_>>(),
            expected,
            "no local date may disappear in {zone}"
        );
    }
}

/// A floating identity is a wall clock, so it names the same occurrence for
/// every observer even when that wall clock does not exist in their zone. Only
/// the resolved instant differs.
#[test]
fn a_floating_gap_identity_is_the_same_for_every_observer() {
    let item = timed_item(TimedStart::Floating(local("20260327T023000")), daily());
    let span = window(utc(2026, 3, 29, 0, 0), utc(2026, 3, 30, 0, 0));

    let berlin = expand(
        &item,
        &[],
        &Expansion {
            window: span,
            observer: Tz::Europe__Berlin,
        },
    )
    .expect("expands");
    let anywhere = expand(
        &item,
        &[],
        &Expansion {
            window: span,
            observer: Tz::UTC,
        },
    )
    .expect("expands");

    let gap_day = RecurrenceId::Floating(local("20260329T023000"));
    assert_eq!(berlin.len(), 1);
    assert_eq!(anywhere.len(), 1);
    assert_eq!(berlin[0].recurrence_id, gap_day);
    assert_eq!(anywhere[0].recurrence_id, gap_day);
    assert_eq!(
        berlin[0].span.start(),
        utc(2026, 3, 29, 1, 30),
        "03:30 CEST, the gap shift, is 01:30 UTC"
    );
    assert_eq!(anywhere[0].span.start(), utc(2026, 3, 29, 2, 30));
}

// ---------------------------------------------------------------------------
// Monthly and yearly rules, intervals, end conditions, week starts
// ---------------------------------------------------------------------------

/// Monthly on the 30th skips February (2026 is not a leap year).
#[test]
fn monthly_30th_skips_february() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260130T080000"),
            zone: Tz::UTC,
        },
        Recurrence::Every(Cadence::each(Frequency::Monthly(MonthlyRule::OnDay(
            MonthDay::from_start(30).expect("in range"),
        )))),
    );
    let out = expand(
        &item,
        &[],
        &expansion(utc(2026, 2, 1, 0, 0), utc(2026, 4, 1, 0, 0), Tz::UTC),
    )
    .expect("expands");
    let starts: Vec<_> = out.iter().map(|o| o.span.start()).collect();
    assert_eq!(starts, vec![utc(2026, 3, 30, 8, 0)]);
}

/// Monthly on the 29th skips February 2026 (28 days) but not February 2028.
#[test]
fn monthly_29th_skips_non_leap_february() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260129T080000"),
            zone: Tz::UTC,
        },
        Recurrence::Every(Cadence::each(Frequency::Monthly(MonthlyRule::OnDay(
            MonthDay::from_start(29).expect("in range"),
        )))),
    );
    let out = expand(
        &item,
        &[],
        &expansion(utc(2026, 2, 1, 0, 0), utc(2026, 4, 1, 0, 0), Tz::UTC),
    )
    .expect("expands");
    let starts: Vec<_> = out.iter().map(|o| o.span.start()).collect();
    assert_eq!(
        starts,
        vec![utc(2026, 3, 29, 8, 0)],
        "Feb 2026 has no 29th, so only March appears"
    );

    let out = expand(
        &item,
        &[],
        &expansion(utc(2028, 2, 1, 0, 0), utc(2028, 3, 1, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert_eq!(
        out.iter().map(|o| o.span.start()).collect::<Vec<_>>(),
        vec![utc(2028, 2, 29, 8, 0)],
        "Feb 2028 is a leap year, so the 29th appears"
    );
}

/// Monthly on the 31st with interval 2 from January: Jan, Mar, May, Jul.
#[test]
fn monthly_31st_every_two_months() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260131T080000"),
            zone: Tz::UTC,
        },
        Recurrence::Every(
            Cadence::every(
                Frequency::Monthly(MonthlyRule::OnDay(
                    MonthDay::from_start(31).expect("in range"),
                )),
                2,
            )
            .expect("non-zero"),
        ),
    );
    let out = expand(
        &item,
        &[],
        &expansion(utc(2026, 1, 1, 0, 0), utc(2026, 8, 1, 0, 0), Tz::UTC),
    )
    .expect("expands");
    let starts: Vec<_> = out.iter().map(|o| o.span.start()).collect();
    assert_eq!(
        starts,
        vec![
            utc(2026, 1, 31, 8, 0),
            utc(2026, 3, 31, 8, 0),
            utc(2026, 5, 31, 8, 0),
            utc(2026, 7, 31, 8, 0),
        ]
    );
}

/// Yearly Feb 29 with interval 2 from 2024: 2024, then 2028 (2026 skipped).
#[test]
fn yearly_feb29_every_two_years() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20240229T080000"),
            zone: Tz::UTC,
        },
        Recurrence::Every(
            Cadence::every(
                Frequency::Yearly {
                    month: Month::February,
                    day: MonthDay::from_start(29).expect("in range"),
                },
                2,
            )
            .expect("non-zero"),
        ),
    );
    let out = expand(
        &item,
        &[],
        &expansion(utc(2025, 1, 1, 0, 0), utc(2030, 1, 1, 0, 0), Tz::UTC),
    )
    .expect("expands");
    let starts: Vec<_> = out.iter().map(|o| o.span.start()).collect();
    assert_eq!(starts, vec![utc(2028, 2, 29, 8, 0)]);
}

/// `count` counts the first occurrence: count 3 from Jun 10 is Jun 10-12.
#[test]
fn count_includes_the_first_occurrence() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260610T080000"),
            zone: Tz::UTC,
        },
        Recurrence::Every(
            Cadence::each(Frequency::Daily).ending(RecurrenceEnd::after(3).expect("non-zero")),
        ),
    );
    let out = expand(
        &item,
        &[],
        &expansion(utc(2026, 6, 1, 0, 0), utc(2026, 7, 1, 0, 0), Tz::UTC),
    )
    .expect("expands");
    let starts: Vec<_> = out.iter().map(|o| o.span.start()).collect();
    assert_eq!(
        starts,
        vec![
            utc(2026, 6, 10, 8, 0),
            utc(2026, 6, 11, 8, 0),
            utc(2026, 6, 12, 8, 0),
        ]
    );
}

/// `until` is inclusive: an occurrence starting exactly at `until` is kept.
#[test]
fn until_is_inclusive_of_the_boundary_instant() {
    let item_at = timed_item(
        TimedStart::Zoned {
            local: local("20260610T080000"),
            zone: Tz::UTC,
        },
        Recurrence::Every(
            Cadence::each(Frequency::Daily).ending(RecurrenceEnd::On(utc(2026, 6, 12, 8, 0))),
        ),
    );
    let out = expand(
        &item_at,
        &[],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 14, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert_eq!(out.len(), 3, "Jun 12 08:00 == until must be included");

    let item_before = timed_item(
        TimedStart::Zoned {
            local: local("20260610T080000"),
            zone: Tz::UTC,
        },
        Recurrence::Every(
            Cadence::each(Frequency::Daily).ending(RecurrenceEnd::On(utc(2026, 6, 12, 7, 59))),
        ),
    );
    let out = expand(
        &item_before,
        &[],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 14, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert_eq!(out.len(), 2, "Jun 12 08:00 > until must be excluded");
}

/// `until` is a UTC instant even for a zoned series: 09:00 Berlin (07:00 UTC)
/// on Jun 12 is still before a Jun 12 08:00 UTC cutoff.
#[test]
fn until_compares_instants_not_wall_clock() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260610T090000"),
            zone: Tz::Europe__Berlin,
        },
        Recurrence::Every(
            Cadence::each(Frequency::Daily).ending(RecurrenceEnd::On(utc(2026, 6, 12, 8, 0))),
        ),
    );
    let out = expand(
        &item,
        &[],
        &expansion(
            utc(2026, 6, 10, 0, 0),
            utc(2026, 6, 14, 0, 0),
            Tz::Europe__Berlin,
        ),
    )
    .expect("expands");
    assert_eq!(
        out.len(),
        3,
        "Jun 12 09:00 Berlin is 07:00 UTC, inside a 08:00 UTC cutoff"
    );
}

/// Fortnightly Sun+Mon across the year boundary: with Monday-start weeks,
/// Sun Jan 3 2027 falls in the off week, so the next occurrence after the
/// Dec 27 DTSTART is Mon Jan 4, not Sun Jan 3.
#[test]
fn fortnightly_weeks_start_on_monday_across_year_boundary() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20261227T070000"),
            zone: Tz::UTC,
        },
        Recurrence::Every(
            Cadence::every(
                Frequency::Weekly {
                    weekdays: WeekdaySet::new(&[Weekday::Sun, Weekday::Mon]).expect("non-empty"),
                },
                2,
            )
            .expect("non-zero"),
        ),
    );
    let next = RruleEngine::new()
        .next_after(
            &item,
            &[],
            utc(2026, 12, 27, 10, 0),
            TimeDelta::days(60),
            Tz::UTC,
        )
        .expect("expands")
        .expect("a fortnightly rule recurs within 60 days");
    assert_eq!(
        next.span.start(),
        utc(2027, 1, 4, 7, 0),
        "Sun Jan 3 is in the off week under WKST=MO; Mon Jan 4 is next"
    );
}

// ---------------------------------------------------------------------------
// Overrides
// ---------------------------------------------------------------------------

fn cancelled(item: &ScheduleItem, recurrence_id: RecurrenceId) -> OccurrenceOverrideData {
    OccurrenceOverrideData {
        id: OverrideId::new(),
        item: item.id,
        recurrence_id,
        change: OverrideChange::Cancelled,
    }
}

fn rescheduled(
    item: &ScheduleItem,
    recurrence_id: RecurrenceId,
    to: ScheduleSpan,
) -> OccurrenceOverrideData {
    OccurrenceOverrideData {
        id: OverrideId::new(),
        item: item.id,
        recurrence_id,
        change: OverrideChange::Rescheduled(to),
    }
}

fn utc_span(y: i32, m: u32, d: u32, h: u32, min: u32) -> ScheduleSpan {
    ScheduleSpan::Timed {
        start: TimedStart::Zoned {
            local: NaiveDate::from_ymd_opt(y, m, d)
                .expect("valid date")
                .and_hms_opt(h, min, 0)
                .expect("valid time"),
            zone: Tz::UTC,
        },
        duration: BlockDuration::from_minutes(30).expect("non-zero"),
    }
}

/// A floating cancellation inside the DST gap matches in every observer zone:
/// cancelling Floating(2026-03-29T02:30) removes the day that resolves to
/// 03:30 in Berlin, and the ordinary 02:30 day everywhere else.
#[test]
fn floating_cancellation_in_dst_gap_matches_in_any_zone() {
    let item = timed_item(TimedStart::Floating(local("20260301T023000")), daily());
    let skip = cancelled(&item, RecurrenceId::Floating(local("20260329T023000")));
    // Wide enough that every zone in the test sees whole local days.
    let wide = expansion(utc(2026, 3, 26, 0, 0), utc(2026, 4, 1, 0, 0), Tz::UTC);

    for observer in [
        Tz::Europe__Berlin,
        Tz::Asia__Tokyo,
        Tz::America__Los_Angeles,
        Tz::UTC,
    ] {
        let exp = Expansion {
            window: wide.window,
            observer,
        };
        let out = expand(&item, std::slice::from_ref(&skip), &exp).expect("expands");
        let ids: Vec<_> = out.iter().map(|o| o.recurrence_id).collect();
        assert!(
            !ids.contains(&RecurrenceId::Floating(local("20260329T023000"))),
            "the cancelled gap day must stay cancelled for observer {observer}"
        );
        assert!(
            ids.contains(&RecurrenceId::Floating(local("20260328T023000"))),
            "only the cancelled day should be missing for observer {observer}"
        );
    }
}

/// Moving an occurrence out of the window removes it without adding anything.
#[test]
fn override_moving_an_occurrence_out_of_the_window() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260601T080000"),
            zone: Tz::UTC,
        },
        daily(),
    );
    let moved_out = rescheduled(
        &item,
        RecurrenceId::Instant(utc(2026, 6, 11, 8, 0)),
        utc_span(2026, 6, 20, 9, 0),
    );
    let out = expand(
        &item,
        &[moved_out],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 13, 0, 0), Tz::UTC),
    )
    .expect("expands");
    let starts: Vec<_> = out.iter().map(|o| o.span.start()).collect();
    assert_eq!(
        starts,
        vec![utc(2026, 6, 10, 8, 0), utc(2026, 6, 12, 8, 0)],
        "Jun 11 moved to Jun 20: absent here, and Jun 20 must not leak in"
    );
}

/// Moving an occurrence within the window keeps exactly one entry for it,
/// still keyed by its original rule identity.
#[test]
fn override_moving_an_occurrence_within_the_window() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260601T080000"),
            zone: Tz::UTC,
        },
        daily(),
    );
    let moved = rescheduled(
        &item,
        RecurrenceId::Instant(utc(2026, 6, 11, 8, 0)),
        utc_span(2026, 6, 11, 14, 0),
    );
    let override_id = match &moved.change {
        OverrideChange::Rescheduled(_) => moved.id,
        _ => unreachable!(),
    };
    let out = expand(
        &item,
        &[moved],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 13, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert_eq!(out.len(), 3);
    assert_eq!(out[1].span.start(), utc(2026, 6, 11, 14, 0));
    assert_eq!(out[1].origin, OccurrenceOrigin::Overridden(override_id));
    assert_eq!(
        out[1].recurrence_id,
        RecurrenceId::Instant(utc(2026, 6, 11, 8, 0))
    );
}

/// Two overrides for the same identity must not yield two occurrences.
#[test]
fn duplicate_overrides_for_one_identity_yield_at_most_one_occurrence() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260601T080000"),
            zone: Tz::UTC,
        },
        daily(),
    );
    let rid = RecurrenceId::Instant(utc(2026, 6, 11, 8, 0));
    let first = cancelled(&item, rid);
    let second = rescheduled(&item, rid, utc_span(2026, 6, 11, 14, 0));
    let out = expand(
        &item,
        &[first, second],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 13, 0, 0), Tz::UTC),
    )
    .expect("expands");
    let matching: Vec<_> = out.iter().filter(|o| o.recurrence_id == rid).collect();
    assert!(
        matching.len() <= 1,
        "one identity must never produce two occurrences, got {matching:?}"
    );
}

/// A rescheduled override for an identity the rule never generates adds that
/// occurrence. This is the `RDATE` path: a provider adds a date to a series by
/// sending an instance the rule does not produce, so the engine must keep it.
/// The caller is responsible for passing only overrides it trusts.
#[test]
fn a_rescheduled_override_for_an_ungenerated_identity_adds_an_occurrence() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260601T080000"),
            zone: Tz::UTC,
        },
        daily(),
    );
    let added = rescheduled(
        &item,
        RecurrenceId::Instant(utc(2026, 6, 11, 9, 37)),
        utc_span(2026, 6, 11, 10, 0),
    );
    let added_id = added.id;
    let out = expand(
        &item,
        &[added],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 13, 0, 0), Tz::UTC),
    )
    .expect("expands");
    let starts: Vec<_> = out.iter().map(|o| o.span.start()).collect();
    assert_eq!(
        starts,
        vec![
            utc(2026, 6, 10, 8, 0),
            utc(2026, 6, 11, 8, 0),
            utc(2026, 6, 11, 10, 0),
            utc(2026, 6, 12, 8, 0),
        ],
        "the added date joins the rule's own occurrences"
    );
    assert_eq!(out[2].origin, OccurrenceOrigin::Overridden(added_id));
}

/// The same `RDATE` path on a one-off: an override naming an identity the
/// single occurrence does not have adds a second event.
#[test]
fn a_rescheduled_override_adds_an_occurrence_to_a_one_off() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260610T080000"),
            zone: Tz::UTC,
        },
        Recurrence::Once,
    );
    let added = rescheduled(
        &item,
        RecurrenceId::Instant(utc(2026, 6, 1, 8, 0)),
        utc_span(2026, 6, 11, 9, 0),
    );
    let out = expand(
        &item,
        &[added],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 13, 0, 0), Tz::UTC),
    )
    .expect("expands");
    let starts: Vec<_> = out.iter().map(|o| o.span.start()).collect();
    assert_eq!(
        starts,
        vec![utc(2026, 6, 10, 8, 0), utc(2026, 6, 11, 9, 0)],
        "the added date joins the one-off"
    );
}

/// A cancelled override for an identity the rule never generates changes nothing.
#[test]
fn cancellation_for_an_unknown_identity_changes_nothing() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260601T080000"),
            zone: Tz::UTC,
        },
        daily(),
    );
    let unknown = cancelled(&item, RecurrenceId::Instant(utc(2026, 6, 11, 9, 37)));
    let out = expand(
        &item,
        &[unknown],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 13, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert_eq!(out.len(), 3);
}

// ---------------------------------------------------------------------------
// Window edges
// ---------------------------------------------------------------------------

/// An occurrence starting exactly at the window end is outside the window.
#[test]
fn occurrence_starting_exactly_at_window_end_is_excluded() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260612T000000"),
            zone: Tz::UTC,
        },
        Recurrence::Once,
    );
    let out = expand(
        &item,
        &[],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 12, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert!(out.is_empty(), "half-open windows exclude the end instant");
}

/// An occurrence starting exactly at the window start is inside the window.
#[test]
fn occurrence_starting_exactly_at_window_start_is_included() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260610T000000"),
            zone: Tz::UTC,
        },
        Recurrence::Once,
    );
    let out = expand(
        &item,
        &[],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 12, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert_eq!(out.len(), 1);
}

/// An event ending exactly at the window start touches but does not overlap it.
#[test]
fn occurrence_ending_exactly_at_window_start_is_excluded_everywhere() {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "touching".to_string(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local: local("20260609T233000"),
                zone: Tz::UTC,
            },
            duration: BlockDuration::from_minutes(30).expect("non-zero"),
        },
        recurrence: Recurrence::Once,
        reference: None,
        alarm: None,
    };
    let exp = expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 10, 1, 0), Tz::UTC);
    assert!(
        expand(&item, &[], &exp).expect("expands").is_empty(),
        "plain expansion selects by start"
    );
    assert!(
        RruleEngine::new()
            .overlapping_occurrences(&item, &[], &exp)
            .expect("expands")
            .is_empty(),
        "a merely touching event does not overlap a half-open window"
    );
}

/// An overnight event is invisible to plain expansion but visible to overlap.
#[test]
fn overnight_event_needs_overlap_expansion() {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "overnight".to_string(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local: local("20260609T233000"),
                zone: Tz::UTC,
            },
            duration: BlockDuration::from_minutes(120).expect("non-zero"),
        },
        recurrence: Recurrence::Once,
        reference: None,
        alarm: None,
    };
    let exp = expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 10, 1, 0), Tz::UTC);
    assert!(expand(&item, &[], &exp).expect("expands").is_empty());
    let overlapping = RruleEngine::new()
        .overlapping_occurrences(&item, &[], &exp)
        .expect("expands");
    assert_eq!(overlapping.len(), 1);
    assert_eq!(overlapping[0].span.end(), utc(2026, 6, 10, 1, 30));
}

/// A multi-day all-day event overlaps every window it covers, across DST.
#[test]
fn multi_day_all_day_overlaps_each_covered_window() {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "conference".to_string(),
        span: ScheduleSpan::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 3, 28).expect("valid date"),
            days: NonZeroU32::new(3).expect("non-zero"),
        },
        recurrence: Recurrence::Once,
        reference: None,
        alarm: None,
    };
    // Middle day, entirely inside the event.
    let exp = expansion(
        utc(2026, 3, 29, 0, 0),
        utc(2026, 3, 29, 12, 0),
        Tz::Europe__Berlin,
    );
    let overlapping = RruleEngine::new()
        .overlapping_occurrences(&item, &[], &exp)
        .expect("expands");
    assert_eq!(overlapping.len(), 1);
    // Plain expansion selects by start, so the middle day is not listed there.
    assert!(expand(&item, &[], &exp).expect("expands").is_empty());
}

/// A window entirely before the series start is empty, not an error.
#[test]
fn window_before_series_start_is_empty() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260610T080000"),
            zone: Tz::UTC,
        },
        daily(),
    );
    let out = expand(
        &item,
        &[],
        &expansion(utc(2026, 1, 1, 0, 0), utc(2026, 1, 2, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert!(out.is_empty());
}

/// A zero-length window cannot be built, so expansion never sees one.
/// `next_after` with a zero or negative horizon reports the same error.
#[test]
fn zero_length_windows_are_rejected() {
    let instant = utc(2026, 6, 10, 0, 0);
    assert!(TimeRange::new(instant, instant).is_err());
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260610T080000"),
            zone: Tz::UTC,
        },
        daily(),
    );
    assert!(
        RruleEngine::new()
            .next_after(&item, &[], instant, TimeDelta::zero(), Tz::UTC)
            .is_err()
    );
    assert!(
        RruleEngine::new()
            .next_after(&item, &[], instant, TimeDelta::minutes(-1), Tz::UTC)
            .is_err()
    );
}

// ---------------------------------------------------------------------------
// Imported rules and their UNTIL forms
// ---------------------------------------------------------------------------

fn import_id() -> clipper_api_types::ObjectId {
    uuid::Uuid::from_u128(0x9999).into()
}

/// The ordinary shape of an all-day recurring ICS event: a DATE-valued `UNTIL`.
/// It must import and expand, and the last day counts as inside the rule.
#[test]
fn an_imported_all_day_rule_with_a_date_until_includes_the_last_day() {
    let feed = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:team-day@example.com\r\n\
SUMMARY:Team day\r\n\
DTSTART;VALUE=DATE:20250106\r\n\
DTEND;VALUE=DATE:20250107\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20250203\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
    let import = import_id();
    let event = parse_ics(feed, SourceId::new(), import)
        .expect("the feed parses")
        .events
        .pop()
        .expect("the event parses");
    let rules = parse_imported_recurrence_rules(feed, import).expect("the rule resolves");
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: event.title,
        span: event.span,
        recurrence: event.recurrence,
        reference: None,
        alarm: None,
    };

    let out = RruleEngine::with_imported_rules(rules)
        .occurrences(
            &item,
            &[],
            &expansion(utc(2025, 1, 1, 0, 0), utc(2025, 3, 1, 0, 0), Tz::UTC),
        )
        .expect("a DATE until must not break expansion");
    let dates: Vec<_> = out.iter().map(|o| o.recurrence_id).collect();
    assert_eq!(
        dates,
        [6, 13, 20, 27]
            .into_iter()
            .map(|day| RecurrenceId::Date(
                NaiveDate::from_ymd_opt(2025, 1, day).expect("valid date")
            ))
            .chain(std::iter::once(RecurrenceId::Date(
                NaiveDate::from_ymd_opt(2025, 2, 3).expect("valid date")
            )))
            .collect::<Vec<_>>(),
        "every Monday up to and including the UNTIL date"
    );
}

/// A UTC `UNTIL` is an instant. On a zoned series it must still cut the series
/// at the occurrence that instant names, wall clock or not.
#[test]
fn an_imported_rule_with_a_utc_until_keeps_the_matching_day() {
    let import = import_id();
    let uid = "berlin-standup@example.com";
    let mut rules = ImportedRuleResolver::new();
    rules
        .insert(import, uid, "FREQ=DAILY;UNTIL=20260612T070000Z")
        .expect("a UTC until is valid");
    let mut item = timed_item(
        TimedStart::Zoned {
            local: local("20260610T090000"),
            zone: Tz::Europe__Berlin,
        },
        Recurrence::Once,
    );
    item.recurrence = Recurrence::Imported {
        import,
        uid: uid.to_string(),
    };

    let out = RruleEngine::with_imported_rules(rules)
        .occurrences(
            &item,
            &[],
            &expansion(
                utc(2026, 6, 9, 0, 0),
                utc(2026, 6, 15, 0, 0),
                Tz::Europe__Berlin,
            ),
        )
        .expect("expands");
    let starts: Vec<_> = out.iter().map(|o| o.span.start()).collect();
    assert_eq!(
        starts,
        vec![
            utc(2026, 6, 10, 7, 0),
            utc(2026, 6, 11, 7, 0),
            utc(2026, 6, 12, 7, 0),
        ],
        "09:00 Berlin on Jun 12 is 07:00 UTC, exactly the cutoff; Jun 13 is out"
    );
}

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// Exactly one candidate with a limit of one is complete, not an error.
#[test]
fn single_candidate_with_limit_one_is_complete() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260101T080000"),
            zone: Tz::UTC,
        },
        daily(),
    );
    let out = RruleEngine::with_max_candidates(1)
        .occurrences(
            &item,
            &[],
            &expansion(utc(2026, 1, 1, 0, 0), utc(2026, 1, 2, 0, 0), Tz::UTC),
        )
        .expect("an exact-size result is complete");
    assert_eq!(out.len(), 1);
}

/// Two candidates with a limit of one errors rather than truncating.
#[test]
fn two_candidates_with_limit_one_errors() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("20260101T080000"),
            zone: Tz::UTC,
        },
        daily(),
    );
    assert_eq!(
        RruleEngine::with_max_candidates(1).occurrences(
            &item,
            &[],
            &expansion(utc(2026, 1, 1, 0, 0), utc(2026, 1, 3, 0, 0), Tz::UTC),
        ),
        Err(EngineError::ExpansionLimitExceeded { limit: 1 })
    );
}

/// A daily series from 1800 expanded in 2026 must scan ~82k historical
/// candidates, past the 65,535 scan cap. The error must surface: the client
/// skips such a series, so silent truncation would drop alarms invisibly.
#[test]
fn very_old_daily_series_hits_the_historical_scan_limit() {
    let item = timed_item(
        TimedStart::Zoned {
            local: local("18000101T080000"),
            zone: Tz::UTC,
        },
        daily(),
    );
    assert_eq!(
        RruleEngine::new().occurrences(
            &item,
            &[],
            &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 12, 0, 0), Tz::UTC),
        ),
        Err(EngineError::ScanLimitExceeded { limit: 65_535 })
    );
}

// ---------------------------------------------------------------------------
// Alarms
// ---------------------------------------------------------------------------

fn alarmed_daily_utc() -> ScheduleItem {
    ScheduleItem {
        id: ScheduleItemId::new(),
        title: "standup".to_string(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local: local("20260610T080000"),
                zone: Tz::UTC,
            },
            duration: BlockDuration::from_minutes(30).expect("non-zero"),
        },
        recurrence: daily(),
        reference: None,
        alarm: Some(AlarmPolicy::at_start()),
    }
}

/// A lead time that fires before the expansion window still rings while it is
/// in the future: the planner filters by `now`, not by the window.
#[test]
fn alarm_lead_before_the_window_still_rings() {
    let mut item = alarmed_daily_utc();
    item.alarm = Some(AlarmPolicy::minutes_before(12 * 60));
    let out = expand(
        &item,
        &[],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 11, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert_eq!(out.len(), 1);
    // Fire time Jun 9 20:00 is before the window but after `now`.
    let planned = plan_alarms(&item, &out, utc(2026, 6, 9, 19, 0));
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].fire_at, utc(2026, 6, 9, 20, 0));
    assert_eq!(planned[0].occurrence_start, utc(2026, 6, 10, 8, 0));
}

/// An alarm whose lead already passed is dropped, including the exact-now edge.
#[test]
fn alarm_with_passed_lead_is_dropped() {
    let mut item = alarmed_daily_utc();
    item.alarm = Some(AlarmPolicy::minutes_before(60));
    let out = expand(
        &item,
        &[],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 11, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert!(plan_alarms(&item, &out, utc(2026, 6, 10, 7, 30)).is_empty());
    assert!(
        plan_alarms(&item, &out, utc(2026, 6, 10, 7, 0)).is_empty(),
        "an alarm firing exactly now counts as passed"
    );
}

/// Cancelled occurrences raise no alarm.
#[test]
fn cancelled_occurrence_raises_no_alarm() {
    let item = alarmed_daily_utc();
    let skip = cancelled(&item, RecurrenceId::Instant(utc(2026, 6, 10, 8, 0)));
    let out = expand(
        &item,
        &[skip],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 11, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert!(out.is_empty());
    assert!(plan_alarms(&item, &out, utc(2026, 6, 9, 0, 0)).is_empty());
}

/// A rescheduled occurrence raises exactly one alarm at its moved time, and
/// alarms come back soonest-first even when a later occurrence moved earlier.
#[test]
fn rescheduled_occurrence_raises_one_ordered_alarm() {
    let item = alarmed_daily_utc();
    // Move Jun 10 08:00 earlier to 06:00, and Jun 12 08:00 later to 09:00.
    let early = rescheduled(
        &item,
        RecurrenceId::Instant(utc(2026, 6, 10, 8, 0)),
        utc_span(2026, 6, 10, 6, 0),
    );
    let late = rescheduled(
        &item,
        RecurrenceId::Instant(utc(2026, 6, 12, 8, 0)),
        utc_span(2026, 6, 12, 9, 0),
    );
    let out = expand(
        &item,
        &[early, late],
        &expansion(utc(2026, 6, 10, 0, 0), utc(2026, 6, 13, 0, 0), Tz::UTC),
    )
    .expect("expands");
    assert_eq!(out.len(), 3, "no duplicate alarms for moved occurrences");
    let planned = plan_alarms(&item, &out, utc(2026, 6, 9, 0, 0));
    let fires: Vec<_> = planned.iter().map(|a| a.fire_at).collect();
    assert_eq!(
        fires,
        vec![
            utc(2026, 6, 10, 6, 0),
            utc(2026, 6, 11, 8, 0),
            utc(2026, 6, 12, 9, 0),
        ]
    );
}

// ---------------------------------------------------------------------------
// Serialization: JSON and postcard round-trips, invalid values rejected
// ---------------------------------------------------------------------------

fn json_round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_value(value).expect("serialize to JSON");
    let back: T = serde_json::from_value(json).expect("deserialize from JSON");
    assert_eq!(&back, value, "JSON round trip");
    back
}

fn postcard_round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = postcard::to_allocvec(value).expect("serialize to postcard");
    let back: T = postcard::from_bytes(&bytes).expect("deserialize from postcard");
    assert_eq!(&back, value, "postcard round trip");
    back
}

fn sample_item() -> ScheduleItem {
    ScheduleItem {
        id: ScheduleItemId::new(),
        title: "sample".to_string(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local: local("20260610T090000"),
                zone: Tz::Europe__Berlin,
            },
            duration: BlockDuration::from_minutes(45).expect("non-zero"),
        },
        recurrence: Recurrence::Every(
            Cadence::every(
                Frequency::Weekly {
                    weekdays: WeekdaySet::new(&[Weekday::Mon, Weekday::Wed]).expect("non-empty"),
                },
                2,
            )
            .expect("non-zero")
            .ending(RecurrenceEnd::after(10).expect("non-zero")),
        ),
        reference: None,
        alarm: Some(AlarmPolicy::minutes_before(15)),
    }
}

#[test]
fn every_public_time_shape_round_trips() {
    for span in [
        ScheduleSpan::Timed {
            start: TimedStart::Floating(local("20260610T070000")),
            duration: BlockDuration::from_minutes(45).expect("non-zero"),
        },
        ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local: local("20261025T023000"),
                zone: Tz::Europe__Berlin,
            },
            duration: BlockDuration::from_minutes(1).expect("non-zero"),
        },
        ScheduleSpan::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 3, 29).expect("valid date"),
            days: NonZeroU32::new(2).expect("non-zero"),
        },
    ] {
        json_round_trip(&span);
    }
    json_round_trip(
        &TimeRange::new(utc(2026, 6, 10, 9, 0), utc(2026, 6, 10, 10, 0)).expect("range"),
    );
}

#[test]
fn every_public_recurrence_shape_round_trips() {
    let weekly = Frequency::Weekly {
        weekdays: WeekdaySet::weekdays(),
    };
    let monthly_day =
        Frequency::Monthly(MonthlyRule::OnDay(MonthDay::from_end(1).expect("in range")));
    let monthly_weekday = Frequency::Monthly(MonthlyRule::OnWeekday {
        nth: NthWeekday::from_end(2).expect("in range"),
        weekday: Weekday::Fri,
    });
    let yearly = Frequency::Yearly {
        month: Month::February,
        day: MonthDay::from_start(29).expect("in range"),
    };
    for frequency in [
        Frequency::Daily,
        weekly,
        monthly_day,
        monthly_weekday,
        yearly,
    ] {
        for end in [
            RecurrenceEnd::Never,
            RecurrenceEnd::after(7).expect("non-zero"),
            RecurrenceEnd::On(utc(2026, 12, 31, 23, 59)),
        ] {
            let cadence = Cadence {
                frequency: frequency.clone(),
                interval: NonZeroU32::new(3).expect("non-zero"),
                end,
            };
            let recurrence = Recurrence::Every(cadence);
            json_round_trip(&recurrence);
        }
    }
    for recurrence in [
        Recurrence::Once,
        Recurrence::Imported {
            import: clipper_api_types::ObjectId::from(uuid::Uuid::new_v4()),
            uid: "meeting@example.com".into(),
        },
    ] {
        json_round_trip(&recurrence);
    }
    for id in [
        RecurrenceId::Floating(local("20260610T070000")),
        RecurrenceId::Instant(utc(2026, 6, 10, 7, 0)),
        RecurrenceId::Date(NaiveDate::from_ymd_opt(2026, 6, 10).expect("valid date")),
    ] {
        json_round_trip(&id);
    }
}

#[test]
fn every_public_item_and_alarm_shape_round_trips() {
    json_round_trip(&sample_item());

    let item = sample_item();
    let occurrence = Occurrence {
        item: item.id,
        recurrence_id: RecurrenceId::Instant(utc(2026, 6, 10, 7, 0)),
        span: TimeRange::new(utc(2026, 6, 10, 7, 0), utc(2026, 6, 10, 7, 45)).expect("range"),
        origin: OccurrenceOrigin::Overridden(OverrideId::new()),
    };
    json_round_trip(&occurrence);

    for change in [
        OverrideChange::Cancelled,
        OverrideChange::Rescheduled(utc_span(2026, 6, 11, 14, 0)),
    ] {
        let data = OccurrenceOverrideData {
            id: OverrideId::new(),
            item: item.id,
            recurrence_id: RecurrenceId::Instant(utc(2026, 6, 11, 7, 0)),
            change,
        };
        json_round_trip(&data);
    }

    for policy in [AlarmPolicy::at_start(), AlarmPolicy::minutes_before(30)] {
        json_round_trip(&policy);
    }
    let alarm = clipper_schedule::PlannedAlarm {
        item: item.id,
        recurrence_id: RecurrenceId::Floating(local("20260610T070000")),
        label: "ring".to_string(),
        fire_at: utc(2026, 6, 10, 6, 45),
        occurrence_start: utc(2026, 6, 10, 7, 0),
    };
    json_round_trip(&alarm);
}

/// Plain structs without internally-tagged enums round-trip through postcard
/// as well as JSON.
#[test]
fn postcard_supported_shapes_round_trip() {
    postcard_round_trip(&BlockDuration::from_minutes(90).expect("non-zero"));
    postcard_round_trip(&AlarmPolicy::minutes_before(15));
    postcard_round_trip(&WeekdaySet::weekdays());
    postcard_round_trip(
        &TimeRange::new(utc(2026, 6, 10, 9, 0), utc(2026, 6, 10, 10, 0)).expect("range"),
    );
}

/// The schedule enums use `#[serde(tag = ...)]`, which postcard serializes but
/// cannot deserialize (`WontImplement`: it has no content buffering). That is
/// why the wire, IPC and ciphertext formats must stay JSON: switching the
/// encrypted payload to postcard would make every schedule object unreadable.
#[test]
fn internally_tagged_enums_are_json_only() {
    let span = ScheduleSpan::Timed {
        start: TimedStart::Floating(local("20260610T070000")),
        duration: BlockDuration::from_minutes(45).expect("non-zero"),
    };
    let bytes = postcard::to_allocvec(&span).expect("postcard serializes");
    assert!(
        postcard::from_bytes::<ScheduleSpan>(&bytes).is_err(),
        "postcard must not silently decode an internally-tagged enum"
    );
}

/// Zero durations are rejected when deserializing, not only on construction.
#[test]
fn zero_duration_is_rejected_on_deserialize() {
    assert!(BlockDuration::from_minutes(0).is_err());
    let timed = serde_json::json!({
        "kind": "timed",
        "start": {"kind": "floating", "at": "2026-06-10T07:00:00"},
        "duration": 0,
    });
    assert!(serde_json::from_value::<ScheduleSpan>(timed).is_err());
}

/// A zero repeat interval is rejected when deserializing.
#[test]
fn zero_interval_is_rejected_on_deserialize() {
    assert!(Cadence::every(Frequency::Daily, 0).is_err());
    let cadence = serde_json::json!({
        "frequency": {"unit": "daily"},
        "interval": 0,
        "end": {"when": "never"},
    });
    assert!(serde_json::from_value::<Cadence>(cadence).is_err());
    let negative = serde_json::json!({
        "frequency": {"unit": "daily"},
        "interval": -1,
        "end": {"when": "never"},
    });
    assert!(serde_json::from_value::<Cadence>(negative).is_err());
}

/// A zero repeat count is rejected when deserializing.
#[test]
fn zero_count_is_rejected_on_deserialize() {
    assert!(RecurrenceEnd::after(0).is_err());
    let end = serde_json::json!({"when": "after", "value": 0});
    assert!(serde_json::from_value::<RecurrenceEnd>(end).is_err());
}

/// A zero-day all-day span is rejected when deserializing.
#[test]
fn zero_day_all_day_is_rejected_on_deserialize() {
    let span = serde_json::json!({
        "kind": "all_day",
        "start": "2026-06-10",
        "days": 0,
    });
    assert!(serde_json::from_value::<ScheduleSpan>(span).is_err());
}

/// An unknown or empty zone is rejected when deserializing.
#[test]
fn bad_zone_is_rejected_on_deserialize() {
    for zone in ["", "Mars/Olympus", "europe/berlin"] {
        let start = serde_json::json!({
            "kind": "zoned",
            "at": {"local": "2026-06-10T09:00:00", "zone": zone},
        });
        assert!(
            serde_json::from_value::<TimedStart>(start).is_err(),
            "zone {zone:?} must be rejected"
        );
    }
}
