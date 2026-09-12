//! Parsing a real-shaped iCalendar feed.
//!
//! The fixture holds a zoned meeting, a UTC one, a floating one, a single-day
//! and a multi-day all-day event, a recurring series, a cancellation, and two
//! entries that cannot be read at all. Every one of those appears in an
//! ordinary Google or Zoho export.

use chrono::{NaiveDate, NaiveTime, Weekday};
use chrono_tz::Tz;
use clipper_schedule::{
    Frequency, IngestedStatus, Recurrence, RecurrenceEnd, ScheduleSpan, SourceId, TimedStart,
    WeekdaySet, parse_ics, parse_imported_recurrence_rules,
};

const FEED: &str = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//Test//Feed//EN\r\n\
BEGIN:VEVENT\r\n\
UID:standup@example.com\r\n\
SUMMARY:Standup\r\n\
DESCRIPTION:Daily sync\r\n\
DTSTART;TZID=Europe/Berlin:20260907T093000\r\n\
DTEND;TZID=Europe/Berlin:20260907T094500\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:utc-call@example.com\r\n\
SUMMARY:Vendor call\r\n\
DTSTART:20260908T140000Z\r\n\
DTEND:20260908T150000Z\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:floating@example.com\r\n\
SUMMARY:Morning pages\r\n\
DTSTART:20260909T070000\r\n\
DTEND:20260909T073000\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:holiday@example.com\r\n\
SUMMARY:Public holiday\r\n\
DTSTART;VALUE=DATE:20261003\r\n\
DTEND;VALUE=DATE:20261004\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:conference@example.com\r\n\
SUMMARY:Conference\r\n\
DTSTART;VALUE=DATE:20261012\r\n\
DTEND;VALUE=DATE:20261015\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:cancelled@example.com\r\n\
SUMMARY:Cancelled thing\r\n\
STATUS:CANCELLED\r\n\
DTSTART;TZID=Europe/Berlin:20260910T110000\r\n\
DTEND;TZID=Europe/Berlin:20260910T113000\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:No uid at all\r\n\
DTSTART:20260911T090000Z\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:no-start@example.com\r\n\
SUMMARY:No start\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

fn parse() -> clipper_schedule::IngestOutcome {
    parse_ics(FEED, SourceId(uuid_fixture()), import_fixture()).expect("the feed parses")
}

/// A fixed source id, so derived event ids are stable across runs.
fn uuid_fixture() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888)
}

fn import_fixture() -> clipper_api_types::ObjectId {
    uuid::Uuid::from_u128(0xaaaa_bbbb_cccc_dddd_eeee_ffff_0000_1111).into()
}

fn event(uid: &str) -> clipper_schedule::IngestedEvent {
    parse()
        .events
        .into_iter()
        .find(|event| event.uid == uid)
        .unwrap_or_else(|| panic!("no event with uid {uid}"))
}

#[test]
fn readable_events_are_kept_and_unreadable_ones_are_reported() {
    let outcome = parse();
    assert_eq!(outcome.events.len(), 6, "six events are readable");
    assert_eq!(outcome.skipped.len(), 2, "two cannot be read");

    // An entry that cannot be read is reported, never dropped silently.
    let reasons: Vec<&str> = outcome
        .skipped
        .iter()
        .map(|skipped| skipped.reason.as_str())
        .collect();
    assert!(reasons.iter().any(|reason| reason.contains("UID")));
    assert!(reasons.iter().any(|reason| reason.contains("DTSTART")));
}

#[test]
fn a_tzid_start_stays_pinned_to_its_zone() {
    let standup = event("standup@example.com");
    let ScheduleSpan::Timed { start, duration } = &standup.span else {
        panic!("standup is a timed event");
    };
    assert_eq!(
        start,
        &TimedStart::Zoned {
            local: NaiveDate::from_ymd_opt(2026, 9, 7)
                .expect("valid")
                .and_time(NaiveTime::from_hms_opt(9, 30, 0).expect("valid")),
            zone: Tz::Europe__Berlin,
        },
        "a meeting with a TZID must not become floating — it would move when \
         the owner travels"
    );
    assert_eq!(duration.minutes(), 15);
    assert_eq!(standup.title, "Standup");
    assert_eq!(standup.description.as_deref(), Some("Daily sync"));
}

