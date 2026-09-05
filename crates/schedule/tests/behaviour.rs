//! Behaviour the corpus does not cover: the three time kinds, override
//! application, and the bounds that keep expansion from running away.

use std::num::NonZeroU32;

use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use clipper_schedule::{
    BlockDuration, Cadence, EngineError, Expansion, Frequency, MonthDay, NthWeekday, Occurrence,
    OccurrenceException, OccurrenceOrigin, OverrideChange, OverrideId, RawRule, Recurrence,
    RecurrenceEngine, RecurrenceError, RecurrenceId, RruleEngine, ScheduleItem, ScheduleItemId,
    ScheduleSpan, TimeError, TimedStart, WeekdaySet, Window,
};

fn local(text: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(text, "%Y%m%dT%H%M%S").expect("valid local datetime")
}

fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0)
        .single()
        .expect("unambiguous UTC instant")
}

fn daily_at(start: TimedStart) -> ScheduleItem {
    ScheduleItem {
        id: ScheduleItemId::new(),
        title: "daily".to_string(),
        span: ScheduleSpan::Timed {
            start,
            duration: BlockDuration::from_minutes(30).expect("30 is non-zero"),
        },
        recurrence: Recurrence::Every(Cadence::each(Frequency::Daily)),
        reference: None,
        alarm: None,
    }
}

fn expand(
    item: &ScheduleItem,
    overrides: &[OccurrenceException],
    expansion: &Expansion,
) -> Vec<Occurrence> {
    RruleEngine::new()
        .occurrences(item, overrides, expansion)
        .expect("expansion succeeds")
}

fn window(from: chrono::DateTime<Utc>, to: chrono::DateTime<Utc>) -> Window {
    Window::new(from, to).expect("non-empty window")
}

/// A floating alarm is a wall-clock promise, not an instant. 07:00 means 07:00
/// wherever the device happens to be, so the same series resolves to different
/// instants for different observers.
#[test]
fn floating_follows_the_observer() {
    let item = daily_at(TimedStart::Floating(local("20260610T070000")));
    let span = window(utc(2026, 6, 10, 0, 0), utc(2026, 6, 11, 0, 0));

    let berlin = expand(
        &item,
        &[],
        &Expansion {
            window: span,
            observer: Tz::Europe__Berlin,
        },
    );
    let tokyo = expand(
        &item,
        &[],
        &Expansion {
            window: span,
            observer: Tz::Asia__Tokyo,
        },
    );

    assert_eq!(berlin.len(), 1);
    assert_eq!(tokyo.len(), 1);
    for (occurrence, zone) in [
        (&berlin[0], Tz::Europe__Berlin),
        (&tokyo[0], Tz::Asia__Tokyo),
    ] {
        assert_eq!(
            occurrence
                .span
                .start
                .with_timezone(&zone)
                .format("%H:%M")
                .to_string(),
            "07:00",
            "a floating alarm must keep its wall-clock time"
        );
    }
    assert_ne!(
        berlin[0].span.start, tokyo[0].span.start,
        "different zones must give different instants"
    );
}

/// A zoned meeting is an instant. Travelling does not move it.
#[test]
fn zoned_stays_put_wherever_it_is_read() {
    let item = daily_at(TimedStart::Zoned {
        local: local("20260610T090000"),
        zone: Tz::Europe__Berlin,
    });
    let span = window(utc(2026, 6, 10, 0, 0), utc(2026, 6, 11, 0, 0));

    let read_in_berlin = expand(
        &item,
        &[],
        &Expansion {
            window: span,
            observer: Tz::Europe__Berlin,
        },
    );
    let read_in_tokyo = expand(
        &item,
        &[],
        &Expansion {
            window: span,
            observer: Tz::Asia__Tokyo,
        },
    );

    assert_eq!(read_in_berlin.len(), 1);
    assert_eq!(read_in_tokyo.len(), 1);
    assert_eq!(
        read_in_berlin[0].span.start, read_in_tokyo[0].span.start,
        "a zoned meeting is the same instant for every observer"
    );
}

