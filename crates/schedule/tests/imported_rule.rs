use chrono::{NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use clipper_schedule::{
    BlockDuration, Expansion, RawRule, Recurrence, RecurrenceEngine, RruleEngine, ScheduleItem,
    ScheduleItemId, ScheduleSpan, TimedStart, Window,
};

fn starts(rule: Recurrence, local: NaiveDateTime) -> Vec<chrono::DateTime<Utc>> {
    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "import comparison".into(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Zoned {
                local,
                zone: Tz::UTC,
            },
            duration: BlockDuration::from_minutes(30).unwrap(),
        },
        recurrence: rule,
        reference: None,
        alarm: None,
    };
    RruleEngine::new()
        .occurrences(
            &item,
            &[],
            &Expansion {
                window: Window::new(
                    Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
                    Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap(),
                )
                .unwrap(),
                observer: Tz::UTC,
            },
        )
        .unwrap()
        .into_iter()
        .map(|occurrence| occurrence.span.start)
        .collect()
}

#[test]
fn converted_cadences_have_the_same_occurrences_as_the_imported_rule() {
    let local = NaiveDateTime::parse_from_str("20240110T090000", "%Y%m%dT%H%M%S").unwrap();
    for rule in [
        "FREQ=DAILY;INTERVAL=3;COUNT=8",
        "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;WKST=MO;COUNT=8",
        "FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=8",
        "FREQ=MONTHLY;BYDAY=2WE;COUNT=8",
        "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29;COUNT=3",
        "FREQ=DAILY;UNTIL=20240201T090000Z",
    ] {
        let converted = Recurrence::from_imported_rule(rule, local).unwrap();
        assert!(matches!(converted, Recurrence::Every(_)), "{rule}");
        let raw = Recurrence::Raw {
            rule: RawRule::new(rule).unwrap(),
        };
        assert_eq!(starts(raw, local), starts(converted, local), "{rule}");
    }
}

#[test]
fn unsupported_rules_retain_the_exact_validated_value() {
    let local = NaiveDateTime::parse_from_str("20240110T090000", "%Y%m%dT%H%M%S").unwrap();
    let rule = "FREQ=WEEKLY;BYDAY=MO;WKST=SU";
    let Recurrence::Raw { rule: retained } = Recurrence::from_imported_rule(rule, local).unwrap()
    else {
        panic!("unsupported rule was converted")
    };
    assert_eq!(retained.as_str(), rule);
}