#[test]
fn a_z_suffix_is_utc_rather_than_floating() {
    let call = event("utc-call@example.com");
    let ScheduleSpan::Timed { start, duration } = &call.span else {
        panic!("timed");
    };
    assert!(
        matches!(start, TimedStart::Zoned { zone: Tz::UTC, .. }),
        "a trailing Z means UTC, not 'no zone'"
    );
    assert_eq!(duration.minutes(), 60);
}

/// RFC 5545 floating time, with no TZID and no Z, means what Clipper's
/// floating means, so it maps straight across.
#[test]
fn a_bare_local_start_stays_floating() {
    let pages = event("floating@example.com");
    let ScheduleSpan::Timed { start, .. } = &pages.span else {
        panic!("timed");
    };
    assert!(matches!(start, TimedStart::Floating(_)));
}

#[test]
fn all_day_events_are_dates_and_dtend_is_exclusive() {
    let holiday = event("holiday@example.com");
    assert_eq!(
        holiday.span,
        ScheduleSpan::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 10, 3).expect("valid"),
            days: std::num::NonZeroU32::new(1).expect("non-zero"),
        },
        "DTEND is exclusive, so 3rd to 4th is one day, not two"
    );

    let conference = event("conference@example.com");
    assert_eq!(
        conference.span,
        ScheduleSpan::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 10, 12).expect("valid"),
            days: std::num::NonZeroU32::new(3).expect("non-zero"),
        },
    );
}

/// All-day events remain visible in the planner instead of being filtered out.
#[test]
fn all_day_events_are_not_filtered_out() {
    let outcome = parse();
    assert!(
        outcome
            .events
            .iter()
            .any(|event| matches!(event.span, ScheduleSpan::AllDay { .. })),
        "all-day events must survive ingest"
    );
}

#[test]
fn a_cancelled_event_is_tombstoned_not_dropped() {
    let cancelled = event("cancelled@example.com");
    assert_eq!(
        cancelled.status,
        IngestedStatus::Cancelled,
        "cancelled upstream means tombstoned here, so time already logged \
         against it survives"
    );
}

#[test]
fn a_representable_ingested_rule_becomes_an_editable_cadence() {
    let standup = event("standup@example.com");
    let Recurrence::Every(cadence) = &standup.recurrence else {
        panic!("a representable imported rule should become editable");
    };
    assert_eq!(cadence.interval.get(), 1);
    assert_eq!(cadence.end, RecurrenceEnd::Never);
    assert_eq!(
        cadence.frequency,
        Frequency::Weekly {
            weekdays: WeekdaySet::new(&[
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ])
            .expect("non-empty weekday set"),
        }
    );
}

#[test]
fn events_without_a_rule_happen_once() {
    assert_eq!(event("utc-call@example.com").recurrence, Recurrence::Once);
}

/// Re-ingesting the same feed updates events in place instead of piling up
/// duplicates, because each id is derived from its source and provider uid.
#[test]
fn ids_are_stable_across_passes_and_distinct_per_source() {
    let first = parse();
    let second = parse();
    let ids = |outcome: &clipper_schedule::IngestOutcome| {
        let mut ids: Vec<_> = outcome.events.iter().map(|event| event.id).collect();
        ids.sort();
        ids
    };
    assert_eq!(
        ids(&first),
        ids(&second),
        "ids must not change between passes"
    );

    let other_source = parse_ics(
        FEED,
        SourceId(uuid::Uuid::from_u128(0xDEAD_BEEF)),
        import_fixture(),
    )
    .expect("parses")
    .events;
    assert!(
        other_source
            .iter()
            .all(|event| !ids(&first).contains(&event.id)),
        "the same feed under a second source must not collide with the first"
    );
}

