//! Behaviour the corpus does not cover: the three time kinds, override
//! application, and the bounds that keep expansion from running away.

use std::num::NonZeroU32;

use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use clipper_schedule::{
    BlockDuration, Cadence, EngineError, Expansion, Frequency, MonthDay, NthWeekday, Occurrence,
    OccurrenceOrigin, OccurrenceOverride, OverrideChange, OverrideId, Recurrence, RecurrenceEngine,
    RecurrenceError, RecurrenceId, RruleEngine, ScheduleItem, ScheduleItemId, ScheduleSpan,
    TimeError, TimedStart, WeekdaySet, Window,
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
    overrides: &[OccurrenceOverride],
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
    let skipped = OccurrenceOverride {
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
    let cancelled = OccurrenceOverride {
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
    let moved = OccurrenceOverride {
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
    let pulled_forward = OccurrenceOverride {
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
    let someone_elses = OccurrenceOverride {
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
            week_start: chrono::Weekday::Mon,
        })),
        reference: None,
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
        json["recurrence"]["frequency"]["week_start"], "mon",
        "a lone weekday is spelled the same way a weekday set spells its members"
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
