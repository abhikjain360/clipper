//! Human-readable descriptions of a schedule item.
//!
//! These live in the domain crate rather than in each shell so that web,
//! desktop and mobile render a cadence identically instead of growing three
//! slightly different phrasings of "every second Tuesday".

use chrono::{Month, Weekday};

use crate::{
    item::ScheduleItem,
    recurrence::{
        Cadence, Frequency, MonthDay, MonthlyRule, NthWeekday, Recurrence, RecurrenceEnd,
        WeekdaySet,
    },
    time::{ScheduleSpan, TimedStart},
};

impl Recurrence {
    /// A short description of the cadence, e.g. `"Every weekday"`.
    pub fn summary(&self) -> String {
        match self {
            Self::Once => "Once".to_string(),
            Self::Every(cadence) => cadence.summary(),
        }
    }
}

impl Cadence {
    pub fn summary(&self) -> String {
        let every = match self.interval.get() {
            1 => match &self.frequency {
                Frequency::Daily => "Every day".to_string(),
                Frequency::Weekly { weekdays, .. } => weekly_phrase(*weekdays),
                Frequency::Monthly(rule) => format!("Every month on {}", rule.phrase()),
                Frequency::Yearly { month, day } => {
                    format!("Every year on {} {}", month_name(*month), day.phrase())
                }
            },
            n => match &self.frequency {
                Frequency::Daily => format!("Every {n} days"),
                Frequency::Weekly { weekdays, .. } => {
                    format!("Every {n} weeks on {}", weekday_list(*weekdays))
                }
                Frequency::Monthly(rule) => format!("Every {n} months on {}", rule.phrase()),
                Frequency::Yearly { month, day } => {
                    format!("Every {n} years on {} {}", month_name(*month), day.phrase())
                }
            },
        };
        match self.end {
            RecurrenceEnd::Never => every,
            RecurrenceEnd::After(count) => format!("{every}, {count} times"),
            RecurrenceEnd::On(until) => {
                format!("{every}, until {}", until.format("%-d %b %Y"))
            }
        }
    }
}

impl MonthlyRule {
    fn phrase(&self) -> String {
        match self {
            Self::OnDay(day) => format!("the {}", day.phrase()),
            Self::OnWeekday { nth, weekday } => {
                format!("the {} {}", nth.phrase(), weekday_name(*weekday))
            }
        }
    }
}

impl MonthDay {
    fn phrase(self) -> String {
        match self {
            Self::FromStart(day) => ordinal(u32::from(day)),
            Self::FromEnd(1) => "last day".to_string(),
            Self::FromEnd(day) => format!("{}-to-last day", ordinal(u32::from(day))),
        }
    }
}

impl NthWeekday {
    fn phrase(self) -> String {
        match self {
            Self::FromStart(nth) => ordinal(u32::from(nth)),
            Self::FromEnd(1) => "last".to_string(),
            Self::FromEnd(nth) => format!("{}-to-last", ordinal(u32::from(nth))),
        }
    }
}

impl ScheduleItem {
    /// A short description of when this happens, e.g. `"07:00 (floating)"`.
    pub fn time_summary(&self) -> String {
        match &self.span {
            ScheduleSpan::Timed { start, duration } => {
                let clock = start.local().format("%H:%M");
                let minutes = duration.minutes();
                match start {
                    // Naming the zone is the point: a floating alarm and a
                    // zoned one look identical otherwise, and they behave
                    // differently the moment the owner travels.
                    TimedStart::Floating(_) => format!("{clock} for {minutes} min (floating)"),
                    TimedStart::Zoned { zone, .. } => {
                        format!("{clock} for {minutes} min ({zone})")
                    }
                }
            }
            ScheduleSpan::AllDay { days, .. } => match days.get() {
                1 => "All day".to_string(),
                n => format!("All day, {n} days"),
            },
        }
    }
}