/// A rule Clipper cannot model still expands, through its import snapshot.
#[test]
fn an_ingested_series_expands() {
    use chrono::{TimeZone, Utc};
    use clipper_schedule::{
        Expansion, RecurrenceEngine, RruleEngine, ScheduleItem, ScheduleItemId, TimeRange,
    };

    let standup = event("standup@example.com");
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: standup.title.clone(),
        span: standup.span.clone(),
        recurrence: standup.recurrence.clone(),
        reference: None,
        alarm: None,
    };

    let from = Utc
        .with_ymd_and_hms(2026, 9, 7, 0, 0, 0)
        .single()
        .expect("valid");
    let occurrences = RruleEngine::new()
        .occurrences(
            &item,
            &[],
            &Expansion {
                window: TimeRange::new(from, from + chrono::TimeDelta::days(7)).expect("window"),
                observer: Tz::Europe__Berlin,
            },
        )
        .expect("an ingested rule expands");

    assert_eq!(occurrences.len(), 5, "Mon to Fri");
    assert_eq!(
        occurrences[0]
            .span
            .start()
            .with_timezone(&Tz::Europe__Berlin)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        "2026-09-07 09:30",
    );
}

#[test]
fn garbage_and_incomplete_calendars_are_rejected_but_an_empty_snapshot_is_valid() {
    let source = SourceId(uuid_fixture());
    for invalid in [
        "this is not a calendar",
        "<html><body>sign in</body></html>",
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
    ] {
        assert!(
            parse_ics(invalid, source, import_fixture()).is_err(),
            "must reject {invalid:?} instead of treating it as an empty snapshot"
        );
    }

    let empty = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n";
    let outcome =
        parse_ics(empty, source, import_fixture()).expect("an explicit empty snapshot is valid");
    assert!(outcome.events.is_empty());
    assert!(outcome.skipped.is_empty());
}

#[test]
fn parser_has_an_input_size_ceiling() {
    let mut oversized = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n".to_string();
    oversized.push_str(&" ".repeat(8 * 1024 * 1024));
    assert!(parse_ics(&oversized, SourceId(uuid_fixture()), import_fixture()).is_err());
}

#[test]
fn a_feed_with_too_many_lines_is_rejected_before_parsing() {
    // MAX_PROPERTIES + 2 * MAX_COMPONENTS + 1, counting the envelope lines.
    let total_lines = 500_000 + 2 * 50_000 + 1;
    let mut feed = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n");
    for _ in 0..total_lines - 3 {
        feed.push_str("X-A:1\r\n");
    }
    feed.push_str("END:VCALENDAR\r\n");
    assert_eq!(feed.lines().count(), total_lines);
    assert!(
        feed.len() < 8 * 1024 * 1024,
        "must not hit the size ceiling"
    );
    match parse_ics(&feed, SourceId(uuid_fixture()), import_fixture()) {
        Err(clipper_schedule::IngestError::LimitExceeded(message)) => {
            assert_eq!(message, "too many calendar lines");
        }
        other => panic!("expected the line cap, got {other:?}"),
    }
}

#[test]
fn an_unknown_tzid_skips_the_event_instead_of_becoming_floating() {
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:unknown-zone@example.com\r\nSUMMARY:Unknown zone\r\n\
DTSTART;TZID=Mars/Olympus_Mons:20260908T090000\r\n\
DTEND;TZID=Mars/Olympus_Mons:20260908T100000\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";
    let outcome = parse_ics(feed, SourceId(uuid_fixture()), import_fixture())
        .expect("the feed itself parses");
    assert!(outcome.events.is_empty());
    assert_eq!(outcome.skipped.len(), 1);
    assert!(outcome.skipped[0].reason.contains("Mars/Olympus_Mons"));
}

