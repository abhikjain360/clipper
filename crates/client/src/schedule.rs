//! Sealing schedule records into objects, and rendering them for display.
//!
//! A schedule object has the same shape as a clipboard one: a small encrypted
//! meta saying what the payload is, plus one payload holding the record. A
//! record is a few hundred bytes, so the payload always travels inline and the
//! object is complete the moment `object_init` returns.
//!
//! The server sees an object of kind `schedule` and nothing more. Whether it
//! holds a plan, an override, or a log of time spent is `ScheduleRecordKind`,
//! inside the ciphertext.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use clipper_app_types::{ActualView, CalendarSourceView, OccurrenceView, ScheduleItemView};
use clipper_core::{
    crypto,
    models::{
        ObjectEnvelopeBody, ObjectPayloadId, SCHEDULE_PAYLOAD_VERSION, ScheduleMeta,
        ScheduleRecordKind,
    },
};
use clipper_schedule::{Occurrence, OccurrenceOrigin, ScheduleItem, ScheduleSpan};

/// A schedule record, in the form it is serialized into an object payload.
///
/// One tagged enum rather than a payload shape per kind, so the meta's
/// discriminant and the payload cannot disagree. Deserializing checks the tag
/// either way.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum ScheduleRecord {
    Item(Box<ScheduleItem>),
    Override(Box<clipper_schedule::OccurrenceOverride>),
    Actual(Box<clipper_schedule::ActualRecord>),
    Source(Box<clipper_schedule::CalendarSource>),
    Ingested(Box<clipper_schedule::IngestedEvent>),
}

impl ScheduleRecord {
    pub fn kind(&self) -> ScheduleRecordKind {
        match self {
            Self::Item(_) => ScheduleRecordKind::Item,
            Self::Override(_) => ScheduleRecordKind::Override,
            Self::Actual(_) => ScheduleRecordKind::Actual,
            Self::Source(_) => ScheduleRecordKind::Source,
            Self::Ingested(_) => ScheduleRecordKind::Ingested,
        }
    }

    pub fn meta(&self) -> ScheduleMeta {
        ScheduleMeta {
            record: self.kind(),
            version: SCHEDULE_PAYLOAD_VERSION,
        }
    }

    /// The series definition, if this is one.
    pub fn as_item(&self) -> Option<&ScheduleItem> {
        match self {
            Self::Item(item) => Some(item),
            Self::Override(_) | Self::Actual(_) | Self::Source(_) | Self::Ingested(_) => None,
        }
    }

    pub fn as_source(&self) -> Option<&clipper_schedule::CalendarSource> {
        match self {
            Self::Source(source) => Some(source),
            Self::Item(_) | Self::Override(_) | Self::Actual(_) | Self::Ingested(_) => None,
        }
    }

    pub fn as_ingested(&self) -> Option<&clipper_schedule::IngestedEvent> {
        match self {
            Self::Ingested(event) => Some(event),
            Self::Item(_) | Self::Override(_) | Self::Actual(_) | Self::Source(_) => None,
        }
    }

    /// Both owned blocks and imported meetings can be the plan for logged time.
    pub fn planned_title(&self) -> Option<(clipper_schedule::ScheduleItemId, &str)> {
        match self {
            Self::Item(item) => Some((item.id, &item.title)),
            Self::Ingested(event) => {
                Some((clipper_schedule::ScheduleItemId(event.id), &event.title))
            }
            Self::Override(_) | Self::Actual(_) | Self::Source(_) => None,
        }
    }
}

/// Encrypt a schedule object's metadata.
pub fn encrypt_schedule_meta(
    meta: &ScheduleMeta,
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBody,
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let json = serde_json::to_vec(meta)
        .map_err(|e| crypto::CryptoError::Encrypt(format!("json: {}", e)))?;
    let aad = crypto::object_meta_aad(envelope_body)?;
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, &json, &aad)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt a schedule object's metadata.
pub fn decrypt_schedule_meta(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBody,
) -> Result<ScheduleMeta, crypto::CryptoError> {
    let aad = crypto::object_meta_aad(envelope_body)?;
    let plaintext = crypto::decrypt(encryption_key, nonce, ciphertext, &aad)?;
    serde_json::from_slice(&plaintext)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("json: {}", e)))
}

/// Encrypt a schedule record into an object payload.
pub fn encrypt_schedule_payload(
    record: &ScheduleRecord,
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBody,
    payload_id: ObjectPayloadId,
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let json = serde_json::to_vec(record)
        .map_err(|e| crypto::CryptoError::Encrypt(format!("json: {}", e)))?;
    let aad = crypto::object_payload_aad(envelope_body, payload_id)?;
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, &json, &aad)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt a schedule record from an object payload.
pub fn decrypt_schedule_payload(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBody,
    payload_id: ObjectPayloadId,
) -> Result<ScheduleRecord, crypto::CryptoError> {
    let aad = crypto::object_payload_aad(envelope_body, payload_id)?;
    let plaintext = crypto::decrypt(encryption_key, nonce, ciphertext, &aad)?;
    serde_json::from_slice(&plaintext)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("json: {}", e)))
}

