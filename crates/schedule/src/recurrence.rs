//! How a block repeats.
//!
//! Modelled as typed cadences rather than an RFC 5545 `RRULE` string. The
//! string form exists only inside the engine adapter, where it is generated on
//! the way to the expansion library — it is never a field, never parsed from
//! user input, and never round-tripped. Every constructor below validates, so
//! an out-of-range weekday ordinal or an empty weekday set cannot be built.
//!
//! Rules that Google returns and this enum cannot express will need a variant
//! of their own when ingest lands. Adding one is a compiler-guided change:
//! every `match` in the crate is exhaustive and will fail to build until the
//! new case is handled. That is the point of not storing a string.

use std::num::NonZeroU32;

use chrono::{DateTime, Month, Utc, Weekday};
use serde::{Deserialize, Serialize};

/// The complete repeat behaviour of a block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Recurrence {
    /// Happens once. The overwhelming majority of ingested meetings.
    Once,
    /// Repeats on a cadence.
    Every(Cadence),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cadence {
    pub frequency: Frequency,
    /// Every `interval` days/weeks/months/years. One means every one.
    pub interval: NonZeroU32,
    pub end: RecurrenceEnd,
}

impl Cadence {
    /// A cadence repeating every `interval` periods, forever.
    pub fn every(frequency: Frequency, interval: u32) -> Result<Self, RecurrenceError> {
        Ok(Self {
            frequency,
            interval: NonZeroU32::new(interval).ok_or(RecurrenceError::ZeroInterval)?,
            end: RecurrenceEnd::Never,
        })
    }

    /// Every period, forever. The common case.
    pub fn each(frequency: Frequency) -> Self {
        Self {
            frequency,
            interval: NonZeroU32::new(1).expect("1 is non-zero"),
            end: RecurrenceEnd::Never,
        }
    }

    pub fn ending(mut self, end: RecurrenceEnd) -> Self {
        self.end = end;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Frequency {
    /// Every N days.
    Daily,
    /// On the given weekdays, every N weeks.
    Weekly {
        weekdays: WeekdaySet,
        /// Which day the week starts on. Changes which occurrences fall in
        /// which interval when `interval > 1`, so it is not cosmetic.
        week_start: Weekday,
    },
    /// Every N months, on a day picked by ordinal or by weekday.
    Monthly(MonthlyRule),
    /// Every N years, in a fixed month, on a day of that month.
    Yearly { month: Month, day: MonthDay },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonthlyRule {
    /// The 15th; the last day; the second-to-last day.
    OnDay(MonthDay),
    /// The second Tuesday; the last Friday.
    OnWeekday { nth: NthWeekday, weekday: Weekday },
}

/// A day of the month, countable from either end.
///
/// Counting from the end is how "the last day of the month" works without
/// special-casing February.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonthDay {
    /// 1..=31, counting forwards. Months without the day are skipped, per
    /// RFC 5545: `BYMONTHDAY=31` simply does not occur in April.
    FromStart(u8),
    /// 1..=31, counting backwards. One is the last day of the month.
    FromEnd(u8),
}

impl MonthDay {
    pub fn from_start(day: u8) -> Result<Self, RecurrenceError> {
        Self::check(day).map(|()| Self::FromStart(day))
    }

    pub fn from_end(day: u8) -> Result<Self, RecurrenceError> {
        Self::check(day).map(|()| Self::FromEnd(day))
    }

    fn check(day: u8) -> Result<(), RecurrenceError> {
        if (1..=31).contains(&day) {
            Ok(())
        } else {
            Err(RecurrenceError::MonthDayOutOfRange(day))
        }
    }

    /// The signed form RFC 5545 uses in `BYMONTHDAY`.
    pub(crate) fn as_ical(self) -> i8 {
        match self {
            Self::FromStart(d) => d as i8,
            Self::FromEnd(d) => -(d as i8),
        }
    }
}

/// Which occurrence of a weekday within a month, countable from either end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NthWeekday {
    /// 1..=5. One is the first such weekday in the month.
    FromStart(u8),
    /// 1..=5. One is the last such weekday in the month.
    FromEnd(u8),
}

impl NthWeekday {
    pub fn from_start(nth: u8) -> Result<Self, RecurrenceError> {
        Self::check(nth).map(|()| Self::FromStart(nth))
    }

    pub fn from_end(nth: u8) -> Result<Self, RecurrenceError> {
        Self::check(nth).map(|()| Self::FromEnd(nth))
    }

    /// The last occurrence of a weekday in the month.
    pub fn last() -> Self {
        Self::FromEnd(1)
    }

    fn check(nth: u8) -> Result<(), RecurrenceError> {
        if (1..=5).contains(&nth) {
            Ok(())
        } else {
            Err(RecurrenceError::WeekdayOrdinalOutOfRange(nth))
        }
    }

    pub(crate) fn as_ical(self) -> i8 {
        match self {
            Self::FromStart(n) => n as i8,
            Self::FromEnd(n) => -(n as i8),
        }
    }
}

/// A non-empty set of weekdays.
///
/// Non-empty by construction: a weekly rule with no days would expand to
/// nothing, silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeekdaySet(u8);

impl WeekdaySet {
    pub fn new(days: &[Weekday]) -> Result<Self, RecurrenceError> {
        let mask = days
            .iter()
            .fold(0u8, |acc, day| acc | (1 << day.num_days_from_monday()));
        if mask == 0 {
            Err(RecurrenceError::EmptyWeekdaySet)
        } else {
            Ok(Self(mask))
        }
    }

    pub fn just(day: Weekday) -> Self {
        Self(1 << day.num_days_from_monday())
    }

    /// Monday through Friday.
    pub fn weekdays() -> Self {
        Self::new(&[
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ])
        .expect("five days is non-empty")
    }

    pub fn contains(self, day: Weekday) -> bool {
        self.0 & (1 << day.num_days_from_monday()) != 0
    }

    pub fn iter(self) -> impl Iterator<Item = Weekday> {
        const ORDER: [Weekday; 7] = [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ];
        ORDER.into_iter().filter(move |day| self.contains(*day))
    }
}

/// When a repeating block stops repeating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecurrenceEnd {
    Never,
    /// After this many occurrences in total, counting the first.
    After(NonZeroU32),
    /// Up to and including this instant.
    On(DateTime<Utc>),
}

impl RecurrenceEnd {
    pub fn after(count: u32) -> Result<Self, RecurrenceError> {
        NonZeroU32::new(count)
            .map(Self::After)
            .ok_or(RecurrenceError::ZeroCount)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecurrenceError {
    #[error("a repeat interval must be at least 1")]
    ZeroInterval,
    #[error("a repeat count must be at least 1")]
    ZeroCount,
    #[error("day of month {0} is out of range (expected 1..=31)")]
    MonthDayOutOfRange(u8),
    #[error("weekday ordinal {0} is out of range (expected 1..=5)")]
    WeekdayOrdinalOutOfRange(u8),
    #[error("a weekly repeat needs at least one weekday")]
    EmptyWeekdaySet,
}