#[test]
fn durations_use_instants_across_zones_and_dst() {
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:mixed@example.com\r\nDTSTART;TZID=America/New_York:20260115T090000\r\n\
DTEND:20260115T150000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\n\
UID:dst@example.com\r\nDTSTART;TZID=Europe/Berlin:20260329T013000\r\n\
DTEND;TZID=Europe/Berlin:20260329T033000\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";
    let outcome = parse_ics(feed, SourceId(uuid_fixture()), import_fixture()).expect("feed parses");
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    for event in outcome.events {
        let ScheduleSpan::Timed { duration, .. } = event.span else {
            panic!("timed fixture");
        };
        assert_eq!(duration.minutes(), 60, "{} uses elapsed time", event.uid);
    }
}

#[test]
fn duration_properties_are_honoured_for_timed_and_all_day_events() {
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:timed-duration@example.com\r\nDTSTART:20260908T090000Z\r\n\
DURATION:PT1H45M\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\n\
UID:day-duration@example.com\r\nDTSTART;VALUE=DATE:20260908\r\n\
DURATION:P2D\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let outcome = parse_ics(feed, SourceId(uuid_fixture()), import_fixture()).expect("feed parses");
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);

    let timed = outcome
        .events
        .iter()
        .find(|event| event.uid.starts_with("timed"))
        .expect("timed event");
    let ScheduleSpan::Timed { duration, .. } = timed.span else {
        panic!("timed span");
    };
    assert_eq!(duration.minutes(), 105);

    let all_day = outcome
        .events
        .iter()
        .find(|event| event.uid.starts_with("day"))
        .expect("all-day event");
    let ScheduleSpan::AllDay { days, .. } = all_day.span else {
        panic!("all-day span");
    };
    assert_eq!(days.get(), 2);
}

#[test]
fn exdate_rdate_and_recurrence_id_components_become_overrides() {
    use chrono::{TimeZone, Utc};
    use clipper_schedule::{
        Expansion, RecurrenceEngine, RruleEngine, ScheduleItem, ScheduleItemId, TimeRange,
    };

    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:series@example.com\r\nSUMMARY:Series\r\nDTSTART:20260901T090000Z\r\n\
DTEND:20260901T100000Z\r\nRRULE:FREQ=DAILY;COUNT=4\r\n\
EXDATE:20260902T090000Z\r\nRDATE:20260905T090000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:series@example.com\r\nRECURRENCE-ID:20260903T090000Z\r\n\
DTSTART:20260903T140000Z\r\nDTEND:20260903T150000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:series@example.com\r\nRECURRENCE-ID:20260904T090000Z\r\n\
STATUS:CANCELLED\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";
    let source = SourceId(uuid_fixture());
    let event = parse_ics(feed, source, import_fixture())
        .expect("feed parses")
        .events
        .pop()
        .expect("master event");
    assert_eq!(event.overrides.len(), 4);
    assert_eq!(
        event,
        parse_ics(feed, source, import_fixture())
            .expect("second pass parses")
            .events
            .pop()
            .expect("master event"),
        "derived override ids must keep unchanged refreshes unchanged"
    );

    let series = ScheduleItem {
        id: ScheduleItemId(event.id),
        title: event.title,
        span: event.span,
        recurrence: event.recurrence,
        reference: None,
        alarm: None,
    };
    let from = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).single().unwrap();
    let occurrences = RruleEngine::new()
        .occurrences(
            &series,
            &event.overrides,
            &Expansion {
                window: TimeRange::new(from, from + chrono::TimeDelta::days(7)).unwrap(),
                observer: Tz::UTC,
            },
        )
        .expect("overrides expand");
    let starts: Vec<_> = occurrences
        .iter()
        .map(|occurrence| occurrence.span.start().format("%Y-%m-%d %H:%M").to_string())
        .collect();
    assert_eq!(
        starts,
        ["2026-09-01 09:00", "2026-09-03 14:00", "2026-09-05 09:00"]
    );
}

