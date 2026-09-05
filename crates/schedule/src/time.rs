//! When a block starts and how long it lasts.
//!
//! RFC 5545 distinguishes three kinds of start and conflating them breaks the
//! alarm path (`docs/schedule-plan.md`, D5). A floating 07:00 alarm follows the
//! device across timezones; a zoned meeting stays pinned to the zone it was
//! scheduled in; an all-day event has no instant at all.
//!
//! The span type makes the invalid combinations unrepresentable: an all-day
//! event cannot carry a minute duration, and a timed event cannot lack a time.

use std::num::NonZeroU32;

use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// A start that has a time of day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimedStart {
    /// Wall-clock time with no zone. Follows the observer: 07:00 is 07:00
    /// wherever you wake up. This is what an alarm wants.
    Floating(NaiveDateTime),
    /// Wall-clock time pinned to an IANA zone. Stays put when you travel; the
    /// local time is stored rather than an instant so that a 09:00 Berlin
    /// meeting is still 09:00 after a DST transition.
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
/// Splitting timed from all-day at the type level is deliberate: D9 commits to
/// ingesting all-day events, and coercing a date into an instant silently moves
/// it across a border.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn resolve(&self, observer: Tz) -> Result<ResolvedSpan, TimeError> {
        match self {
            Self::Timed { start, duration } => {
                let begin = start.resolve(observer)?;
                Ok(ResolvedSpan {
                    start: begin,
                    end: begin + duration.as_time_delta(),
                })
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
                Ok(ResolvedSpan {
                    start: resolve_local(observer, begin_local).ok_or(
                        TimeError::UnresolvableLocalTime {
                            local: begin_local,
                            zone: observer,
                        },
                    )?,
                    end: resolve_local(observer, end_local).ok_or(
                        TimeError::UnresolvableLocalTime {
                            local: end_local,
                            zone: observer,
                        },
                    )?,
                })
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
/// Stored in minutes because the UI grid snaps to 5 or 10 of them (D5), but
/// ingested events are not grid-aligned so any positive count is legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockDuration(NonZeroU32);

impl BlockDuration {
    /// A duration of `minutes`. Zero-length blocks are rejected: an instant is
    /// not a block, and a zero duration breaks overlap detection.
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

/// An absolute half-open interval, `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSpan {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl ResolvedSpan {
    /// Half-open, so blocks that merely touch do not count as overlapping.
    pub fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Resolve a wall-clock time in a zone to an instant.
///
/// DST edges follow `java.time`, which is what abnormalarm already does and
/// therefore what the owner's alarms have behaved like for months: a time in a
/// spring-forward gap shifts forward past the gap, and an ambiguous time in the
/// autumn overlap takes the earlier (pre-transition) offset.
fn resolve_local(zone: Tz, local: NaiveDateTime) -> Option<DateTime<Utc>> {
    match zone.from_local_datetime(&local) {
        LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        LocalResult::Ambiguous(earlier, _later) => Some(earlier.with_timezone(&Utc)),
        // Spring-forward gap. No real transition exceeds a few hours, so probe
        // forward rather than hard-coding an offset delta.
        LocalResult::None => (1..=4).find_map(|hours| {
            match zone.from_local_datetime(&(local + TimeDelta::hours(hours))) {
                LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
                LocalResult::Ambiguous(earlier, _) => Some(earlier.with_timezone(&Utc)),
                LocalResult::None => None,
            }
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("a block must last at least one minute")]
    ZeroDuration,
    #[error("local time {local} does not resolve in {zone}")]
    UnresolvableLocalTime { local: NaiveDateTime, zone: Tz },
    #[error("date arithmetic overflowed the representable range")]
    DateOverflow,
}
