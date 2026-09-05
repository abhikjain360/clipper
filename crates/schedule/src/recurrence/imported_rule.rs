use std::{collections::BTreeMap, num::NonZeroU32};

use chrono::{Datelike, NaiveDateTime, TimeZone, Utc, Weekday};

use super::{
    Cadence, Frequency, MonthDay, MonthlyRule, NthWeekday, RawRule, Recurrence, RecurrenceEnd,
    RecurrenceError, WeekdaySet,
};

pub(super) fn convert(
    rule: String,
    local_start: NaiveDateTime,
) -> Result<Recurrence, RecurrenceError> {
    let raw = RawRule::new(rule)?;
    let Some(cadence) = cadence(raw.as_str(), local_start) else {
        return Ok(Recurrence::Raw { rule: raw });
    };
    Ok(Recurrence::Every(cadence))
}

fn cadence(rule: &str, local_start: NaiveDateTime) -> Option<Cadence> {
    let mut fields = BTreeMap::new();
    for part in rule.split(';') {
        let (key, value) = part.split_once('=')?;
        let key = key.to_ascii_uppercase();
        if fields.insert(key, value.to_ascii_uppercase()).is_some() {
            return None;
        }
    }

    let allowed = [
        "FREQ",
        "INTERVAL",
        "COUNT",
        "UNTIL",
        "BYDAY",
        "BYMONTHDAY",
        "BYMONTH",
        "WKST",
    ];
    if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
        return None;
    }
    if fields.contains_key("COUNT") && fields.contains_key("UNTIL") {
        return None;
    }

    let interval = parse_positive(fields.get("INTERVAL").map(String::as_str).unwrap_or("1"))?;
    let end = if let Some(count) = fields.get("COUNT") {
        RecurrenceEnd::After(parse_positive(count)?)
    } else if let Some(until) = fields.get("UNTIL") {
        // A local or DATE UNTIL depends on the DTSTART zone/type and cannot be
        // represented by RecurrenceEnd::On, which is always an absolute UTC instant.
        if !until.ends_with('Z') {
            return None;
        }
        let naive = NaiveDateTime::parse_from_str(until, "%Y%m%dT%H%M%SZ").ok()?;
        RecurrenceEnd::On(Utc.from_utc_datetime(&naive))
    } else {
        RecurrenceEnd::Never
    };

    let frequency = match fields.get("FREQ")?.as_str() {
        "DAILY" => {
            if has_any(&fields, &["BYDAY", "BYMONTHDAY", "BYMONTH", "WKST"]) {
                return None;
            }
            Frequency::Daily
        }
        "WEEKLY" => {
            if has_any(&fields, &["BYMONTHDAY", "BYMONTH"]) {
                return None;
            }
            if fields.get("WKST").is_some_and(|value| value != "MO") {
                return None;
            }
            let weekdays = match fields.get("BYDAY") {
                Some(days) => WeekdaySet::new(
                    &days
                        .split(',')
                        .map(parse_plain_weekday)
                        .collect::<Option<Vec<_>>>()?,
                )
                .ok()?,
                None => WeekdaySet::just(local_start.weekday()),
            };
            Frequency::Weekly { weekdays }
        }
        "MONTHLY" => {
            if has_any(&fields, &["BYMONTH", "WKST"]) {
                return None;
            }
            let rule = match (fields.get("BYMONTHDAY"), fields.get("BYDAY")) {
                (None, None) => {
                    MonthlyRule::OnDay(MonthDay::from_start(local_start.day() as u8).ok()?)
                }
                (Some(day), None) if !day.contains(',') => {
                    MonthlyRule::OnDay(parse_month_day(day)?)
                }
                (None, Some(day)) if !day.contains(',') => parse_monthly_weekday(day)?,
                _ => return None,
            };
            Frequency::Monthly(rule)
        }
        "YEARLY" => {
            if has_any(&fields, &["BYDAY", "WKST"]) {
                return None;
            }
            let month_number = fields
                .get("BYMONTH")
                .and_then(|value| value.parse().ok())
                .unwrap_or(local_start.month() as u8);
            if fields
                .get("BYMONTH")
                .is_some_and(|value| value.contains(','))
            {
                return None;
            }
            let month = chrono::Month::try_from(month_number).ok()?;
            let day = match fields.get("BYMONTHDAY") {
                Some(value) if !value.contains(',') => parse_month_day(value)?,
                None => MonthDay::from_start(local_start.day() as u8).ok()?,
                _ => return None,
            };
            Frequency::Yearly { month, day }
        }
        _ => return None,
    };
    Some(Cadence {
        frequency,
        interval,
        end,
    })
}

