//! When a block starts and how long it lasts.
//!
//! There are three kinds of start and they behave differently. A floating
//! 07:00 alarm follows the device across timezones. A zoned meeting stays
//! pinned to the zone it was scheduled in. An all-day event has no instant at
//! all.
//!
//! [`ScheduleSpan`] leaves the invalid combinations unrepresentable: an all-day
//! event cannot carry a minute duration, and a timed event cannot lack a time.

use std::num::NonZeroU32;

use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// A start that has a time of day.
///
/// Serialized with a `kind` tag. The same shape crosses the IPC boundary,
/// reaches the UI, and lands in the encrypted payload on disk, and all three
/// get read by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "at", rename_all = "snake_case")]
pub enum TimedStart {
    /// Wall-clock time with no zone. Follows the observer, so 07:00 stays
    /// 07:00 in every zone. Alarms use this.
    Floating(NaiveDateTime),
    /// Wall-clock time pinned to an IANA zone. Does not follow the observer.
    /// Stores the local time rather than an instant, so a 09:00 Berlin meeting
    /// is still 09:00 after a DST transition.
    Zoned { local: NaiveDateTime, zone: Tz },
}

impl TimedStart {
    /// The wall-clock time, whichever kind this is.
    pub fn local(&self) -> NaiveDateTime {
        match self {
            Self::Floating(local) => *local,
            Self::Zoned { local, .. } => *local,
        }
    }

    /// Resolve to an absolute instant. `observer` supplies the zone for a
    /// floating start and is ignored for a zoned one.
    pub fn resolve(&self, observer: Tz) -> Result<DateTime<Utc>, TimeError> {
        let (local, zone) = match self {
            Self::Floating(local) => (*local, observer),
            Self::Zoned { local, zone } => (*local, *zone),
        };
        resolve_local(zone, local).ok_or(TimeError::UnresolvableLocalTime { local, zone })
    }
}

/// A block's extent in time.
///
/// All-day is its own variant rather than a timed span of 24 hours. Turning an
/// all-day date into an instant lands it on a different day for observers in
/// other zones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleSpan {
    Timed {
        start: TimedStart,
        duration: BlockDuration,
    },
    AllDay {
        start: NaiveDate,
        days: NonZeroU32,
    },
}

impl ScheduleSpan {
    /// Resolve to an absolute half-open interval `[start, end)`.
    ///
    /// `observer` supplies the zone for floating and all-day spans, which have
    /// no zone of their own.
    pub fn resolve(&self, observer: Tz) -> Result<TimeRange, TimeError> {
        match self {
            Self::Timed { start, duration } => {
                let begin = start.resolve(observer)?;
                TimeRange::new(
                    begin,
                    begin
                        .checked_add_signed(duration.as_time_delta())
                        .ok_or(TimeError::DateOverflow)?,
                )
            }
            Self::AllDay { start, days } => {
                let begin_local = start
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is always valid");
                let end_local = start
                    .checked_add_days(chrono::Days::new(u64::from(days.get())))
                    .ok_or(TimeError::DateOverflow)?
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is always valid");
                TimeRange::new(
                    resolve_local(observer, begin_local).ok_or(
                        TimeError::UnresolvableLocalTime {
                            local: begin_local,
                            zone: observer,
                        },
                    )?,
                    resolve_local(observer, end_local).ok_or(TimeError::UnresolvableLocalTime {
                        local: end_local,
                        zone: observer,
                    })?,
                )
            }
        }
    }

    /// The wall-clock start, for building an iCalendar `DTSTART`.
    pub fn local_start(&self) -> NaiveDateTime {
        match self {
            Self::Timed { start, .. } => start.local(),
            Self::AllDay { start, .. } => start
                .and_hms_opt(0, 0, 0)
                .expect("midnight is always valid"),
        }
    }
}

/// How long a timed block lasts.
///
/// Counted in minutes. The UI grid snaps to 5 or 10 of them, but ingested
/// events do not, so any positive count is legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockDuration(NonZeroU32);

impl BlockDuration {
    /// Rejects zero. An instant is not a block, and a zero-length span never
    /// overlaps anything.
    pub fn from_minutes(minutes: u32) -> Result<Self, TimeError> {
        NonZeroU32::new(minutes)
            .map(Self)
            .ok_or(TimeError::ZeroDuration)
    }

    pub fn minutes(self) -> u32 {
        self.0.get()
    }

    pub fn as_time_delta(self) -> TimeDelta {
        TimeDelta::minutes(i64::from(self.0.get()))
    }
}

/// A non-empty absolute half-open interval, `[start, end)`.
/// Used for both resolved occurrence spans and bounded search windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "TimeRangeFields")]
pub struct TimeRange {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

#[derive(Deserialize)]
struct TimeRangeFields {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

impl TryFrom<TimeRangeFields> for TimeRange {
    type Error = TimeError;

    fn try_from(fields: TimeRangeFields) -> Result<Self, Self::Error> {
        Self::new(fields.start, fields.end)
    }
}

impl TimeRange {
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Self, TimeError> {
        if end <= start {
            return Err(TimeError::InvalidRange { start, end });
        }
        Ok(Self { start, end })
    }

    pub fn start(&self) -> DateTime<Utc> {
        self.start
    }

    pub fn end(&self) -> DateTime<Utc> {
        self.end
    }

    pub fn contains(&self, instant: DateTime<Utc>) -> bool {
        self.start <= instant && instant < self.end
    }

    /// Blocks that merely touch do not count as overlapping.
    pub fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Resolve a wall-clock time in a zone to an instant.
///
/// A time in a spring-forward gap shifts forward by the offset change,
/// preserving its position within the gap. An ambiguous time in the autumn
/// overlap takes the earlier (pre-transition) offset.
fn resolve_local(zone: Tz, local: NaiveDateTime) -> Option<DateTime<Utc>> {
    match zone.from_local_datetime(&local) {
        LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        LocalResult::Ambiguous(first, second) => Some(first.min(second).with_timezone(&Utc)),
        // Shift the wall clock forward by the transition's offset change,
        // which keeps the time's position inside the gap. Jumping to the next
        // valid whole hour instead gets half-hour transitions wrong (Lord
        // Howe), and a four-hour search ceiling cannot cross Samoa's skipped
        // date.
        LocalResult::None => {
            const SEARCH_MINUTES: i64 = 48 * 60;
            let before = (1..=SEARCH_MINUTES)
                .find_map(|minutes| one_local(zone, local - TimeDelta::minutes(minutes)))?;
            let after = (1..=SEARCH_MINUTES)
                .find_map(|minutes| one_local(zone, local + TimeDelta::minutes(minutes)))?;
            let offset_change = i64::from(
                after.offset().fix().local_minus_utc() - before.offset().fix().local_minus_utc(),
            );
            if offset_change <= 0 {
                return None;
            }
            one_local(zone, local + TimeDelta::seconds(offset_change))
                .map(|resolved| resolved.with_timezone(&Utc))
        }
    }
}

fn one_local(zone: Tz, local: NaiveDateTime) -> Option<DateTime<Tz>> {
    match zone.from_local_datetime(&local) {
        LocalResult::Single(dt) => Some(dt),
        LocalResult::Ambiguous(first, second) => Some(first.min(second)),
        LocalResult::None => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("time range start {start} must be before end {end}")]
    InvalidRange {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
    #[error("a block must last at least one minute")]
    ZeroDuration,
    #[error("local time {local} does not resolve in {zone}")]
    UnresolvableLocalTime { local: NaiveDateTime, zone: Tz },
    #[error("date arithmetic overflowed the representable range")]
    DateOverflow,
}
