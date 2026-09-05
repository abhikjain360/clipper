//! Calendar sources, and the events pulled from them.
//!
//! This is the "original" layer of `docs/schedule-plan.md`'s D10: fields owned
//! upstream, written only by the sync worker, never edited in Clipper. The
//! owner's plan for an ingested event is a separate record, so a refresh that
//! replaces the original wholesale cannot clobber anything the owner wrote.
//!
//! Parsing lives here because it is pure. Fetching does not — that needs I/O and
//! belongs to whichever client holds the source (D4).

use std::num::NonZeroU32;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    recurrence::{RawRule, Recurrence, RecurrenceError},
    time::{BlockDuration, ScheduleSpan, TimeError, TimedStart},
};

/// Identifies a calendar source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceId(pub Uuid);

impl SourceId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SourceId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A calendar Clipper pulls events from.
///
/// Stored as an encrypted object like everything else, which matters here: an
/// iCalendar feed URL *is* the credential, so it must never be server-visible
/// (D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarSource {
    pub id: SourceId,
    /// What the owner calls it — "Work", "Gmail", "Zoho".
    pub name: String,
    pub kind: SourceKind,
    /// Whether this client should sync it. Per-client, because each device
    /// decides which sources it is responsible for (D4).
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum SourceKind {
    /// A read-only iCalendar feed at a private URL. No OAuth and no admin
    /// approval, which is why D9 keeps it as the fallback for a work calendar
    /// whose Workspace blocks third-party apps. It is coarser than the API:
    /// polling only, and it carries no RSVP or attendee detail.
    Ics { url: String },
}

/// An event as the provider describes it. Read-only in Clipper (D10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestedEvent {
    /// Derived from `(source, uid)` rather than random, so re-ingesting a feed
    /// updates each event in place instead of duplicating it.
    pub id: Uuid,
    pub source: SourceId,
    /// The provider's own identifier. Stable across edits, and stable across
    /// calendars for the same meeting, which is what makes cross-source
    /// deduplication possible later.
    pub uid: String,
    pub title: String,
    pub description: Option<String>,
    pub span: ScheduleSpan,
    pub recurrence: Recurrence,
    pub status: IngestedStatus,
}