/// A floating series is identified by wall-clock time, so an override recorded
/// on one device still matches the same occurrence on a device in another zone.
///
/// Identifying floating occurrences by instant instead would break this: the
/// same wall-clock morning is a different instant in each zone, so the override
/// would silently apply to the wrong day or to nothing at all.
#[test]
fn a_floating_override_matches_in_any_zone() {
    let item = daily_at(TimedStart::Floating(local("20260601T070000")));
    // Recorded on a device in Berlin: skip the morning of the 10th.
    let skipped = OccurrenceException {
        id: OverrideId::new(),
        item: item.id,
        recurrence_id: RecurrenceId::Floating(local("20260610T070000")),
        change: OverrideChange::Cancelled,
    };
    // Wide enough that every zone in the test sees the whole local day.
    let span = window(utc(2026, 6, 8, 0, 0), utc(2026, 6, 13, 0, 0));

    for observer in [
        Tz::Europe__Berlin,
        Tz::Asia__Tokyo,
        Tz::America__Los_Angeles,
    ] {
        let mornings: Vec<RecurrenceId> = expand(
            &item,
            std::slice::from_ref(&skipped),
            &Expansion {
                window: span,
                observer,
            },
        )
        .into_iter()
        .map(|occurrence| occurrence.recurrence_id)
        .collect();

        assert!(
            !mornings.contains(&RecurrenceId::Floating(local("20260610T070000"))),
            "the cancelled morning must stay cancelled when read from {observer}"
        );
        assert!(
            mornings.contains(&RecurrenceId::Floating(local("20260611T070000"))),
            "only the cancelled morning should be missing when read from {observer}"
        );
    }
}

#[test]
fn cancelled_occurrence_is_skipped() {
    let item = daily_at(TimedStart::Zoned {
        local: local("20260610T080000"),
        zone: Tz::UTC,
    });
    let cancelled = OccurrenceException {
        id: OverrideId::new(),
        item: item.id,
        recurrence_id: RecurrenceId::Instant(utc(2026, 6, 11, 8, 0)),
        change: OverrideChange::Cancelled,
    };

    let out = expand(
        &item,
        &[cancelled],
        &Expansion {
            window: window(utc(2026, 6, 10, 0, 0), utc(2026, 6, 13, 0, 0)),
            observer: Tz::UTC,
        },
    );

    let starts: Vec<_> = out.iter().map(|o| o.span.start).collect();
    assert_eq!(
        starts,
        vec![utc(2026, 6, 10, 8, 0), utc(2026, 6, 12, 8, 0)],
        "the 11th was cancelled and must not appear"
    );
}

#[test]
fn rescheduled_occurrence_reports_its_override() {
    let item = daily_at(TimedStart::Zoned {
        local: local("20260610T080000"),
        zone: Tz::UTC,
    });
    let moved = OccurrenceException {
        id: OverrideId::new(),
        item: item.id,
        recurrence_id: RecurrenceId::Instant(utc(2026, 6, 11, 8, 0)),
        change: OverrideChange::Rescheduled(ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local: local("20260611T140000"),
                zone: Tz::UTC,
            },
            duration: BlockDuration::from_minutes(45).expect("45 is non-zero"),
        }),
    };
    let override_id = moved.id;

    let out = expand(
        &item,
        &[moved],
        &Expansion {
            window: window(utc(2026, 6, 11, 0, 0), utc(2026, 6, 12, 0, 0)),
            observer: Tz::UTC,
        },
    );

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].span.start, utc(2026, 6, 11, 14, 0));
    assert_eq!(out[0].span.end, utc(2026, 6, 11, 14, 45));
    assert_eq!(out[0].origin, OccurrenceOrigin::Overridden(override_id));
    assert_eq!(
        out[0].recurrence_id,
        RecurrenceId::Instant(utc(2026, 6, 11, 8, 0)),
        "the recurrence id stays where the rule put it, even though the \
         occurrence moved"
    );
}