#[test]
fn an_unsupported_range_override_skips_its_whole_series() {
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:range@example.com\r\nDTSTART:20260901T090000Z\r\nRRULE:FREQ=DAILY\r\n\
END:VEVENT\r\nBEGIN:VEVENT\r\nUID:range@example.com\r\n\
RECURRENCE-ID;RANGE=THISANDFUTURE:20260903T090000Z\r\n\
DTSTART:20260903T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let outcome = parse_ics(feed, SourceId(uuid_fixture()), import_fixture()).expect("feed parses");
    assert!(
        outcome.events.is_empty(),
        "the affected series is unsafe to show"
    );
    assert_eq!(outcome.skipped.len(), 1);
    assert!(outcome.skipped[0].reason.contains("THISANDFUTURE"));
}

#[test]
fn exdate_takes_precedence_over_the_same_rdate() {
    use clipper_schedule::OverrideChange;

    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:collision@example.com\r\nDTSTART:20260901T090000Z\r\n\
RRULE:FREQ=DAILY;COUNT=2\r\nRDATE:20260905T090000Z\r\n\
EXDATE:20260905T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let event = parse_ics(feed, SourceId(uuid_fixture()), import_fixture())
        .expect("feed parses")
        .events
        .pop()
        .expect("series parses");
    assert_eq!(event.overrides.len(), 1);
    assert_eq!(event.overrides[0].change, OverrideChange::Cancelled);
}

#[test]
fn a_detached_instance_moved_across_the_window_boundary_still_overlaps() {
    use chrono::{TimeZone, Utc};
    use clipper_schedule::{
        Expansion, RecurrenceEngine, RruleEngine, ScheduleItem, ScheduleItemId, TimeRange,
    };

    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:overnight-series@example.com\r\nDTSTART:20260901T090000Z\r\n\
DTEND:20260901T093000Z\r\nRRULE:FREQ=DAILY;COUNT=2\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:overnight-series@example.com\r\n\
RECURRENCE-ID:20260902T090000Z\r\nDTSTART:20260903T233000Z\r\n\
DTEND:20260904T013000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let event = parse_ics(feed, SourceId(uuid_fixture()), import_fixture())
        .expect("feed parses")
        .events
        .pop()
        .expect("series parses");
    let series = ScheduleItem {
        id: ScheduleItemId(event.id),
        title: event.title.clone(),
        span: event.span.clone(),
        recurrence: event.recurrence.clone(),
        reference: None,
        alarm: None,
    };
    let from = Utc.with_ymd_and_hms(2026, 9, 4, 0, 0, 0).single().unwrap();
    let overlapping = RruleEngine::new()
        .overlapping_occurrences(
            &series,
            &event.overrides,
            &Expansion {
                window: TimeRange::new(from, from + chrono::TimeDelta::hours(1)).unwrap(),
                observer: Tz::UTC,
            },
        )
        .expect("detached occurrence expands");
    assert_eq!(overlapping.len(), 1);
    assert_eq!(
        overlapping[0].span.end(),
        from + chrono::TimeDelta::minutes(90)
    );
}

#[test]
fn an_unsupported_rule_is_resolved_from_the_referenced_snapshot() {
    use chrono::{TimeZone, Utc};
    use clipper_schedule::{
        Expansion, RecurrenceEngine, RruleEngine, ScheduleItem, ScheduleItemId, TimeRange,
    };

    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:opaque@example.com\r\nDTSTART:20260101T090000Z\r\n\
DTEND:20260101T093000Z\r\nRRULE:FREQ=DAILY;COUNT=2;BYHOUR=9\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";
    let import = import_fixture();
    let event = parse_ics(feed, SourceId(uuid_fixture()), import)
        .expect("snapshot parses")
        .events
        .pop()
        .expect("event parses");
    assert_eq!(event.import, Some(import));
    assert_eq!(
        event.recurrence,
        Recurrence::Imported {
            import,
            uid: "opaque@example.com".into(),
        }
    );

    let rules = parse_imported_recurrence_rules(feed, import).expect("rules resolve");
    assert_eq!(
        rules
            .lookup(import, "opaque@example.com")
            .expect("opaque rule")
            .as_str(),
        "FREQ=DAILY;COUNT=2;BYHOUR=9"
    );
    let item = ScheduleItem {
        id: ScheduleItemId(event.id),
        title: event.title,
        span: event.span,
        recurrence: event.recurrence,
        reference: None,
        alarm: None,
    };
    let occurrences = RruleEngine::with_imported_rules(rules)
        .occurrences(
            &item,
            &[],
            &Expansion {
                window: TimeRange::new(
                    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
                    Utc.with_ymd_and_hms(2026, 1, 4, 0, 0, 0).unwrap(),
                )
                .unwrap(),
                observer: Tz::UTC,
            },
        )
        .expect("resolved rule expands");
    assert_eq!(occurrences.len(), 2);
}