impl IngestedEvent {
    /// The stable id for an event, given its source and provider uid.
    pub fn derive_id(source: SourceId, uid: &str) -> Uuid {
        // A fixed namespace so the derivation is reproducible across devices —
        // two clients ingesting the same feed must agree on ids.
        const NAMESPACE: Uuid = Uuid::from_u128(0x9f2c_4c6e_5d17_4c9b_a1e8_3f0b_7d24_88a1);
        Uuid::new_v5(&NAMESPACE, format!("{source}:{uid}").as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestedStatus {
    Confirmed,
    Tentative,
    /// Cancelled upstream. Tombstoned rather than erased (D10) so that time
    /// already logged against the meeting survives it.
    Cancelled,
}

/// What one pass over a feed produced.
///
/// Skipped events are reported rather than swallowed: a feed that silently
/// drops half its entries is worse than one that says which it could not read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestOutcome {
    pub events: Vec<IngestedEvent>,
    pub skipped: Vec<SkippedEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEvent {
    pub uid: Option<String>,
    pub reason: String,
}

/// Parse an iCalendar feed into events.
///
/// Deliberately unfiltered. abnormalarm admits an event only if the owner
/// organizes it, has accepted it, or it has no attendees, and drops all-day
/// events entirely — rules that suit an alarm app. A planner wants the
/// opposite: every invite visible, all-day included, with RSVP shown as a
/// property rather than used as a filter (D9).
pub fn parse_ics(text: &str, source: SourceId) -> Result<IngestOutcome, IngestError> {
    use calcard::icalendar::{ICalendar, ICalendarComponentType};

    let calendar =
        ICalendar::parse(text).map_err(|error| IngestError::Malformed(format!("{error:?}")))?;
    let mut outcome = IngestOutcome::default();

    for component in &calendar.components {
        if component.component_type != ICalendarComponentType::VEvent {
            continue;
        }
        let uid = text_property(component, "UID");
        match event_from_component(component, source, uid.clone()) {
            Ok(event) => outcome.events.push(event),
            Err(reason) => outcome.skipped.push(SkippedEvent {
                uid,
                reason: reason.to_string(),
            }),
        }
    }
    Ok(outcome)
}

fn event_from_component(
    component: &calcard::icalendar::ICalendarComponent,
    source: SourceId,
    uid: Option<String>,
) -> Result<IngestedEvent, IngestError> {
    let uid = uid.ok_or(IngestError::MissingUid)?;
    let start = date_time_property(component, "DTSTART").ok_or(IngestError::MissingStart)?;
    let end = date_time_property(component, "DTEND");

    let span = span_from(&start, end.as_ref())?;
    let recurrence = match rrule_text(component) {
        Some(rule) => Recurrence::Raw(RawRule::new(rule)?),
        None => Recurrence::Once,
    };

    Ok(IngestedEvent {
        id: IngestedEvent::derive_id(source, &uid),
        source,
        title: text_property(component, "SUMMARY").unwrap_or_else(|| "(no title)".to_string()),
        description: text_property(component, "DESCRIPTION"),
        span,
        recurrence,
        status: match text_property(component, "STATUS").as_deref() {
            Some("CANCELLED") => IngestedStatus::Cancelled,
            Some("TENTATIVE") => IngestedStatus::Tentative,
            _ => IngestedStatus::Confirmed,
        },
        uid,
    })
}

/// A start as the feed expresses it, before it becomes a [`ScheduleSpan`].
struct FeedTime {
    date: NaiveDate,
    /// `None` for a date-only value, which is what marks an all-day event.
    time: Option<NaiveTime>,
    zone: Option<Tz>,
    utc: bool,
}

fn span_from(start: &FeedTime, end: Option<&FeedTime>) -> Result<ScheduleSpan, IngestError> {
    let Some(clock) = start.time else {
        // Date-only: an all-day event. DTEND is exclusive in RFC 5545, so a
        // one-day event ends on the following date.
        let days = end
            .map(|end| (end.date - start.date).num_days())
            .filter(|days| *days > 0)
            .unwrap_or(1);
        return Ok(ScheduleSpan::AllDay {
            start: start.date,
            days: NonZeroU32::new(days as u32).unwrap_or(NonZeroU32::MIN),
        });
    };

    let local = NaiveDateTime::new(start.date, clock);
    let minutes = match end.and_then(|end| end.time.map(|time| (end.date, time))) {
        Some((end_date, end_time)) => {
            let delta = NaiveDateTime::new(end_date, end_time) - local;
            delta.num_minutes().max(1) as u32
        }
        // RFC 5545 says a VEVENT with no DTEND and no DURATION lasts a day when
        // date-only, and is instantaneous otherwise. An instant cannot be drawn,
        // so give it the shortest block the grid can show.
        None => 5,
    };

    Ok(ScheduleSpan::Timed {
        start: match (start.zone, start.utc) {
            (Some(zone), _) => TimedStart::Zoned { local, zone },
            (None, true) => TimedStart::Zoned {
                local,
                zone: Tz::UTC,
            },
            // No TZID and no Z suffix is RFC 5545 floating time, and it means
            // exactly what Clipper's floating means: this wall clock, wherever
            // you are.
            (None, false) => TimedStart::Floating(local),
        },
        duration: BlockDuration::from_minutes(minutes)?,
    })
}

fn text_property(component: &calcard::icalendar::ICalendarComponent, name: &str) -> Option<String> {
    use calcard::{common::IanaString, icalendar::ICalendarValue};

    component
        .entries
        .iter()
        .find(|entry| entry.name.as_str().eq_ignore_ascii_case(name))
        .and_then(|entry| match entry.values.first() {
            Some(ICalendarValue::Text(text)) => Some(text.clone()),
            Some(ICalendarValue::Status(status)) => Some(status.as_str().to_string()),
            _ => None,
        })
}

fn rrule_text(component: &calcard::icalendar::ICalendarComponent) -> Option<String> {
    use calcard::icalendar::ICalendarValue;

    component
        .entries
        .iter()
        .find(|entry| entry.name.as_str().eq_ignore_ascii_case("RRULE"))
        .and_then(|entry| match entry.values.first() {
            // calcard round-trips a parsed rule back to RFC 5545 text, which is
            // what the expansion engine wants — no re-derivation here.
            Some(ICalendarValue::RecurrenceRule(rule)) => Some(rule.to_string()),
            Some(ICalendarValue::Text(text)) => Some(text.clone()),
            _ => None,
        })
}

fn date_time_property(
    component: &calcard::icalendar::ICalendarComponent,
    name: &str,
) -> Option<FeedTime> {
    use calcard::icalendar::{ICalendarParameterName, ICalendarParameterValue, ICalendarValue};

    let entry = component
        .entries
        .iter()
        .find(|entry| entry.name.as_str().eq_ignore_ascii_case(name))?;
    let ICalendarValue::PartialDateTime(partial) = entry.values.first()? else {
        return None;
    };

    let date = NaiveDate::from_ymd_opt(
        i32::from(partial.year?),
        u32::from(partial.month?),
        u32::from(partial.day?),
    )?;
    let time = partial.hour.and_then(|hour| {
        NaiveTime::from_hms_opt(
            u32::from(hour),
            u32::from(partial.minute.unwrap_or(0)),
            u32::from(partial.second.unwrap_or(0)),
        )
    });

    let zone = entry.params.iter().find_map(|param| {
        matches!(param.name, ICalendarParameterName::Tzid)
            .then(|| match &param.value {
                ICalendarParameterValue::Text(name) => name.parse::<Tz>().ok(),
                _ => None,
            })
            .flatten()
    });

    Some(FeedTime {
        date,
        time,
        zone,
        // A trailing Z parses as a zero UTC offset rather than as no offset.
        utc: zone.is_none() && partial.tz_hour.is_some(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IngestError {
    #[error("calendar feed could not be parsed: {0}")]
    Malformed(String),
    #[error("event has no UID, so it cannot be tracked across refreshes")]
    MissingUid,
    #[error("event has no DTSTART")]
    MissingStart,
    #[error(transparent)]
    Time(#[from] TimeError),
    #[error(transparent)]
    Recurrence(#[from] RecurrenceError),
}