/// An occurrence whose rule position is outside the window can still be moved
/// into it. Walking rule positions alone would miss this.
#[test]
fn override_can_move_an_occurrence_into_the_window() {
    let item = daily_at(TimedStart::Zoned {
        local: local("20260601T080000"),
        zone: Tz::UTC,
    });
    let pulled_forward = OccurrenceException {
        id: OverrideId::new(),
        item: item.id,
        // The 11th, which the window below does not contain.
        recurrence_id: RecurrenceId::Instant(utc(2026, 6, 11, 8, 0)),
        change: OverrideChange::Rescheduled(ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local: local("20260610T090000"),
                zone: Tz::UTC,
            },
            duration: BlockDuration::from_minutes(30).expect("30 is non-zero"),
        }),
    };

    let out = expand(
        &item,
        &[pulled_forward],
        &Expansion {
            window: window(utc(2026, 6, 10, 0, 0), utc(2026, 6, 10, 12, 0)),
            observer: Tz::UTC,
        },
    );

    let starts: Vec<_> = out.iter().map(|o| o.span.start).collect();
    assert_eq!(
        starts,
        vec![utc(2026, 6, 10, 8, 0), utc(2026, 6, 10, 9, 0)],
        "the moved occurrence belongs in this window even though its rule \
         position does not"
    );
}

#[test]
fn overrides_for_other_items_are_ignored() {
    let item = daily_at(TimedStart::Zoned {
        local: local("20260610T080000"),
        zone: Tz::UTC,
    });
    let someone_elses = OccurrenceException {
        id: OverrideId::new(),
        item: ScheduleItemId::new(),
        recurrence_id: RecurrenceId::Instant(utc(2026, 6, 10, 8, 0)),
        change: OverrideChange::Cancelled,
    };

    let out = expand(
        &item,
        &[someone_elses],
        &Expansion {
            window: window(utc(2026, 6, 10, 0, 0), utc(2026, 6, 11, 0, 0)),
            observer: Tz::UTC,
        },
    );

    assert_eq!(out.len(), 1, "another item's override must not apply here");
}

/// An all-day event is a date. Its extent depends on where it is read, and it
/// is not 24 hours across a DST boundary.
#[test]
fn all_day_spans_whole_local_days() {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "conference".to_string(),
        span: ScheduleSpan::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 3, 29).expect("valid date"),
            days: NonZeroU32::new(1).expect("1 is non-zero"),
        },
        recurrence: Recurrence::Once,
        reference: None,
        alarm: None,
    };

    let resolved = item
        .span
        .resolve(Tz::Europe__Berlin)
        .expect("all-day spans resolve");

    assert_eq!(
        (resolved.end - resolved.start).num_hours(),
        23,
        "29 March 2026 is a 23-hour day in Berlin; an all-day event is a date, \
         not a fixed number of hours"
    );
}

#[test]
fn one_off_items_expand_to_themselves() {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "dentist".to_string(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local: local("20260610T150000"),
                zone: Tz::UTC,
            },
            duration: BlockDuration::from_minutes(30).expect("30 is non-zero"),
        },
        recurrence: Recurrence::Once,
        reference: None,
        alarm: None,
    };

    let inside = expand(
        &item,
        &[],
        &Expansion {
            window: window(utc(2026, 6, 10, 0, 0), utc(2026, 6, 11, 0, 0)),
            observer: Tz::UTC,
        },
    );
    let outside = expand(
        &item,
        &[],
        &Expansion {
            window: window(utc(2026, 6, 11, 0, 0), utc(2026, 6, 12, 0, 0)),
            observer: Tz::UTC,
        },
    );

    assert_eq!(inside.len(), 1);
    assert_eq!(inside[0].span.start, utc(2026, 6, 10, 15, 0));
    assert!(outside.is_empty());
}

#[test]
fn a_half_hour_dst_gap_keeps_the_position_within_the_gap() {
    let start = TimedStart::Zoned {
        local: local("20261004T021500"),
        zone: Tz::Australia__Lord_Howe,
    };
    let resolved = start.resolve(Tz::UTC).expect("gap shifts forward");
    assert_eq!(
        resolved
            .with_timezone(&Tz::Australia__Lord_Howe)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        "2026-10-04 02:45",
        "Lord Howe advances by 30 minutes, so 02:15 becomes 02:45"
    );
}

