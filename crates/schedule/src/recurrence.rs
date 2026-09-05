//! How a block repeats.
//!
//! Rules Clipper understands are modelled as typed cadences. Imported RFC 5545
//! `RRULE` values are converted when their complete meaning fits those types;
//! otherwise their validated string form is retained verbatim. Every typed
//! constructor below validates, so an out-of-range weekday ordinal or an empty
//! weekday set cannot be built.
//!
//! Ingest is the exception, and it has its own variant rather than a loophole.
//! A provider can send a rule this enum cannot express, and [`Recurrence::Raw`]
//! carries it verbatim — validated at construction, expanded as-is, and never
//! editable in Clipper, because Clipper does not understand its structure. The
//! type says which rules are understood and which are passed through, which is
//! the property that matters; a bare string field everywhere would say nothing.

use std::{num::NonZeroU32, str::FromStr};

use chrono::{DateTime, Month, Utc, Weekday};
use serde::{Deserialize, Serialize};

mod imported_rule;

/// The complete repeat behaviour of a block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recurrence {
    /// Happens once. The overwhelming majority of ingested meetings.
    Once,
    /// Repeats on a cadence Clipper understands and can edit.
    Every(Cadence),
    /// An RFC 5545 rule ingested from a provider that [`Cadence`] cannot
    /// express. Expanded verbatim and read-only: Clipper can show when it
    /// happens without claiming to understand why.
    ///
    /// A struct variant rather than a newtype because serde cannot internally
    /// tag a newtype wrapping a string — and `{"kind":"raw","rule":"…"}` reads
    /// better than a bare string would anyway.
    Raw { rule: RawRule },
}

impl Recurrence {
    /// Converts a validated imported `RRULE` to an editable cadence when doing
    /// so is lossless, retaining the original rule otherwise.
    pub fn from_imported_rule(
        rule: impl Into<String>,
        local_start: chrono::NaiveDateTime,
    ) -> Result<Self, RecurrenceError> {
        imported_rule::convert(rule.into(), local_start)
    }
}

/// An RFC 5545 `RRULE` value, validated on the way in.
///
/// Parsing at construction rather than at expansion means an unparseable feed
/// is rejected where it arrives, not hours later when an alarm fails to fire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RawRule(String);