#[test]
fn runtime_rule_extraction_rejects_ambiguous_master_uids_and_rrules() {
    let duplicate_uid = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
BEGIN:VEVENT\r\nUID:same@example.com\r\nDTSTART:20260101T090000Z\r\n\
END:VEVENT\r\nBEGIN:VEVENT\r\nUID:same@example.com\r\n\
DTSTART:20260102T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    assert!(
        parse_imported_recurrence_rules(duplicate_uid, import_fixture()).is_err(),
        "a UID must identify exactly one master inside its snapshot"
    );

    let multiple_rules = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:two-rules@example.com\r\nDTSTART:20260101T090000Z\r\n\
RRULE:FREQ=DAILY;COUNT=2\r\nRRULE:FREQ=WEEKLY;COUNT=2\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";
    assert!(
        parse_imported_recurrence_rules(multiple_rules, import_fixture()).is_err(),
        "multiple RRULE properties cannot be resolved by UID alone"
    );
}

fn feed_with_rrule(rule: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:rule-check@example.com\r\nDTSTART:20260901T090000Z\r\n\
DTEND:20260901T100000Z\r\nRRULE:{rule}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn assert_malformed_rrule(rule: &str, part: &str) {
    let feed = feed_with_rrule(rule);
    match parse_ics(&feed, SourceId(uuid_fixture()), import_fixture()) {
        Err(clipper_schedule::IngestError::Malformed(message)) => {
            assert!(
                message.contains(part),
                "error {message:?} must name {part:?} for rule {rule:?}"
            );
        }
        other => panic!("rule {rule:?} must be Malformed, got {other:?}"),
    }
}

#[test]
fn rewritten_interval_and_count_rules_are_rejected() {
    assert_malformed_rrule("FREQ=DAILY;INTERVAL=0", "INTERVAL");
    assert_malformed_rrule("FREQ=DAILY;COUNT=0", "COUNT");
    assert_malformed_rrule("FREQ=DAILY;INTERVAL=-1", "INTERVAL");
    assert_malformed_rrule("FREQ=DAILY;COUNT=-5", "COUNT");
    assert_malformed_rrule("FREQ=DAILY;INTERVAL=+2", "INTERVAL");
}

#[test]
fn overflowing_interval_and_count_rules_are_rejected() {
    assert_malformed_rrule("FREQ=DAILY;INTERVAL=65536", "INTERVAL");
    assert_malformed_rrule("FREQ=DAILY;COUNT=4294967296", "COUNT");
}

#[test]
fn narrowed_by_clauses_are_rejected() {
    // calcard narrows each list to a fixed width, wrapping or saturating on
    // overflow and stripping the sign, so any value outside that width would
    // import as a different rule.
    assert_malformed_rrule("FREQ=DAILY;BYSECOND=256", "BYSECOND");
    assert_malformed_rrule("FREQ=DAILY;BYMINUTE=256", "BYMINUTE");
    assert_malformed_rrule("FREQ=DAILY;BYHOUR=256", "BYHOUR");
    assert_malformed_rrule("FREQ=DAILY;BYHOUR=-1", "BYHOUR");
    assert_malformed_rrule("FREQ=MONTHLY;BYMONTHDAY=128", "BYMONTHDAY");
    assert_malformed_rrule("FREQ=MONTHLY;BYMONTHDAY=-129", "BYMONTHDAY");
    assert_malformed_rrule("FREQ=YEARLY;BYYEARDAY=32768", "BYYEARDAY");
    assert_malformed_rrule("FREQ=YEARLY;BYWEEKNO=128", "BYWEEKNO");
    assert_malformed_rrule("FREQ=YEARLY;BYMONTH=128", "BYMONTH");
    assert_malformed_rrule(
        "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=2147483648",
        "BYSETPOS",
    );
    assert_malformed_rrule("FREQ=MONTHLY;BYDAY=40000MO", "BYDAY");
}

#[test]
fn a_numeric_wkst_is_rejected() {
    // The parser only reads weekday names for WKST. A numeric one falls
    // through to the expansion library's probe, which refuses it, so the
    // event is skipped instead of importing with a rewritten week start.
    let feed = feed_with_rrule("FREQ=WEEKLY;BYDAY=MO;WKST=1");
    let outcome =
        parse_ics(&feed, SourceId(uuid_fixture()), import_fixture()).expect("the feed parses");
    assert!(outcome.events.is_empty());
    assert_eq!(outcome.skipped.len(), 1);
    assert!(outcome.skipped[0].reason.contains("weekday"), "{:?}", outcome.skipped[0].reason);
}

#[test]
fn boundary_interval_and_count_are_kept() {
    let feed = feed_with_rrule("FREQ=DAILY;INTERVAL=65535");
    let outcome =
        parse_ics(&feed, SourceId(uuid_fixture()), import_fixture()).expect("valid rule parses");
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    let event = outcome.events.into_iter().next().expect("one event");
    let Recurrence::Every(cadence) = event.recurrence else {
        panic!("expected a cadence, got {:?}", event.recurrence);
    };
    assert_eq!(cadence.interval.get(), 65535);

    let feed = feed_with_rrule("FREQ=DAILY;COUNT=4294967295");
    let outcome =
        parse_ics(&feed, SourceId(uuid_fixture()), import_fixture()).expect("valid rule parses");
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    let event = outcome.events.into_iter().next().expect("one event");
    let Recurrence::Every(cadence) = event.recurrence else {
        panic!("expected a cadence, got {:?}", event.recurrence);
    };
    assert_eq!(
        cadence.end,
        RecurrenceEnd::After(std::num::NonZeroU32::new(4294967295).expect("non-zero"))
    );
}

#[test]
fn boundary_interval_expands_with_that_interval() {
    use chrono::{TimeDelta, TimeZone, Utc};
    use clipper_schedule::{
        Expansion, RecurrenceEngine, RruleEngine, ScheduleItem, ScheduleItemId, TimeRange,
    };

    let event = parse_ics(
        &feed_with_rrule("FREQ=DAILY;INTERVAL=65535"),
        SourceId(uuid_fixture()),
        import_fixture(),
    )
    .expect("valid rule parses")
    .events
    .pop()
    .expect("one event");
    let item = ScheduleItem {
        id: ScheduleItemId(event.id),
        title: event.title.clone(),
        span: event.span.clone(),
        recurrence: event.recurrence.clone(),
        reference: None,
        alarm: None,
    };
    let from = Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).single().unwrap();
    let occurrences = RruleEngine::new()
        .occurrences(
            &item,
            &[],
            &Expansion {
                window: TimeRange::new(from, from + TimeDelta::days(2 * 65535)).unwrap(),
                observer: Tz::UTC,
            },
        )
        .expect("expands");
    let starts: Vec<_> = occurrences
        .iter()
        .map(|occurrence| occurrence.span.start())
        .collect();
    assert_eq!(
        starts,
        vec![from, from + TimeDelta::days(65535)],
        "a wrapped interval would show far more than two occurrences"
    );
}

