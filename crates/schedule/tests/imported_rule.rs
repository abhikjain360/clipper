use chrono::{NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use clipper_schedule::{
    BlockDuration, Expansion, ImportedRuleResolver, Recurrence, RecurrenceEngine, RruleEngine,
    ScheduleItem, ScheduleItemId, ScheduleSpan, TimedStart, Window,
};

fn import_id() -> clipper_api_types::ObjectId {
    uuid::Uuid::from_u128(0x1111).into()
}

fn starts(
    rule: Recurrence,
    local: NaiveDateTime,
    imported_rules: ImportedRuleResolver,
) -> Vec<chrono::DateTime<Utc>> {
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
    RruleEngine::with_imported_rules(imported_rules)
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
        let converted =
            Recurrence::from_imported_rule(rule, local, import_id(), "event@example.com").unwrap();
        assert!(matches!(converted, Recurrence::Every(_)), "{rule}");
        let imported = Recurrence::Imported {
            import: import_id(),
            uid: "event@example.com".into(),
        };
        let mut resolver = ImportedRuleResolver::new();
        resolver
            .insert(import_id(), "event@example.com", rule)
            .unwrap();
        assert_eq!(
            starts(imported, local, resolver),
            starts(converted, local, ImportedRuleResolver::new()),
            "{rule}"
        );
    }
}

#[test]
fn unsupported_rules_retain_only_the_import_reference() {
    let local = NaiveDateTime::parse_from_str("20240110T090000", "%Y%m%dT%H%M%S").unwrap();
    let rule = "FREQ=WEEKLY;BYDAY=MO;WKST=SU";
    let Recurrence::Imported { import, uid } =
        Recurrence::from_imported_rule(rule, local, import_id(), "event@example.com").unwrap()
    else {
        panic!("unsupported rule was converted")
    };
    assert_eq!(import, import_id());
    assert_eq!(uid, "event@example.com");
}
