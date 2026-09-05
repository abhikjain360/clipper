//! Calendar sources, and the events pulled from them.
//!
//! These are the original calendar fields owned
//! upstream, written only by the sync worker, never edited in Clipper. The
//! user's plan for an ingested event is a separate record, so a refresh that
//! replaces the original wholesale cannot clobber anything the user wrote.
//!
//! Parsing lives here because it is pure. Fetching does not — that needs I/O and
//! belongs to whichever client holds the source.

use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU32,
};

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use chrono_tz::Tz;
use clipper_api_types::ObjectId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    engine::ImportedRuleResolver,
    item::{OccurrenceOverrideData, OverrideChange, OverrideId, RecurrenceId, ScheduleItemId},
    recurrence::{Recurrence, RecurrenceError},
    time::{BlockDuration, ScheduleSpan, TimeError, TimedStart},
};

/// Bound parser work even when `parse_ics` is called outside the HTTP fetcher.
const MAX_ICS_BYTES: usize = 8 * 1024 * 1024;
const MAX_COMPONENTS: usize = 50_000;
const MAX_PROPERTIES: usize = 500_000;

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
/// iCalendar feed URL *is* the credential, so it must never be server-visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarSource {
    pub id: SourceId,
    /// What the user calls it — "Work", "Gmail", "Zoho".
    pub name: String,
    pub kind: SourceKind,
    /// Whether this client should sync it. Per-client, because each device
    /// decides which sources it is responsible for.
    pub enabled: bool,
    /// Only this complete batch contributes events to the current calendar.
    pub active_import: Option<CalendarImport>,
    /// A staged batch to resume after an interrupted upload.
    pub pending_import: Option<CalendarImport>,
    /// Superseded batches awaiting irreversible cleanup.
    pub retired_imports: Vec<CalendarImport>,
}

/// One source fetch, stored once as an encrypted file, with its parsed event objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarImport {
    pub object_id: clipper_api_types::ObjectId,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub events: Vec<clipper_api_types::ObjectId>,
}

