//! Sealing schedule records into objects, and rendering them for display.
//!
//! A schedule object mirrors clipboard: a small encrypted meta saying what the
//! payload is, plus one inline payload holding the record itself. Records are a
//! few hundred bytes, so the payload always travels inline and the object is
//! complete the moment `object_init` returns.
//!
//! The server sees an object of kind `schedule` and nothing else. Whether it
//! holds a plan, an override, or a log of time actually spent is inside the
//! ciphertext (`ScheduleRecordKind`), which is the point.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use clipper_app_types::{OccurrenceView, ScheduleItemView};
use clipper_core::{
    crypto,
    models::{
        ObjectEnvelopeBodyV1, ObjectPayloadId, SCHEDULE_PAYLOAD_VERSION, ScheduleMeta,
        ScheduleRecordKind,
    },
};
use clipper_schedule::{Occurrence, OccurrenceOrigin, ScheduleItem, ScheduleSpan};

/// A schedule record, in the form it is serialized into an object payload.
///
/// One enum rather than three payload shapes so the meta's discriminant and the
/// payload cannot disagree: deserializing checks the tag either way.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum ScheduleRecord {
    Item(Box<ScheduleItem>),
    Override(Box<clipper_schedule::OccurrenceOverride>),
    Actual(Box<clipper_schedule::ActualRecord>),
}

impl ScheduleRecord {
    pub fn kind(&self) -> ScheduleRecordKind {
        match self {
            Self::Item(_) => ScheduleRecordKind::Item,
            Self::Override(_) => ScheduleRecordKind::Override,
            Self::Actual(_) => ScheduleRecordKind::Actual,
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
            Self::Override(_) | Self::Actual(_) => None,
        }
    }
}

/// Encrypt a schedule object's metadata.
pub fn encrypt_schedule_meta(
    meta: &ScheduleMeta,
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let json = serde_json::to_vec(meta)
        .map_err(|e| crypto::CryptoError::Encrypt(format!("json: {}", e)))?;
    let aad = crypto::object_meta_aad_v1(envelope_body)?;
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, &json, &aad)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt a schedule object's metadata.
pub fn decrypt_schedule_meta(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
) -> Result<ScheduleMeta, crypto::CryptoError> {
    let aad = crypto::object_meta_aad_v1(envelope_body)?;
    let plaintext = crypto::decrypt(encryption_key, nonce, ciphertext, &aad)?;
    serde_json::from_slice(&plaintext)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("json: {}", e)))
}

/// Encrypt a schedule record into an object payload.
pub fn encrypt_schedule_payload(
    record: &ScheduleRecord,
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
    payload_id: ObjectPayloadId,
) -> Result<(Vec<u8>, Vec<u8>), crypto::CryptoError> {
    let json = serde_json::to_vec(record)
        .map_err(|e| crypto::CryptoError::Encrypt(format!("json: {}", e)))?;
    let aad = crypto::object_payload_aad_v1(envelope_body, payload_id)?;
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, &json, &aad)?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Decrypt a schedule record from an object payload.
pub fn decrypt_schedule_payload(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBodyV1,
    payload_id: ObjectPayloadId,
) -> Result<ScheduleRecord, crypto::CryptoError> {
    let aad = crypto::object_payload_aad_v1(envelope_body, payload_id)?;
    let plaintext = crypto::decrypt(encryption_key, nonce, ciphertext, &aad)?;
    serde_json::from_slice(&plaintext)
        .map_err(|e| crypto::CryptoError::Decrypt(format!("json: {}", e)))
}

/// Render a series for a list.
pub fn item_view(item: &ScheduleItem, created_at: &str) -> ScheduleItemView {
    ScheduleItemView {
        id: item.id.to_string(),
        title: item.title.clone(),
        recurrence: item.recurrence.summary(),
        time_summary: item.time_summary(),
        all_day: matches!(item.span, ScheduleSpan::AllDay { .. }),
        created_at: created_at.to_string(),
    }
}

/// Render one computed occurrence for a grid.
pub fn occurrence_view(occurrence: &Occurrence, title: &str, all_day: bool) -> OccurrenceView {
    OccurrenceView {
        item_id: occurrence.item.to_string(),
        title: title.to_string(),
        start: to_rfc3339(occurrence.span.start),
        end: to_rfc3339(occurrence.span.end),
        all_day,
        overridden: matches!(occurrence.origin, OccurrenceOrigin::Overridden(_)),
    }
}

fn to_rfc3339(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Resolve an IANA zone name, falling back to UTC.
///
/// A shell reports its own zone and can get it wrong (a browser on a device
/// with a bad locale, say). Falling back beats refusing to render a calendar.
pub fn zone_or_utc(name: &str) -> Tz {
    name.parse().unwrap_or(Tz::UTC)
}

#[cfg(test)]
mod tests {
    use clipper_schedule::{
        BlockDuration, Cadence, Frequency, Recurrence, ScheduleItemId, TimedStart,
    };

    use super::*;

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

    #[test]
    fn an_unknown_zone_falls_back_rather_than_failing() {
        assert_eq!(zone_or_utc("Europe/Berlin"), Tz::Europe__Berlin);
        assert_eq!(zone_or_utc("Mars/Olympus_Mons"), Tz::UTC);
    }
}