#[test]
fn a_skipped_civil_date_shifts_by_the_full_transition() {
    let start = TimedStart::Zoned {
        local: local("20111230T120000"),
        zone: Tz::Pacific__Apia,
    };
    let resolved = start.resolve(Tz::UTC).expect("date skip shifts forward");
    assert_eq!(
        resolved
            .with_timezone(&Tz::Pacific__Apia)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        "2011-12-31 12:00",
        "Samoa's skipped Friday was a 24-hour gap"
    );
}

#[test]
fn a_narrow_window_does_not_count_decades_of_series_history() {
    let item = daily_at(TimedStart::Zoned {
        local: local("19900101T080000"),
        zone: Tz::UTC,
    });
    let out = expand(
        &item,
        &[],
        &Expansion {
            window: window(utc(2026, 6, 10, 0, 0), utc(2026, 6, 12, 0, 0)),
            observer: Tz::UTC,
        },
    );
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].span.start, utc(2026, 6, 10, 8, 0));
}

#[test]
fn a_dense_old_rule_hits_the_history_scan_bound() {
    let mut item = daily_at(TimedStart::Zoned {
        local: local("20260101T000000"),
        zone: Tz::UTC,
    });
    item.recurrence = Recurrence::Raw {
        rule: RawRule::new("FREQ=MINUTELY").expect("valid raw rule"),
    };

    let result = RruleEngine::new().occurrences(
        &item,
        &[],
        &Expansion {
            window: window(utc(2026, 4, 1, 0, 0), utc(2026, 4, 1, 1, 0)),
            observer: Tz::UTC,
        },
    );
    assert_eq!(
        result,
        Err(EngineError::ScanLimitExceeded { limit: 65_535 })
    );
}

#[test]
fn an_impossible_finite_raw_rule_is_empty() {
    let mut item = daily_at(TimedStart::Zoned {
        local: local("20260101T000000"),
        zone: Tz::UTC,
    });
    item.recurrence = Recurrence::Raw {
        rule: RawRule::new("FREQ=YEARLY;COUNT=2;BYMONTH=2;BYMONTHDAY=30")
            .expect("syntactically valid raw rule"),
    };

    let occurrences = RruleEngine::new()
        .occurrences(
            &item,
            &[],
            &Expansion {
                window: window(utc(2030, 1, 1, 0, 0), utc(2031, 1, 1, 0, 0)),
                observer: Tz::UTC,
            },
        )
        .expect("an impossible finite rule has no occurrences");
    assert!(occurrences.is_empty());
}

#[test]
fn an_overnight_occurrence_overlaps_the_next_days_window() {
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
    let expansion = Expansion {
        window: window(utc(2026, 6, 10, 0, 0), utc(2026, 6, 10, 1, 0)),
        observer: Tz::UTC,
    };

    assert!(
        RruleEngine::new()
            .occurrences(&item, &[], &expansion)
            .expect("expands")
            .is_empty()
    );
    let overlapping = RruleEngine::new()
        .overlapping_occurrences(&item, &[], &expansion)
        .expect("overlap expansion succeeds");
    assert_eq!(overlapping.len(), 1);
    assert_eq!(overlapping[0].span.end, utc(2026, 6, 10, 1, 30));
}

#[test]
fn a_multi_day_occurrence_overlaps_a_window_across_dst() {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "dst weekend".to_string(),
        span: ScheduleSpan::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 3, 28).expect("valid date"),
            days: NonZeroU32::new(2).expect("non-zero"),
        },
        recurrence: Recurrence::Once,
        reference: None,
        alarm: None,
    };
    let expansion = Expansion {
        window: window(utc(2026, 3, 29, 0, 0), utc(2026, 3, 29, 12, 0)),
        observer: Tz::Europe__Berlin,
    };

    let overlapping = RruleEngine::new()
        .overlapping_occurrences(&item, &[], &expansion)
        .expect("overlap expansion succeeds");
    assert_eq!(overlapping.len(), 1);
    assert_eq!(
        (overlapping[0].span.end - overlapping[0].span.start).num_hours(),
        47,
        "two local days around spring-forward last 47 elapsed hours"
    );
}