impl CalendarSource {
    pub fn contains_event(&self, object_id: &str, event: &IngestedEvent) -> bool {
        self.id == event.source
            && self.active_import.as_ref().is_some_and(|batch| {
                event.belongs_to_import(batch.object_id)
                    && batch.events.iter().any(|id| id.to_string() == object_id)
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum SourceKind {
    /// A read-only iCalendar feed at a private URL. No OAuth and no admin
    /// approval, making it a fallback for a work calendar
    /// whose Workspace blocks third-party apps. It is coarser than the API:
    /// polling only, and it carries no RSVP or attendee detail.
    Ics { url: String },
}

/// An event as the provider describes it. Read-only in Clipper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestedEvent {
    /// Stable domain identity derived from `(source, uid)`. Storage still uses
    /// batch-specific object identities when an import snapshot is replaced.
    pub id: Uuid,
    pub source: SourceId,
    /// Complete original feed. Typed cadences and one-off events remain usable
    /// if it is unavailable; reference-backed recurrence cannot expand without
    /// resolving its raw rule from this snapshot. Together with `uid` this
    /// identifies the original series and its provider overrides.
    pub import: Option<clipper_api_types::ObjectId>,
    /// The provider's own identifier. Stable across edits, and stable across
    /// calendars for the same meeting, which is what makes cross-source
    /// deduplication possible later.
    pub uid: String,
    pub title: String,
    pub description: Option<String>,
    pub span: ScheduleSpan,
    pub recurrence: Recurrence,
    /// Provider-owned overrides to the recurrence set. IDs are derived from
    /// the stable event identity and recurrence position.
    #[serde(default)]
    pub overrides: Vec<OccurrenceOverrideData>,
    pub status: IngestedStatus,
}

impl IngestedEvent {
    /// Both provenance and opaque recurrence must identify the same source event.
    pub fn belongs_to_import(&self, import: ObjectId) -> bool {
        self.import == Some(import)
            && match &self.recurrence {
                Recurrence::Imported {
                    import: target,
                    uid,
                } => *target == import && *uid == self.uid,
                Recurrence::Once | Recurrence::Every(_) => true,
            }
    }

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
    /// Cancelled upstream. Tombstoned rather than erased so that time
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
/// Includes all-day events and invites regardless of organizer or RSVP.
/// Attendance status is retained as metadata rather than used as a filter.
///
/// `import` identifies the immutable raw file this parse came from. Opaque
/// recurrence rules retain only that snapshot ID and their event UID.
pub fn parse_ics(
    text: &str,
    source: SourceId,
    import: ObjectId,
) -> Result<IngestOutcome, IngestError> {
    use calcard::icalendar::ICalendarComponentType;

    let calendar = parse_calendar(text)?;

    let mut outcome = IngestOutcome::default();
    let mut masters = Vec::new();
    let mut overrides: HashMap<String, Vec<&calcard::icalendar::ICalendarComponent>> =
        HashMap::new();

    for component in &calendar.components {
        if component.component_type != ICalendarComponentType::VEvent {
            continue;
        }
        let uid = text_property(component, "UID");
        if property(component, "RECURRENCE-ID").is_some() {
            match uid {
                Some(uid) => overrides.entry(uid).or_default().push(component),
                None => outcome.skipped.push(SkippedEvent {
                    uid: None,
                    reason: IngestError::MissingUid.to_string(),
                }),
            }
        } else {
            masters.push((component, uid));
        }
    }

    for (component, uid) in masters {
        let matching = uid
            .as_ref()
            .and_then(|uid| overrides.remove(uid))
            .unwrap_or_default();
        match event_from_component(component, &matching, source, import, uid.clone()) {
            Ok(event) => outcome.events.push(event),
            Err(reason) => outcome.skipped.push(SkippedEvent {
                uid,
                reason: reason.to_string(),
            }),
        }
    }

    for (uid, orphaned) in overrides {
        for _ in orphaned {
            outcome.skipped.push(SkippedEvent {
                uid: Some(uid.clone()),
                reason: IngestError::MissingRecurringMaster.to_string(),
            });
        }
    }
    Ok(outcome)
}

/// Recover validated opaque recurrence rules from one immutable import file.
///
/// Callers can merge the result for several snapshots, then construct one
/// [`crate::RruleEngine`] and reuse it while expanding their events.
pub fn parse_imported_recurrence_rules(
    text: &str,
    import: ObjectId,
) -> Result<ImportedRuleResolver, IngestError> {
    use calcard::icalendar::ICalendarComponentType;

    let calendar = parse_calendar(text)?;
    let mut resolver = ImportedRuleResolver::new();
    let mut master_uids = HashSet::new();
    for component in &calendar.components {
        if component.component_type != ICalendarComponentType::VEvent
            || property(component, "RECURRENCE-ID").is_some()
        {
            continue;
        }
        let Some(uid) = text_property(component, "UID") else {
            continue;
        };
        if !master_uids.insert(uid.clone()) {
            return Err(IngestError::DuplicateMasterUid(uid));
        }
        let Some(rule) = rrule_text(component)? else {
            continue;
        };
        resolver.insert(import, uid, rule)?;
    }
    Ok(resolver)
}

fn parse_calendar(text: &str) -> Result<calcard::icalendar::ICalendar, IngestError> {
    use calcard::icalendar::ICalendar;

    validate_calendar_envelope(text)?;
    let calendar =
        ICalendar::parse(text).map_err(|error| IngestError::Malformed(format!("{error:?}")))?;
    if calendar.components.len() > MAX_COMPONENTS {
        return Err(IngestError::LimitExceeded("too many calendar components"));
    }
    let property_count = calendar
        .components
        .iter()
        .try_fold(0usize, |total, component| {
            total.checked_add(component.entries.len())
        })
        .ok_or(IngestError::LimitExceeded("too many calendar properties"))?;
    if property_count > MAX_PROPERTIES {
        return Err(IngestError::LimitExceeded("too many calendar properties"));
    }
    Ok(calendar)
}

fn validate_calendar_envelope(text: &str) -> Result<(), IngestError> {
    if text.len() > MAX_ICS_BYTES {
        return Err(IngestError::LimitExceeded("calendar exceeds 8 MiB"));
    }
    let mut nonempty = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = nonempty.next();
    let last = nonempty.next_back();
    let mut has_version = false;
    for line in text.lines().map(str::trim) {
        has_version |= line.eq_ignore_ascii_case("VERSION:2.0");
    }
    if !first.is_some_and(|line| line.eq_ignore_ascii_case("BEGIN:VCALENDAR"))
        || !last.is_some_and(|line| line.eq_ignore_ascii_case("END:VCALENDAR"))
        || !has_version
    {
        return Err(IngestError::Malformed(
            "expected a complete VERSION:2.0 VCALENDAR".to_string(),
        ));
    }
    Ok(())
}

fn event_from_component(
    component: &calcard::icalendar::ICalendarComponent,
    overrides: &[&calcard::icalendar::ICalendarComponent],
    source: SourceId,
    import: ObjectId,
    uid: Option<String>,
) -> Result<IngestedEvent, IngestError> {
    let uid = uid.ok_or(IngestError::MissingUid)?;
    let id = IngestedEvent::derive_id(source, &uid);
    let start = date_time_property(component, "DTSTART")?.ok_or(IngestError::MissingStart)?;
    let end = date_time_property(component, "DTEND")?;
    let duration = duration_property(component)?;

    let span = span_from(&start, end.as_ref(), duration.as_ref(), None)?;
    let recurrence = match rrule_text(component)? {
        Some(rule) => Recurrence::from_imported_rule(
            rule,
            start.date.and_time(start.time.unwrap_or_default()),
            import,
            uid.clone(),
        )?,
        None => Recurrence::Once,
    };
    let overrides = recurrence_overrides(component, overrides, id, &span)?;

    Ok(IngestedEvent {
        id,
        source,
        import: Some(import),
        title: text_property(component, "SUMMARY").unwrap_or_else(|| "(no title)".to_string()),
        description: text_property(component, "DESCRIPTION"),
        span,
        recurrence,
        overrides,
        status: match text_property(component, "STATUS").as_deref() {
            Some("CANCELLED") => IngestedStatus::Cancelled,
            Some("TENTATIVE") => IngestedStatus::Tentative,
            _ => IngestedStatus::Confirmed,
        },
        uid,
    })
}

/// A start as the feed expresses it, before it becomes a [`ScheduleSpan`].
#[derive(Debug, Clone)]
struct FeedTime {
    date: NaiveDate,
    /// `None` for a date-only value, which is what marks an all-day event.
    time: Option<NaiveTime>,
    zone: Option<Tz>,
    utc: bool,
}

impl FeedTime {
    fn local(&self) -> Option<NaiveDateTime> {
        self.time.map(|time| NaiveDateTime::new(self.date, time))
    }

    fn is_floating(&self) -> bool {
        self.time.is_some() && self.zone.is_none() && !self.utc
    }

    fn timed_start(&self) -> Result<TimedStart, IngestError> {
        let local = self.local().ok_or(IngestError::MismatchedDateType)?;
        Ok(match (self.zone, self.utc) {
            (Some(zone), _) => TimedStart::Zoned { local, zone },
            (None, true) => TimedStart::Zoned {
                local,
                zone: Tz::UTC,
            },
            (None, false) => TimedStart::Floating(local),
        })
    }

    fn instant(&self) -> Result<chrono::DateTime<chrono::Utc>, IngestError> {
        if self.is_floating() {
            return Err(IngestError::MismatchedTimeZone);
        }
        Ok(self.timed_start()?.resolve(Tz::UTC)?)
    }
}

fn span_from(
    start: &FeedTime,
    end: Option<&FeedTime>,
    duration: Option<&calcard::icalendar::ICalendarDuration>,
    inherited: Option<&ScheduleSpan>,
) -> Result<ScheduleSpan, IngestError> {
    if end.is_some() && duration.is_some() {
        return Err(IngestError::ConflictingEnd);
    }

    let Some(clock) = start.time else {
        if end.is_some_and(|end| end.time.is_some()) {
            return Err(IngestError::MismatchedDateType);
        }
        // Date-only: an all-day event. DTEND is exclusive in RFC 5545, so a
        // one-day event ends on the following date.
        let days = if let Some(end) = end {
            positive_days((end.date - start.date).num_days())?
        } else if let Some(duration) = duration {
            all_day_duration(duration)?
        } else if let Some(ScheduleSpan::AllDay { days, .. }) = inherited {
            days.get()
        } else {
            1
        };
        return Ok(ScheduleSpan::AllDay {
            start: start.date,
            days: NonZeroU32::new(days).expect("validated positive duration"),
        });
    };

    let local = NaiveDateTime::new(start.date, clock);
    let minutes = match end {
        Some(end) => {
            let end_local = end.local().ok_or(IngestError::MismatchedDateType)?;
            let delta = if start.is_floating() && end.is_floating() {
                end_local - local
            } else if !start.is_floating() && !end.is_floating() {
                end.instant()? - start.instant()?
            } else {
                return Err(IngestError::MismatchedTimeZone);
            };
            duration_minutes(delta.num_seconds())?
        }
        None if duration.is_some() => ical_duration_minutes(duration.expect("checked above"))?,
        None => match inherited {
            Some(ScheduleSpan::Timed { duration, .. }) => duration.minutes(),
            _ => 5,
        },
        // RFC 5545 says a VEVENT with no DTEND and no DURATION lasts a day when
        // date-only, and is instantaneous otherwise. An instant cannot be drawn,
        // so give it the shortest block the grid can show.
    };

    Ok(ScheduleSpan::Timed {
        start: start.timed_start()?,
        duration: BlockDuration::from_minutes(minutes)?,
    })
}

fn positive_days(days: i64) -> Result<u32, IngestError> {
    u32::try_from(days)
        .ok()
        .filter(|days| *days > 0)
        .ok_or(IngestError::InvalidDuration)
}

fn duration_minutes(seconds: i64) -> Result<u32, IngestError> {
    if seconds <= 0 || seconds % 60 != 0 {
        return Err(IngestError::InvalidDuration);
    }
    u32::try_from(seconds / 60).map_err(|_| IngestError::InvalidDuration)
}

fn all_day_duration(duration: &calcard::icalendar::ICalendarDuration) -> Result<u32, IngestError> {
    if duration.neg || duration.hours != 0 || duration.minutes != 0 || duration.seconds != 0 {
        return Err(IngestError::InvalidDuration);
    }
    let days = u64::from(duration.weeks)
        .checked_mul(7)
        .and_then(|weeks| weeks.checked_add(u64::from(duration.days)))
        .and_then(|days| u32::try_from(days).ok())
        .ok_or(IngestError::InvalidDuration)?;
    NonZeroU32::new(days)
        .map(NonZeroU32::get)
        .ok_or(IngestError::InvalidDuration)
}

fn ical_duration_minutes(
    duration: &calcard::icalendar::ICalendarDuration,
) -> Result<u32, IngestError> {
    if duration.neg {
        return Err(IngestError::InvalidDuration);
    }
    let seconds = u64::from(duration.weeks)
        .checked_mul(7 * 24 * 60 * 60)
        .and_then(|value| value.checked_add(u64::from(duration.days) * 24 * 60 * 60))
        .and_then(|value| value.checked_add(u64::from(duration.hours) * 60 * 60))
        .and_then(|value| value.checked_add(u64::from(duration.minutes) * 60))
        .and_then(|value| value.checked_add(u64::from(duration.seconds)))
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(IngestError::InvalidDuration)?;
    duration_minutes(seconds)
}

fn duration_property(
    component: &calcard::icalendar::ICalendarComponent,
) -> Result<Option<calcard::icalendar::ICalendarDuration>, IngestError> {
    use calcard::icalendar::ICalendarValue;

    let Some(entry) = component
        .entries
        .iter()
        .find(|entry| entry.name.as_str().eq_ignore_ascii_case("DURATION"))
    else {
        return Ok(None);
    };
    match entry.values.first() {
        Some(ICalendarValue::Duration(duration)) => Ok(Some(duration.clone())),
        _ => Err(IngestError::InvalidDuration),
    }
}

fn recurrence_overrides(
    master: &calcard::icalendar::ICalendarComponent,
    overrides: &[&calcard::icalendar::ICalendarComponent],
    event_id: Uuid,
    master_span: &ScheduleSpan,
) -> Result<Vec<OccurrenceOverrideData>, IngestError> {
    use std::collections::BTreeMap;

    let item = ScheduleItemId(event_id);
    let mut by_recurrence_id = BTreeMap::new();

    for added in recurrence_times(master, "RDATE")? {
        let recurrence_id = recurrence_id_for(master_span, &added)?;
        let span = span_at(master_span, &added)?;
        by_recurrence_id.insert(
            recurrence_id,
            make_override(
                event_id,
                item,
                recurrence_id,
                OverrideChange::Rescheduled(span),
            ),
        );
    }

    for override_data in overrides {
        let recurrence_entry =
            property(override_data, "RECURRENCE-ID").ok_or(IngestError::MissingRecurrenceId)?;
        if recurrence_entry.params.iter().any(|parameter| {
            matches!(
                parameter.name,
                calcard::icalendar::ICalendarParameterName::Range
            )
        }) {
            return Err(IngestError::UnsupportedRecurrenceRange);
        }
        let recurrence_time = feed_time_from_entry(recurrence_entry)?;
        let recurrence_id = recurrence_id_for(master_span, &recurrence_time)?;
        let change = if text_property(override_data, "STATUS").as_deref() == Some("CANCELLED") {
            OverrideChange::Cancelled
        } else {
            let start =
                date_time_property(override_data, "DTSTART")?.ok_or(IngestError::MissingStart)?;
            let end = date_time_property(override_data, "DTEND")?;
            let duration = duration_property(override_data)?;
            OverrideChange::Rescheduled(span_from(
                &start,
                end.as_ref(),
                duration.as_ref(),
                Some(master_span),
            )?)
        };
        by_recurrence_id.insert(
            recurrence_id,
            make_override(event_id, item, recurrence_id, change),
        );
    }

    // RFC 5545 gives EXDATE precedence over inclusion dates. Apply it last so
    // a duplicated RDATE, or a detached component for the same recurrence
    // position, cannot accidentally resurrect an explicitly excluded date.
    for excluded in recurrence_times(master, "EXDATE")? {
        let recurrence_id = recurrence_id_for(master_span, &excluded)?;
        by_recurrence_id.insert(
            recurrence_id,
            make_override(event_id, item, recurrence_id, OverrideChange::Cancelled),
        );
    }

    Ok(by_recurrence_id.into_values().collect())
}

fn make_override(
    event_id: Uuid,
    item: ScheduleItemId,
    recurrence_id: RecurrenceId,
    change: OverrideChange,
) -> OccurrenceOverrideData {
    let stable_name = format!("{recurrence_id:?}");
    OccurrenceOverrideData {
        id: OverrideId(Uuid::new_v5(&event_id, stable_name.as_bytes())),
        item,
        recurrence_id,
        change,
    }
}

fn recurrence_id_for(
    master_span: &ScheduleSpan,
    time: &FeedTime,
) -> Result<RecurrenceId, IngestError> {
    match master_span {
        ScheduleSpan::AllDay { .. } if time.time.is_none() => Ok(RecurrenceId::Date(time.date)),
        ScheduleSpan::Timed {
            start: TimedStart::Floating(_),
            ..
        } if time.is_floating() => Ok(RecurrenceId::Floating(
            time.local().expect("floating times are timed"),
        )),
        ScheduleSpan::Timed {
            start: TimedStart::Zoned { .. },
            ..
        } if time.time.is_some() && !time.is_floating() => {
            Ok(RecurrenceId::Instant(time.instant()?))
        }
        _ => Err(IngestError::MismatchedDateType),
    }
}

fn span_at(master_span: &ScheduleSpan, time: &FeedTime) -> Result<ScheduleSpan, IngestError> {
    match master_span {
        ScheduleSpan::Timed { duration, .. } if time.time.is_some() => Ok(ScheduleSpan::Timed {
            start: time.timed_start()?,
            duration: *duration,
        }),
        ScheduleSpan::AllDay { days, .. } if time.time.is_none() => Ok(ScheduleSpan::AllDay {
            start: time.date,
            days: *days,
        }),
        _ => Err(IngestError::MismatchedDateType),
    }
}

fn recurrence_times(
    component: &calcard::icalendar::ICalendarComponent,
    name: &str,
) -> Result<Vec<FeedTime>, IngestError> {
    use calcard::icalendar::ICalendarValue;

    let mut times = Vec::new();
    for entry in component
        .entries
        .iter()
        .filter(|entry| entry.name.as_str().eq_ignore_ascii_case(name))
    {
        for value in &entry.values {
            match value {
                ICalendarValue::PartialDateTime(partial) => {
                    times.push(feed_time_from_partial(entry, partial)?);
                }
                _ => return Err(IngestError::UnsupportedRecurrenceDate),
            }
        }
    }
    Ok(times)
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

fn rrule_text(
    component: &calcard::icalendar::ICalendarComponent,
) -> Result<Option<String>, IngestError> {
    use calcard::icalendar::ICalendarValue;

    let mut entries = component
        .entries
        .iter()
        .filter(|entry| entry.name.as_str().eq_ignore_ascii_case("RRULE"));
    let Some(entry) = entries.next() else {
        return Ok(None);
    };
    if entries.next().is_some() || entry.values.len() != 1 {
        return Err(IngestError::AmbiguousRecurrenceRule);
    }
    match entry.values.first() {
        // calcard round-trips a parsed rule back to RFC 5545 text, which is
        // what the expansion engine wants — no re-derivation here.
        Some(ICalendarValue::RecurrenceRule(rule)) => Ok(Some(rule.to_string())),
        Some(ICalendarValue::Text(text)) => Ok(Some(text.clone())),
        _ => Err(IngestError::AmbiguousRecurrenceRule),
    }
}

fn date_time_property(
    component: &calcard::icalendar::ICalendarComponent,
    name: &str,
) -> Result<Option<FeedTime>, IngestError> {
    let Some(entry) = property(component, name) else {
        return Ok(None);
    };
    feed_time_from_entry(entry).map(Some)
}

fn property<'a>(
    component: &'a calcard::icalendar::ICalendarComponent,
    name: &str,
) -> Option<&'a calcard::icalendar::ICalendarEntry> {
    component
        .entries
        .iter()
        .find(|entry| entry.name.as_str().eq_ignore_ascii_case(name))
}

fn feed_time_from_entry(
    entry: &calcard::icalendar::ICalendarEntry,
) -> Result<FeedTime, IngestError> {
    use calcard::icalendar::ICalendarValue;

    let Some(ICalendarValue::PartialDateTime(partial)) = entry.values.first() else {
        return Err(IngestError::InvalidDateTime);
    };
    feed_time_from_partial(entry, partial)
}

fn feed_time_from_partial(
    entry: &calcard::icalendar::ICalendarEntry,
    partial: &calcard::common::PartialDateTime,
) -> Result<FeedTime, IngestError> {
    use calcard::icalendar::{ICalendarParameterName, ICalendarParameterValue};

    let date = NaiveDate::from_ymd_opt(
        i32::from(partial.year.ok_or(IngestError::InvalidDateTime)?),
        u32::from(partial.month.ok_or(IngestError::InvalidDateTime)?),
        u32::from(partial.day.ok_or(IngestError::InvalidDateTime)?),
    )
    .ok_or(IngestError::InvalidDateTime)?;
    let time = partial
        .hour
        .map(|hour| {
            NaiveTime::from_hms_opt(
                u32::from(hour),
                u32::from(partial.minute.unwrap_or(0)),
                u32::from(partial.second.unwrap_or(0)),
            )
            .ok_or(IngestError::InvalidDateTime)
        })
        .transpose()?;

    let zone = match entry
        .params
        .iter()
        .find(|param| matches!(param.name, ICalendarParameterName::Tzid))
    {
        Some(param) => match &param.value {
            ICalendarParameterValue::Text(name) => Some(
                name.parse::<Tz>()
                    .map_err(|_| IngestError::UnknownTimeZone(name.clone()))?,
            ),
            _ => return Err(IngestError::InvalidDateTime),
        },
        None => None,
    };
    if zone.is_some() && partial.tz_hour.is_some() {
        return Err(IngestError::MismatchedTimeZone);
    }
    if partial.tz_hour.is_some()
        && (partial.tz_hour != Some(0) || partial.tz_minute.unwrap_or(0) != 0)
    {
        // RFC 5545 DATE-TIME permits UTC (`Z`) or a TZID, not a numeric UTC
        // offset. The domain model intentionally has no fixed-offset zone, so
        // accepting one as UTC would move the event.
        return Err(IngestError::UnsupportedUtcOffset);
    }

    Ok(FeedTime {
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
    #[error("calendar feed limit exceeded: {0}")]
    LimitExceeded(&'static str),
    #[error("event has no UID, so it cannot be tracked across refreshes")]
    MissingUid,
    #[error("event has no DTSTART")]
    MissingStart,
    #[error("recurrence override has no RECURRENCE-ID")]
    MissingRecurrenceId,
    #[error("recurrence override has no matching master event")]
    MissingRecurringMaster,
    #[error("calendar snapshot has more than one master event with UID {0:?}")]
    DuplicateMasterUid(String),
    #[error("event must contain at most one RRULE value")]
    AmbiguousRecurrenceRule,
    #[error("event contains an invalid date or time")]
    InvalidDateTime,
    #[error("TZID {0:?} is not an IANA time zone known to this build")]
    UnknownTimeZone(String),
    #[error("DTSTART and DTEND use incompatible date or date-time values")]
    MismatchedDateType,
    #[error("DTSTART and DTEND mix floating and absolute time")]
    MismatchedTimeZone,
    #[error("numeric UTC offsets are not supported in iCalendar DATE-TIME values")]
    UnsupportedUtcOffset,
    #[error("event has both DTEND and DURATION")]
    ConflictingEnd,
    #[error("event duration must be positive, fit in minutes, and match its value type")]
    InvalidDuration,
    #[error("RDATE periods are not supported")]
    UnsupportedRecurrenceDate,
    #[error("RECURRENCE-ID;RANGE=THISANDFUTURE is not supported")]
    UnsupportedRecurrenceRange,
    #[error(transparent)]
    Time(#[from] TimeError),
    #[error(transparent)]
    Recurrence(#[from] RecurrenceError),
}