/// Render a series for a list.
pub fn item_view(
    item: &ScheduleItem,
    object_id: &str,
    created_at: &str,
    revision: u64,
) -> ScheduleItemView {
    ScheduleItemView {
        // The object id, not the series id. Edits and deletes address this
        // one. The series id lives inside `definition_json` and survives an
        // edit.
        id: object_id.to_string(),
        revision,
        title: item.title.clone(),
        recurrence: item.recurrence.summary(),
        time_summary: item.time_summary(),
        all_day: matches!(item.span, ScheduleSpan::AllDay { .. }),
        has_alarm: item.alarm.is_some(),
        created_at: created_at.to_string(),
        definition_json: serde_json::to_string(item).unwrap_or_default(),
    }
}

/// How an occurrence is labelled: what it is called, where it came from, and
/// whether the provider has cancelled it.
pub struct OccurrenceLabel<'a> {
    pub title: &'a str,
    pub all_day: bool,
    /// The calendar's name, for an ingested event. `None` for an owned block.
    pub source: Option<&'a str>,
    pub cancelled: bool,
}

/// Render a record of time spent.
///
/// `title` comes from the pinned historical definition, or says it is
/// unavailable. The current title is never put in its place.
pub fn actual_view(
    object_id: &str,
    actual: &clipper_schedule::ActualRecord,
    title: &str,
) -> ActualView {
    let (start, end, running) = match actual.span {
        clipper_schedule::ActualSpan::Running { started } => {
            (to_rfc3339(started), String::new(), true)
        }
        clipper_schedule::ActualSpan::Complete(span) => {
            (to_rfc3339(span.start()), to_rfc3339(span.end()), false)
        }
    };
    ActualView {
        id: object_id.to_string(),
        item_id: actual
            .planned
            .map(|planned| planned.item.to_string())
            .unwrap_or_default(),
        title: title.to_string(),
        start,
        end,
        running,
    }
}

/// A stable string for one occurrence of a series.
///
/// Platform and UI layers put this key in an intent extra or a button handler
/// and compare it later. They never need the three ways an occurrence can be
/// identified, only that the same occurrence gives the same string.
pub fn occurrence_key(recurrence_id: &clipper_schedule::RecurrenceId) -> String {
    match recurrence_id {
        clipper_schedule::RecurrenceId::Floating(local) => format!("floating:{local:?}"),
        clipper_schedule::RecurrenceId::Instant(instant) => {
            format!("instant:{}", instant.timestamp_millis())
        }
        clipper_schedule::RecurrenceId::Date(date) => format!("date:{date}"),
    }
}

/// Parse a key produced by [`occurrence_key`].
///
/// The platform and UI layers carry occurrence identity as an opaque string.
/// This is the only place that reads its shape.
pub fn parse_occurrence_key(key: &str) -> Option<clipper_schedule::RecurrenceId> {
    let (kind, value) = key.split_once(':')?;
    match kind {
        "floating" => value
            .parse()
            .ok()
            .map(clipper_schedule::RecurrenceId::Floating),
        "instant" => value
            .parse::<i64>()
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(clipper_schedule::RecurrenceId::Instant),
        "date" => value
            .parse::<chrono::NaiveDate>()
            .ok()
            .map(clipper_schedule::RecurrenceId::Date),
        _ => None,
    }
}

/// Render one computed occurrence for a grid.
pub fn occurrence_view(
    occurrence: &Occurrence,
    label: OccurrenceLabel<'_>,
    context: &clipper_schedule::PlannedRef,
) -> OccurrenceView {
    OccurrenceView {
        item_id: occurrence.item.to_string(),
        occurrence_key: occurrence_key(&occurrence.recurrence_id),
        plan_context: serde_json::to_string(context).expect("plan context is serializable"),
        title: label.title.to_string(),
        start: to_rfc3339(occurrence.span.start()),
        end: to_rfc3339(occurrence.span.end()),
        all_day: label.all_day,
        overridden: matches!(occurrence.origin, OccurrenceOrigin::Overridden(_)),
        source: label.source.map(str::to_string),
        cancelled: label.cancelled,
    }
}

/// Present an ingested event to the expansion engine.
///
/// The engine expands series. An ingested event has a span and a rule, so this
/// wraps it in a throwaway [`ScheduleItem`] rather than making the engine
/// generic over two near-identical shapes. The id is the event's own derived
/// id, so it stays stable across refreshes.
pub fn ingested_as_series(event: &clipper_schedule::IngestedEvent) -> ScheduleItem {
    ScheduleItem {
        id: clipper_schedule::ScheduleItemId(event.id),
        title: event.title.clone(),
        span: event.span.clone(),
        recurrence: event.recurrence.clone(),
        reference: None,
        alarm: None,
    }
}