#[test]
fn extreme_but_exact_values_still_import() {
    for rule in [
        "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
        "FREQ=MONTHLY;BYMONTHDAY=-1",
        "FREQ=YEARLY;BYYEARDAY=-1",
        "FREQ=YEARLY;BYWEEKNO=-1",
        "FREQ=DAILY;BYHOUR=0",
    ] {
        parse_ics(
            &feed_with_rrule(rule),
            SourceId(uuid_fixture()),
            import_fixture(),
        )
        .expect("an exactly representable rule imports: {rule}");
    }
}

#[test]
fn plus_signed_ordinals_are_valid_and_import_unchanged() {
    // RFC 5545 allows a leading plus on ordinals. The parser reads it and
    // keeps the value, so the pre-check must let it through.
    for rule in [
        "FREQ=MONTHLY;BYDAY=+1MO",
        "FREQ=MONTHLY;BYMONTHDAY=+15",
        "FREQ=MONTHLY;BYDAY=MO;BYSETPOS=+1",
        "FREQ=YEARLY;BYYEARDAY=+100",
        "FREQ=YEARLY;BYWEEKNO=+1",
    ] {
        let outcome = parse_ics(
            &feed_with_rrule(rule),
            SourceId(uuid_fixture()),
            import_fixture(),
        )
        .expect("a plus-signed ordinal imports: {rule}");
        assert!(
            outcome.skipped.is_empty(),
            "{rule} must not be skipped: {:?}",
            outcome.skipped
        );
    }
}