#[test]
fn a_longer_override_can_overlap_from_before_the_window() {
    let item = daily_at(TimedStart::Zoned {
        local: local("20260601T080000"),
        zone: Tz::UTC,
    });
    let moved = OccurrenceException {
        id: OverrideId::new(),
        item: item.id,
        recurrence_id: RecurrenceId::Instant(utc(2026, 6, 9, 8, 0)),
        change: OverrideChange::Rescheduled(ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local: local("20260609T233000"),
                zone: Tz::UTC,
            },
            duration: BlockDuration::from_minutes(120).expect("non-zero"),
        }),
    };
    let expansion = Expansion {
        window: window(utc(2026, 6, 10, 0, 0), utc(2026, 6, 10, 1, 0)),
        observer: Tz::UTC,
    };

    let overlapping = RruleEngine::new()
        .overlapping_occurrences(&item, &[moved], &expansion)
        .expect("overlap expansion succeeds");
    assert_eq!(overlapping.len(), 1);
    assert_eq!(overlapping[0].span.start, utc(2026, 6, 9, 23, 30));
}

/// Running out of candidates is an error, never a short answer. A silently
/// truncated expansion would drop alarms.
#[test]
fn expansion_limit_errors_rather_than_truncating() {
    let item = daily_at(TimedStart::Zoned {
        local: local("20260101T080000"),
        zone: Tz::UTC,
    });

    let result = RruleEngine::with_max_candidates(5).occurrences(
        &item,
        &[],
        &Expansion {
            window: window(utc(2026, 1, 1, 0, 0), utc(2026, 12, 31, 0, 0)),
            observer: Tz::UTC,
        },
    );

    assert_eq!(
        result,
        Err(EngineError::ExpansionLimitExceeded { limit: 5 })
    );
}

#[test]
fn overlap_expansion_keeps_the_candidate_limit() {
    let mut item = daily_at(TimedStart::Zoned {
        local: local("20260101T080000"),
        zone: Tz::UTC,
    });
    let ScheduleSpan::Timed { duration, .. } = &mut item.span else {
        unreachable!();
    };
    *duration = BlockDuration::from_minutes(7 * 24 * 60).expect("non-zero");

    let result = RruleEngine::with_max_candidates(5).overlapping_occurrences(
        &item,
        &[],
        &Expansion {
            window: window(utc(2026, 2, 1, 0, 0), utc(2026, 2, 2, 0, 0)),
            observer: Tz::UTC,
        },
    );
    assert_eq!(
        result,
        Err(EngineError::ExpansionLimitExceeded { limit: 5 })
    );
}

#[test]
fn exactly_the_candidate_limit_is_complete() {
    let item = daily_at(TimedStart::Zoned {
        local: local("20260101T080000"),
        zone: Tz::UTC,
    });
    let occurrences = RruleEngine::with_max_candidates(5)
        .occurrences(
            &item,
            &[],
            &Expansion {
                window: window(utc(2026, 1, 1, 0, 0), utc(2026, 1, 6, 0, 0)),
                observer: Tz::UTC,
            },
        )
        .expect("an exact-size result is complete");
    assert_eq!(occurrences.len(), 5);
}

#[test]
fn an_empty_window_is_rejected() {
    let instant = utc(2026, 6, 10, 0, 0);
    assert_eq!(
        Window::new(instant, instant),
        Err(EngineError::EmptyWindow {
            from: instant,
            to: instant
        })
    );
}

#[test]
fn invalid_domain_values_cannot_be_constructed() {
    assert_eq!(BlockDuration::from_minutes(0), Err(TimeError::ZeroDuration));
    assert_eq!(WeekdaySet::new(&[]), Err(RecurrenceError::EmptyWeekdaySet));
    assert_eq!(
        MonthDay::from_start(32),
        Err(RecurrenceError::MonthDayOutOfRange(32))
    );
    assert_eq!(
        MonthDay::from_end(0),
        Err(RecurrenceError::MonthDayOutOfRange(0))
    );
    assert_eq!(
        NthWeekday::from_start(6),
        Err(RecurrenceError::WeekdayOrdinalOutOfRange(6))
    );
    assert_eq!(
        Cadence::every(Frequency::Daily, 0),
        Err(RecurrenceError::ZeroInterval)
    );
}