fn to_rfc3339(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Render a calendar source for a list.
///
/// The caller passes `event_count` because it already holds every record.
pub fn source_view(
    object_id: &str,
    source: &clipper_schedule::CalendarSource,
    event_count: u32,
    raw_import_available: bool,
) -> CalendarSourceView {
    let (protocol, location) = match &source.kind {
        clipper_schedule::SourceKind::Ics { url } => ("ics", redact_url(url)),
    };
    CalendarSourceView {
        id: object_id.to_string(),
        name: source.name.clone(),
        protocol: protocol.to_string(),
        location,
        enabled: source.enabled,
        event_count,
        raw_import_file_id: source
            .active_import
            .as_ref()
            .map(|batch| batch.object_id.to_string()),
        raw_import_available,
    }
}

/// Strip everything after the host and path root.
///
/// A private iCalendar address is a bearer credential: anyone holding the URL
/// can read the calendar. The UI shows where a feed lives without showing how
/// to reach it.
fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => match parsed.host_str() {
            Some(host) => format!("{}://{host}/…", parsed.scheme()),
            None => format!("{}://…", parsed.scheme()),
        },
        Err(_) => "(unparseable URL)".to_string(),
    }
}

/// Resolve an IANA zone name, falling back to UTC.
///
/// Each shell reports its own zone and can get it wrong, such as a browser on
/// a device with a bad locale. Falling back beats refusing to render the
/// calendar.
pub fn zone_or_utc(name: &str) -> Tz {
    name.parse().unwrap_or(Tz::UTC)
}

/// The exact definition and override recorded when a timer started.
#[derive(Debug, Clone)]
pub struct RecordedPlan {
    pub item: ScheduleItem,
    pub override_data: Option<clipper_schedule::OccurrenceOverrideData>,
    pub context: clipper_schedule::PlannedRef,
}

#[cfg(test)]
mod tests {
    use clipper_schedule::{
        BlockDuration, Cadence, Frequency, Recurrence, ScheduleItemId, TimedStart,
    };

    use super::*;

    #[test]
    fn floating_occurrence_keys_round_trip_including_fractional_seconds() {
        for value in ["2026-09-08T07:00:00", "2026-09-08T07:00:00.123456"] {
            let id = clipper_schedule::RecurrenceId::Floating(value.parse().expect("datetime"));
            assert_eq!(parse_occurrence_key(&occurrence_key(&id)), Some(id));
        }
    }

    fn sample_item() -> ScheduleItem {
        ScheduleItem {
            id: ScheduleItemId::new(),
            title: "Gym".to_string(),
            span: ScheduleSpan::Timed {
                start: TimedStart::Floating(
                    chrono::NaiveDateTime::parse_from_str("20260610T070000", "%Y%m%dT%H%M%S")
                        .expect("valid"),
                ),
                duration: BlockDuration::from_minutes(45).expect("non-zero"),
            },
            recurrence: Recurrence::Every(Cadence::each(Frequency::Daily)),
            reference: None,
            alarm: None,
        }
    }

    #[test]
    fn a_record_round_trips_through_json() {
        let record = ScheduleRecord::Item(Box::new(sample_item()));
        let bytes = serde_json::to_vec(&record).expect("serialize");
        let back: ScheduleRecord = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(back.kind(), ScheduleRecordKind::Item);
        assert_eq!(
            back.as_item().expect("an item").title,
            record.as_item().expect("an item").title
        );
    }

    #[test]
    fn the_meta_reports_the_records_own_kind() {
        let record = ScheduleRecord::Item(Box::new(sample_item()));
        let meta = record.meta();
        assert_eq!(meta.record, ScheduleRecordKind::Item);
        assert_eq!(meta.version, SCHEDULE_PAYLOAD_VERSION);
    }

    /// A private calendar URL is a bearer credential. Anything that renders one
    /// can end up in a screenshot or a log.
    #[test]
    fn a_source_url_is_redacted_before_it_reaches_a_view() {
        let secret = "https://calendar.google.com/calendar/ical/abc123secret/basic.ics";
        assert_eq!(redact_url(secret), "https://calendar.google.com/…");
        assert!(!redact_url(secret).contains("abc123secret"));
        assert_eq!(redact_url("nonsense"), "(unparseable URL)");
    }

    #[test]
    fn an_unknown_zone_falls_back_rather_than_failing() {
        assert_eq!(zone_or_utc("Europe/Berlin"), Tz::Europe__Berlin);
        assert_eq!(zone_or_utc("Mars/Olympus_Mons"), Tz::UTC);
    }
}