#[test]
fn a_folded_rrule_with_zero_interval_is_rejected() {
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:folded@example.com\r\nDTSTART:20260901T090000Z\r\n\
DTEND:20260901T100000Z\r\nRRULE:FREQ=DAILY;INTERVAL=\r\n 0\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";
    match parse_ics(feed, SourceId(uuid_fixture()), import_fixture()) {
        Err(clipper_schedule::IngestError::Malformed(message)) => {
            assert!(
                message.contains("INTERVAL"),
                "must name INTERVAL: {message:?}"
            );
        }
        other => panic!("folded INTERVAL=0 must be Malformed, got {other:?}"),
    }
}

#[test]
fn a_valid_interval_and_count_still_becomes_a_cadence() {
    let feed = feed_with_rrule("FREQ=DAILY;INTERVAL=2;COUNT=5");
    let outcome =
        parse_ics(&feed, SourceId(uuid_fixture()), import_fixture()).expect("valid rule parses");
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    let event = outcome.events.into_iter().next().expect("one event");
    let Recurrence::Every(cadence) = event.recurrence else {
        panic!("expected a cadence, got {:?}", event.recurrence);
    };
    assert_eq!(cadence.interval.get(), 2);
    assert_eq!(
        cadence.end,
        RecurrenceEnd::After(std::num::NonZeroU32::new(5).expect("non-zero"))
    );
}

fn feed_with_exdates(count: usize) -> String {
    let values = vec!["20260902T090000Z"; count].join(",");
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:many-overrides@example.com\r\nDTSTART:20260901T090000Z\r\n\
DTEND:20260901T100000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\n\
EXDATE:{values}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

#[test]
fn too_many_exdate_values_are_rejected() {
    let feed = feed_with_exdates(10_001);
    match parse_ics(&feed, SourceId(uuid_fixture()), import_fixture()) {
        Err(clipper_schedule::IngestError::LimitExceeded(message)) => {
            assert!(message.contains("too many recurrence overrides"));
        }
        Ok(outcome) => {
            assert!(outcome.events.is_empty(), "oversized event must not parse");
            assert_eq!(outcome.skipped.len(), 1);
            assert!(
                outcome.skipped[0]
                    .reason
                    .contains("too many recurrence overrides"),
                "must report the limit: {:?}",
                outcome.skipped[0].reason
            );
        }
        Err(other) => panic!("expected the override cap, got {other:?}"),
    }
}

#[test]
fn ten_thousand_exdate_values_parse() {
    let feed = feed_with_exdates(10_000);
    let outcome = parse_ics(&feed, SourceId(uuid_fixture()), import_fixture())
        .expect("10,000 overrides fit the cap");
    assert_eq!(outcome.events.len(), 1);
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
}