fn weekly_phrase(weekdays: WeekdaySet) -> String {
    if weekdays == WeekdaySet::weekdays() {
        return "Every weekday".to_string();
    }
    format!("Every week on {}", weekday_list(weekdays))
}

fn weekday_list(weekdays: WeekdaySet) -> String {
    let names: Vec<&str> = weekdays.iter().map(weekday_name).collect();
    match names.as_slice() {
        [] => String::new(),
        [only] => (*only).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn weekday_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

fn month_name(month: Month) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES[(month.number_from_month() - 1) as usize]
}

fn ordinal(n: u32) -> String {
    // 11th, 12th and 13th break the last-digit rule.
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::recurrence::RecurrenceEnd;

    fn weekly(days: &[Weekday]) -> Recurrence {
        Recurrence::Every(Cadence::each(Frequency::Weekly {
            weekdays: WeekdaySet::new(days).expect("non-empty"),
            week_start: Weekday::Mon,
        }))
    }

    #[test]
    fn weekday_sets_read_naturally() {
        assert_eq!(
            weekly(&[
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri
            ])
            .summary(),
            "Every weekday"
        );
        assert_eq!(weekly(&[Weekday::Tue]).summary(), "Every week on Tue");
        assert_eq!(
            weekly(&[Weekday::Mon, Weekday::Wed, Weekday::Fri]).summary(),
            "Every week on Mon, Wed and Fri"
        );
    }

    #[test]
    fn intervals_and_ends_are_described() {
        let fortnightly = Recurrence::Every(
            Cadence::every(
                Frequency::Weekly {
                    weekdays: WeekdaySet::just(Weekday::Tue),
                    week_start: Weekday::Mon,
                },
                2,
            )
            .expect("non-zero"),
        );
        assert_eq!(fortnightly.summary(), "Every 2 weeks on Tue");

        let three_times = Recurrence::Every(
            Cadence::each(Frequency::Daily).ending(RecurrenceEnd::after(3).expect("non-zero")),
        );
        assert_eq!(three_times.summary(), "Every day, 3 times");

        let until = Recurrence::Every(
            Cadence::each(Frequency::Daily).ending(RecurrenceEnd::On(
                Utc.with_ymd_and_hms(2026, 6, 12, 0, 0, 0)
                    .single()
                    .expect("unambiguous"),
            )),
        );
        assert_eq!(until.summary(), "Every day, until 12 Jun 2026");
    }

    #[test]
    fn month_positions_read_naturally() {
        let second_tuesday =
            Recurrence::Every(Cadence::each(Frequency::Monthly(MonthlyRule::OnWeekday {
                nth: NthWeekday::from_start(2).expect("in range"),
                weekday: Weekday::Tue,
            })));
        assert_eq!(second_tuesday.summary(), "Every month on the 2nd Tue");

        let last_friday =
            Recurrence::Every(Cadence::each(Frequency::Monthly(MonthlyRule::OnWeekday {
                nth: NthWeekday::last(),
                weekday: Weekday::Fri,
            })));
        assert_eq!(last_friday.summary(), "Every month on the last Fri");

        let last_day = Recurrence::Every(Cadence::each(Frequency::Monthly(MonthlyRule::OnDay(
            MonthDay::from_end(1).expect("in range"),
        ))));
        assert_eq!(last_day.summary(), "Every month on the last day");

        let twenty_first = Recurrence::Every(Cadence::each(Frequency::Monthly(
            MonthlyRule::OnDay(MonthDay::from_start(21).expect("in range")),
        )));
        assert_eq!(twenty_first.summary(), "Every month on the 21st");
    }

    #[test]
    fn teens_take_th_not_st() {
        for (n, expected) in [
            (1, "1st"),
            (2, "2nd"),
            (3, "3rd"),
            (4, "4th"),
            (11, "11th"),
            (12, "12th"),
            (13, "13th"),
            (21, "21st"),
            (22, "22nd"),
            (23, "23rd"),
        ] {
            assert_eq!(ordinal(n), expected);
        }
    }
}