fn has_any(fields: &BTreeMap<String, String>, keys: &[&str]) -> bool {
    keys.iter().any(|key| fields.contains_key(*key))
}

fn parse_positive(value: &str) -> Option<NonZeroU32> {
    NonZeroU32::new(value.parse().ok()?)
}

fn parse_plain_weekday(value: &str) -> Option<Weekday> {
    match value {
        "MO" => Some(Weekday::Mon),
        "TU" => Some(Weekday::Tue),
        "WE" => Some(Weekday::Wed),
        "TH" => Some(Weekday::Thu),
        "FR" => Some(Weekday::Fri),
        "SA" => Some(Weekday::Sat),
        "SU" => Some(Weekday::Sun),
        _ => None,
    }
}

fn parse_month_day(value: &str) -> Option<MonthDay> {
    let day: i8 = value.parse().ok()?;
    if day > 0 {
        MonthDay::from_start(day as u8).ok()
    } else {
        MonthDay::from_end(day.unsigned_abs()).ok()
    }
}

fn parse_monthly_weekday(value: &str) -> Option<MonthlyRule> {
    if value.len() < 3 {
        return None;
    }
    let (ordinal, weekday) = value.split_at(value.len() - 2);
    let weekday = parse_plain_weekday(weekday)?;
    let nth: i8 = ordinal.parse().ok()?;
    let nth = if nth > 0 {
        NthWeekday::from_start(nth as u8).ok()?
    } else {
        NthWeekday::from_end(nth.unsigned_abs()).ok()?
    };
    Some(MonthlyRule::OnWeekday { nth, weekday })
}

#[cfg(test)]
mod tests {
    use chrono::{Month, NaiveDate, Weekday};

    use super::*;

    fn start() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2024, 1, 10)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap()
    }
    fn converted(rule: &str) -> Cadence {
        match convert(rule.into(), start()).unwrap() {
            Recurrence::Every(value) => value,
            other => panic!("expected cadence, got {other:?}"),
        }
    }
    fn raw(rule: &str) {
        assert!(matches!(
            convert(rule.into(), start()).unwrap(),
            Recurrence::Raw { .. }
        ));
    }

    #[test]
    fn converts_supported_shapes() {
        assert_eq!(
            converted("FREQ=DAILY;INTERVAL=2;COUNT=4"),
            Cadence::every(Frequency::Daily, 2)
                .unwrap()
                .ending(RecurrenceEnd::after(4).unwrap())
        );
        assert_eq!(
            converted("FREQ=WEEKLY;BYDAY=MO,WE;WKST=MO").frequency,
            Frequency::Weekly {
                weekdays: WeekdaySet::new(&[Weekday::Mon, Weekday::Wed]).unwrap()
            }
        );
        assert_eq!(
            converted("FREQ=MONTHLY;BYDAY=-1FR").frequency,
            Frequency::Monthly(MonthlyRule::OnWeekday {
                nth: NthWeekday::last(),
                weekday: Weekday::Fri
            })
        );
        assert_eq!(
            converted("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29").frequency,
            Frequency::Yearly {
                month: Month::February,
                day: MonthDay::FromStart(29)
            }
        );
        assert!(matches!(
            converted("FREQ=DAILY;UNTIL=20250102T030405Z").end,
            RecurrenceEnd::On(_)
        ));
    }

    #[test]
    fn uses_dtstart_defaults_losslessly() {
        assert_eq!(
            converted("FREQ=WEEKLY").frequency,
            Frequency::Weekly {
                weekdays: WeekdaySet::just(Weekday::Wed)
            }
        );
        assert_eq!(
            converted("FREQ=MONTHLY").frequency,
            Frequency::Monthly(MonthlyRule::OnDay(MonthDay::FromStart(10)))
        );
        assert_eq!(
            converted("FREQ=YEARLY").frequency,
            Frequency::Yearly {
                month: Month::January,
                day: MonthDay::FromStart(10)
            }
        );
    }

    #[test]
    fn retains_every_unsupported_or_context_sensitive_rule() {
        for rule in [
            "FREQ=HOURLY",
            "FREQ=DAILY;BYDAY=MO",
            "FREQ=WEEKLY;WKST=SU",
            "FREQ=MONTHLY;BYDAY=MO",
            "FREQ=MONTHLY;BYMONTHDAY=1,15",
            "FREQ=YEARLY;BYMONTH=1,2",
            "FREQ=DAILY;BYSETPOS=1",
            "FREQ=DAILY;UNTIL=20250102T030405",
            "FREQ=DAILY;UNTIL=20250102",
        ] {
            raw(rule);
        }
    }
}
