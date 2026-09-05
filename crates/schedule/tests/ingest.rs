//! Parsing a real-shaped iCalendar feed.
//!
//! The fixture is deliberately awkward: a zoned meeting, a UTC one, a floating
//! one, a single-day and a multi-day all-day event, a recurring series, a
//! cancellation, and two entries that cannot be read at all. Every one of those
//! shapes appears in an ordinary Google or Zoho export.

use chrono::{NaiveDate, NaiveTime};
use chrono_tz::Tz;
use clipper_schedule::{IngestedStatus, Recurrence, ScheduleSpan, SourceId, TimedStart, parse_ics};

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
    parse_ics(FEED, SourceId(uuid_fixture())).expect("the feed parses")
}

/// A fixed source id, so derived event ids are stable across runs.
fn uuid_fixture() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888)
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

    // Skipping must be explicit. A feed that silently drops entries is worse
    // than one that says which it could not read.
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

/// RFC 5545 floating time — no TZID, no Z — means the same thing Clipper's
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

/// A planner shows every invite. abnormalarm's filter — organized, accepted,
/// or unattended, and no all-day events — belongs to an alarm app, not here.
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
fn an_ingested_rule_is_carried_verbatim() {
    let standup = event("standup@example.com");
    let Recurrence::Raw { rule } = &standup.recurrence else {
        panic!("an ingested rule stays raw rather than being remodelled");
    };
    let text = rule.as_str();
    assert!(text.contains("FREQ=WEEKLY"), "got {text}");
    for day in ["MO", "TU", "WE", "TH", "FR"] {
        assert!(text.contains(day), "{day} missing from {text}");
    }
}

#[test]
fn events_without_a_rule_happen_once() {
    assert_eq!(event("utc-call@example.com").recurrence, Recurrence::Once);
}

/// Re-ingesting the same feed must update events in place rather than pile up
/// duplicates, which is what a derived id buys.
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

    let other_source = parse_ics(FEED, SourceId(uuid::Uuid::from_u128(0xDEAD_BEEF)))
        .expect("parses")
        .events;
    assert!(
        other_source
            .iter()
            .all(|event| !ids(&first).contains(&event.id)),
        "the same feed under a second source must not collide with the first"
    );
}

/// The whole point of `Raw`: a rule Clipper cannot model still expands.
#[test]
fn an_ingested_series_expands() {
    use chrono::{TimeZone, Utc};
    use clipper_schedule::{
        Expansion, RecurrenceEngine, RruleEngine, ScheduleItem, ScheduleItemId, Window,
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
                window: Window::new(from, from + chrono::TimeDelta::days(7)).expect("window"),
                observer: Tz::Europe__Berlin,
            },
        )
        .expect("an ingested rule expands");

    assert_eq!(occurrences.len(), 5, "Mon to Fri");
    assert_eq!(
        occurrences[0]
            .span
            .start
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
            parse_ics(invalid, source).is_err(),
            "must reject {invalid:?} instead of treating it as an empty snapshot"
        );
    }

    let empty = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n";
    let outcome = parse_ics(empty, source).expect("an explicit empty snapshot is valid");
    assert!(outcome.events.is_empty());
    assert!(outcome.skipped.is_empty());
}

#[test]
fn parser_has_an_input_size_ceiling() {
    let mut oversized = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n".to_string();
    oversized.push_str(&" ".repeat(8 * 1024 * 1024));
    assert!(parse_ics(&oversized, SourceId(uuid_fixture())).is_err());
}

#[test]
fn an_unknown_tzid_skips_the_event_instead_of_becoming_floating() {
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:unknown-zone@example.com\r\nSUMMARY:Unknown zone\r\n\
DTSTART;TZID=Mars/Olympus_Mons:20260908T090000\r\n\
DTEND;TZID=Mars/Olympus_Mons:20260908T100000\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";
    let outcome = parse_ics(feed, SourceId(uuid_fixture())).expect("the feed itself parses");
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
    let outcome = parse_ics(feed, SourceId(uuid_fixture())).expect("feed parses");
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
    let outcome = parse_ics(feed, SourceId(uuid_fixture())).expect("feed parses");
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
        Expansion, RecurrenceEngine, RruleEngine, ScheduleItem, ScheduleItemId, Window,
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
    let event = parse_ics(feed, source)
        .expect("feed parses")
        .events
        .pop()
        .expect("master event");
    assert_eq!(event.overrides.len(), 4);
    assert_eq!(
        event,
        parse_ics(feed, source)
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
                window: Window::new(from, from + chrono::TimeDelta::days(7)).unwrap(),
                observer: Tz::UTC,
            },
        )
        .expect("exceptions expand");
    let starts: Vec<_> = occurrences
        .iter()
        .map(|occurrence| occurrence.span.start.format("%Y-%m-%d %H:%M").to_string())
        .collect();
    assert_eq!(
        starts,
        ["2026-09-01 09:00", "2026-09-03 14:00", "2026-09-05 09:00"]
    );
}

#[test]
fn an_unsupported_range_exception_skips_its_whole_series() {
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:range@example.com\r\nDTSTART:20260901T090000Z\r\nRRULE:FREQ=DAILY\r\n\
END:VEVENT\r\nBEGIN:VEVENT\r\nUID:range@example.com\r\n\
RECURRENCE-ID;RANGE=THISANDFUTURE:20260903T090000Z\r\n\
DTSTART:20260903T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let outcome = parse_ics(feed, SourceId(uuid_fixture())).expect("feed parses");
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
    let event = parse_ics(feed, SourceId(uuid_fixture()))
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
        Expansion, RecurrenceEngine, RruleEngine, ScheduleItem, ScheduleItemId, Window,
    };

    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
UID:overnight-series@example.com\r\nDTSTART:20260901T090000Z\r\n\
DTEND:20260901T093000Z\r\nRRULE:FREQ=DAILY;COUNT=2\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:overnight-series@example.com\r\n\
RECURRENCE-ID:20260902T090000Z\r\nDTSTART:20260903T233000Z\r\n\
DTEND:20260904T013000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let event = parse_ics(feed, SourceId(uuid_fixture()))
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
                window: Window::new(from, from + chrono::TimeDelta::hours(1)).unwrap(),
                observer: Tz::UTC,
            },
        )
        .expect("detached occurrence expands");
    assert_eq!(overlapping.len(), 1);
    assert_eq!(
        overlapping[0].span.end,
        from + chrono::TimeDelta::minutes(90)
    );
}