impl RawRule {
    pub fn new(rule: impl Into<String>) -> Result<Self, RecurrenceError> {
        let rule = rule.into();
        let trimmed = rule.trim().trim_start_matches("RRULE:").trim().to_string();
        if trimmed.is_empty() {
            return Err(RecurrenceError::UnparseableRule("empty rule".into()));
        }
        // One property, not a document. The probe below parses a whole
        // `RRuleSet`, so a value carrying its own line break would validate as
        // the probe's DTSTART plus an extra property — and expansion splices
        // the stored value verbatim after `RRULE:`, against the *real* DTSTART
        // and zone. That turns a smuggled `\nEXDATE:` or second `\nRRULE:` into
        // live content this check never saw. Refuse the whole control range so
        // a bare CR or a NUL cannot fold lines either.
        if trimmed.contains(|character: char| character.is_control()) {
            return Err(RecurrenceError::UnparseableRule(
                "rule contains a control character".into(),
            ));
        }
        // UNTIL must have the same value kind as DTSTART (or be UTC for a
        // timed DTSTART). Try each legal imported DTSTART kind: the real start
        // is supplied by the item during expansion.
        let starts = [
            "DTSTART:20200101T000000Z",
            "DTSTART:20200101T000000",
            "DTSTART:20200101",
        ];
        let mut last_error = None;
        if !starts.iter().any(|start| {
            let probe = format!("{start}\nRRULE:{trimmed}");
            match rrule::RRuleSet::from_str(&probe) {
                Ok(_) => true,
                Err(error) => {
                    last_error = Some(error.to_string());
                    false
                }
            }
        }) {
            return Err(RecurrenceError::UnparseableRule(
                last_error.unwrap_or_else(|| "invalid rule".to_string()),
            ));
        }
        Ok(Self(trimmed))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RawRule {
    type Error = RecurrenceError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RawRule> for String {
    fn from(value: RawRule) -> Self {
        value.0
    }
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
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum Frequency {
    /// Every N days.
    Daily,
    /// On the given weekdays, every N weeks, with weeks starting on Monday.
    Weekly { weekdays: WeekdaySet },
    /// Every N months, on a day picked by ordinal or by weekday.
    Monthly(MonthlyRule),
    /// Every N years, in a fixed month, on a day of that month.
    Yearly {
        #[serde(with = "month_name")]
        month: Month,
        day: MonthDay,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum MonthlyRule {
    /// The 15th; the last day; the second-to-last day.
    OnDay(MonthDay),
    /// The second Tuesday; the last Friday.
    OnWeekday {
        nth: NthWeekday,
        #[serde(with = "weekday_name")]
        weekday: Weekday,
    },
}

/// A day of the month, countable from either end.
///
/// Counting from the end is how "the last day of the month" works without
/// special-casing February.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", content = "day", rename_all = "snake_case")]
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
#[serde(tag = "from", content = "nth", rename_all = "snake_case")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeekdaySet(u8);

/// Serialized as a list of day names, never as the internal bitmask — the bit
/// layout is an implementation detail and no consumer should have to know it.
impl Serialize for WeekdaySet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.iter().count()))?;
        for day in self.iter() {
            seq.serialize_element(serde_weekday_name(day))?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for WeekdaySet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let names = Vec::<String>::deserialize(deserializer)?;
        let days = names
            .iter()
            .map(|name| serde_weekday_from_name(name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(serde::de::Error::custom)?;
        Self::new(&days).map_err(serde::de::Error::custom)
    }
}

/// Spell a lone weekday the same way [`WeekdaySet`] spells its members.
///
/// chrono's own `Weekday` serialization is `"Mon"`; mixing that with the
/// lowercase names in a weekday set would put two spellings in one JSON object
/// and make every consumer handle both.
mod weekday_name {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::{Weekday, serde_weekday_from_name, serde_weekday_name};

    pub fn serialize<S: Serializer>(day: &Weekday, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(serde_weekday_name(*day))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Weekday, D::Error> {
        let name = String::deserialize(deserializer)?;
        serde_weekday_from_name(&name).map_err(serde::de::Error::custom)
    }
}

/// Spell a month in lower case, matching the weekday convention above rather
/// than chrono's own `"February"`.
mod month_name {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::Month;

    const NAMES: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];

    pub fn serialize<S: Serializer>(month: &Month, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(NAMES[(month.number_from_month() - 1) as usize])
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Month, D::Error> {
        let name = String::deserialize(deserializer)?;
        NAMES
            .iter()
            .position(|candidate| *candidate == name)
            .and_then(|index| Month::try_from(index as u8 + 1).ok())
            .ok_or_else(|| serde::de::Error::custom(format!("unknown month {name:?}")))
    }
}

fn serde_weekday_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "mon",
        Weekday::Tue => "tue",
        Weekday::Wed => "wed",
        Weekday::Thu => "thu",
        Weekday::Fri => "fri",
        Weekday::Sat => "sat",
        Weekday::Sun => "sun",
    }
}

fn serde_weekday_from_name(name: &str) -> Result<Weekday, String> {
    match name {
        "mon" => Ok(Weekday::Mon),
        "tue" => Ok(Weekday::Tue),
        "wed" => Ok(Weekday::Wed),
        "thu" => Ok(Weekday::Thu),
        "fri" => Ok(Weekday::Fri),
        "sat" => Ok(Weekday::Sat),
        "sun" => Ok(Weekday::Sun),
        other => Err(format!("unknown weekday {other:?}")),
    }
}

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
#[serde(tag = "when", content = "value", rename_all = "snake_case")]
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
    #[error("recurrence rule could not be parsed: {0}")]
    UnparseableRule(String),
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