/// The serialized form is three things at once: the TypeScript contract, the
/// IPC payload, and the ciphertext on disk. Pin it, so a stray serde attribute
/// cannot silently change a format that encrypted records are already written
/// in.
#[test]
fn the_wire_format_is_self_describing() {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "Gym".to_string(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Floating(local("20260610T070000")),
            duration: BlockDuration::from_minutes(45).expect("non-zero"),
        },
        recurrence: Recurrence::Every(Cadence::each(Frequency::Weekly {
            weekdays: WeekdaySet::weekdays(),
        })),
        reference: Some(clipper_api_types::ObjectId::from(uuid::Uuid::new_v4())),
        alarm: None,
    };

    let json = serde_json::to_value(&item).expect("serialize");
    assert_eq!(json["span"]["kind"], "timed");
    assert_eq!(json["span"]["start"]["kind"], "floating");
    assert_eq!(json["span"]["start"]["at"], "2026-06-10T07:00:00");
    assert_eq!(json["span"]["duration"], 45);
    assert_eq!(json["recurrence"]["kind"], "every");
    assert_eq!(json["recurrence"]["interval"], 1);
    assert_eq!(json["recurrence"]["end"]["when"], "never");
    assert_eq!(json["recurrence"]["frequency"]["unit"], "weekly");
    assert_eq!(
        json["recurrence"]["frequency"]["weekdays"],
        serde_json::json!(["mon", "tue", "wed", "thu", "fri"]),
        "a weekday set travels as day names, never as its internal bitmask"
    );

    assert_eq!(
        json["reference"],
        item.reference.expect("reference").to_string(),
        "a reference contains only the target UUID"
    );

    let back: ScheduleItem = serde_json::from_value(json).expect("round trip");
    assert_eq!(back, item);
}

/// The raw variant is the one a feed produces, and it broke serialization the
/// first time it was tried for real: serde cannot internally tag a newtype
/// wrapping a string. Pin its shape alongside the typed cadence.
#[test]
fn a_raw_rule_serializes_under_the_same_tag() {
    use clipper_schedule::RawRule;

    let recurrence = Recurrence::Raw {
        rule: RawRule::new("FREQ=WEEKLY;BYDAY=MO,WE").expect("valid rule"),
    };
    let json = serde_json::to_value(&recurrence).expect("serialize");
    assert_eq!(json["kind"], "raw");
    assert_eq!(json["rule"], "FREQ=WEEKLY;BYDAY=MO,WE");

    let back: Recurrence = serde_json::from_value(json).expect("round trip");
    assert_eq!(back, recurrence);

    // Validation runs on the way back in, not only at construction.
    assert!(
        serde_json::from_value::<Recurrence>(
            serde_json::json!({"kind": "raw", "rule": "NOT A RULE"})
        )
        .is_err(),
        "an unparseable rule must be rejected on deserialize too"
    );
}

/// A raw rule is one property, and expansion splices it verbatim after
/// `RRULE:` against the item's real DTSTART and zone. Validation parses a whole
/// `RRuleSet`, so a value carrying its own line break used to validate as the
/// probe's DTSTART plus a bonus property, and that property then went live
/// against a start it was never checked with. Both smuggling shapes matter: an
/// `EXDATE` silently deletes occurrences, a second `RRULE` silently adds them.
#[test]
fn a_raw_rule_cannot_smuggle_a_second_property() {
    use clipper_schedule::RawRule;

    for smuggled in [
        "FREQ=DAILY;COUNT=10\nEXDATE:20240102T090000Z",
        "FREQ=DAILY;COUNT=2\nRRULE:FREQ=MONTHLY;COUNT=5",
        "FREQ=DAILY;COUNT=2\rEXDATE:20240102T090000Z",
    ] {
        assert!(
            RawRule::new(smuggled).is_err(),
            "a folded line must not validate: {smuggled:?}"
        );
        assert!(
            serde_json::from_value::<Recurrence>(
                serde_json::json!({"kind": "raw", "rule": smuggled})
            )
            .is_err(),
            "a synced record must not carry a folded line either: {smuggled:?}"
        );
    }

    // The legitimate value is unaffected: only control characters are refused,
    // and surrounding whitespace is still trimmed rather than rejected.
    assert_eq!(
        RawRule::new("  RRULE:FREQ=DAILY;COUNT=2  ")
            .expect("a padded rule stays valid")
            .as_str(),
        "FREQ=DAILY;COUNT=2"
    );
}

#[test]
fn descriptive_edits_preserve_exception_applicability() {
    let original = daily_at(TimedStart::Floating(local("20260915T070000")));
    let mut edited = original.clone();
    edited.title = "Renamed workout".to_owned();
    edited.reference = Some(clipper_api_types::ObjectId::from(uuid::Uuid::new_v4()));
    edited.alarm = Some(clipper_schedule::AlarmPolicy::minutes_before(10));
    assert!(original.exceptions_compatible_with(&edited));
}

#[test]
fn structural_edits_require_reconsidering_exceptions() {
    let original = daily_at(TimedStart::Floating(local("20260915T070000")));
    let mut changed_rule = original.clone();
    changed_rule.recurrence = Recurrence::Once;
    let mut changed_start = original.clone();
    changed_start.span = ScheduleSpan::Timed {
        start: TimedStart::Floating(local("20260915T080000")),
        duration: BlockDuration::from_minutes(30).unwrap(),
    };
    let mut changed_duration = original.clone();
    changed_duration.span = ScheduleSpan::Timed {
        start: TimedStart::Floating(local("20260915T070000")),
        duration: BlockDuration::from_minutes(45).unwrap(),
    };
    let mut changed_zone = original.clone();
    changed_zone.span = ScheduleSpan::Timed {
        start: TimedStart::Zoned {
            local: local("20260915T070000"),
            zone: Tz::Europe__Berlin,
        },
        duration: BlockDuration::from_minutes(30).unwrap(),
    };
    let mut different_series = original.clone();
    different_series.id = ScheduleItemId::new();
    for edited in [
        changed_rule,
        changed_start,
        changed_duration,
        changed_zone,
        different_series,
    ] {
        assert!(!original.exceptions_compatible_with(&edited));
    }
}

#[test]
fn historical_plan_retains_original_identity_and_resolved_override_after_travel() {
    use clipper_schedule::{ActualId, ActualRecord, ActualSpan, ObjectRevisionRef, PlannedRef};

    let item = daily_at(TimedStart::Floating(local("20260915T070000")));
    let moved_span = ScheduleSpan::Timed {
        start: TimedStart::Floating(local("20260916T090000")),
        duration: BlockDuration::from_minutes(30).unwrap(),
    };
    let planned = PlannedRef {
        item: item.id,
        recurrence_id: RecurrenceId::Floating(local("20260915T070000")),
        schedule: ObjectRevisionRef {
            object_id: clipper_api_types::ObjectId::from(uuid::Uuid::new_v4()),
            revision: 3,
            body_hash: [3; 32],
        },
        override_revision: Some(ObjectRevisionRef {
            object_id: clipper_api_types::ObjectId::from(uuid::Uuid::new_v4()),
            revision: 2,
            body_hash: [2; 32],
        }),
        observer: Tz::Europe__Berlin,
        span: moved_span.resolve(Tz::Europe__Berlin).unwrap(),
    };
    let actual = ActualRecord {
        id: ActualId::new(),
        planned: Some(planned),
        span: ActualSpan::Running {
            started: utc(2026, 9, 16, 7, 5),
        },
    };
    let restored: ActualRecord =
        serde_json::from_str(&serde_json::to_string(&actual).unwrap()).unwrap();
    assert_eq!(restored, actual);
    assert_eq!(restored.planned.unwrap().span.start, utc(2026, 9, 16, 7, 0));
    assert_ne!(
        restored.planned.unwrap().span,
        moved_span.resolve(Tz::Asia__Tokyo).unwrap()
    );
    assert_eq!(
        restored.planned.unwrap().recurrence_id,
        RecurrenceId::Floating(local("20260915T070000"))
    );
}
