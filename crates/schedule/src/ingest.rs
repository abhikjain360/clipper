//! Calendar sources, and the events pulled from them.
//!
//! Parsing lives here because it is pure. Fetching needs I/O, so it belongs to
//! whichever client holds the source.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    num::NonZeroU32,
};

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use chrono_tz::Tz;
use clipper_api_types::{DeviceId, ObjectId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    engine::ImportedRuleResolver,
    item::{OccurrenceOverrideData, OverrideChange, OverrideId, RecurrenceId, ScheduleItemId},
    recurrence::{Recurrence, RecurrenceError},
    time::{BlockDuration, ScheduleSpan, TimeError, TimedStart},
};

/// Bounds parser work even when `parse_ics` is called outside the HTTP
/// fetcher.
const MAX_ICS_BYTES: usize = 8 * 1024 * 1024;
const MAX_COMPONENTS: usize = 50_000;
const MAX_PROPERTIES: usize = 500_000;
/// Every component costs a BEGIN and an END line on top of its properties, so
/// a feed within both caps holds at most this many lines.
const MAX_CONTENT_LINES: usize = MAX_PROPERTIES + 2 * MAX_COMPONENTS;
const MAX_OVERRIDES_PER_EVENT: usize = 10_000;

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
/// Stored as an encrypted object. An iCalendar feed URL is the credential for
/// that feed, so it must never be server-visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "store::StoredSource", into = "store::StoredSource")]
pub struct CalendarSource {
    pub id: SourceId,
    /// What the user calls it: "Work", "Gmail", "Zoho".
    pub name: String,
    pub kind: SourceKind,
    /// Whether this client syncs it. Per-client, because each device picks the
    /// sources it is responsible for.
    pub enabled: bool,
    #[serde(default)]
    pub owner_email: Option<String>,
    #[serde(default = "alarms_on_by_default")]
    pub alarms_on: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device: Option<DeviceId>,
    pub active_import: Option<CalendarImport>,
    /// A staged batch to resume after an interrupted upload.
    #[serde(
        default,
        alias = "pending_import",
        deserialize_with = "pending_imports"
    )]
    pub pending_imports: Vec<CalendarImport>,
    /// Superseded batches awaiting irreversible cleanup.
    pub retired_imports: Vec<RetiredImport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retained_imports: Vec<CalendarImport>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub event_ids: BTreeMap<Uuid, ObjectId>,
    pub import_anchor: Option<ObjectId>,
    pub delta_state: bool,
    pub removing: bool,
    pub superseded: HashMap<ObjectId, ObjectId>,
    pub pending_retirements: HashSet<ObjectId>,
}

#[path = "import_store.rs"]
mod store;

/// One source fetch, stored once as an encrypted file, with its parsed event objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarImport {
    pub object_id: clipper_api_types::ObjectId,
    #[serde(deserialize_with = "crate::time::deserialize_date")]
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub events: Vec<clipper_api_types::ObjectId>,
    #[serde(default)]
    pub content_hash: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<ImportWindow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hashes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportWindow {
    #[serde(deserialize_with = "crate::time::deserialize_date")]
    pub start: chrono::DateTime<chrono::Utc>,
    #[serde(deserialize_with = "crate::time::deserialize_date")]
    pub end: chrono::DateTime<chrono::Utc>,
}

impl ImportWindow {
    pub fn around(now: chrono::DateTime<chrono::Utc>) -> Self {
        Self {
            start: now - chrono::TimeDelta::days(14),
            end: now + chrono::TimeDelta::days(90),
        }
    }
}

fn alarms_on_by_default() -> bool {
    true
}

fn pending_imports<'de, D>(deserializer: D) -> Result<Vec<CalendarImport>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Many(Vec<CalendarImport>),
        One(Option<CalendarImport>),
    }
    Ok(match Stored::deserialize(deserializer)? {
        Stored::Many(batches) => batches,
        Stored::One(batch) => batch.into_iter().collect(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredImport {
    pub object_id: clipper_api_types::ObjectId,
    pub events: Vec<clipper_api_types::ObjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<Box<CalendarImport>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<ObjectId>,
}

impl From<CalendarImport> for RetiredImport {
    fn from(batch: CalendarImport) -> Self {
        Self {
            superseded_by: None,
            delta: batch.window.as_ref().map(|_| Box::new(batch.clone())),
            object_id: batch.object_id,
            events: batch.events,
        }
    }
}

impl CalendarSource {
    pub fn contains_event(&self, object_id: &str, event: &IngestedEvent) -> bool {
        self.id == event.source
            && event.has_valid_recurrence()
            && (self.imports().any(|batch| {
                (event.snapshot() == Some(batch.object_id)
                    || (batch.window.is_none() && event.import == Some(batch.object_id)))
                    && batch.events.iter().any(|id| id.to_string() == object_id)
            }) || self.pending_imports.iter().any(|batch| {
                batch.window.is_some()
                    && event.snapshot() == Some(batch.object_id)
                    && batch.events.iter().any(|id| id.to_string() == object_id)
            }))
    }

    pub fn can_cleanup(&self, batch: &RetiredImport) -> bool {
        self.removing
            || (self.active_import.is_some()
                && batch
                    .superseded_by
                    .is_some_and(|winner| self.superseded.get(&batch.object_id) == Some(&winner)))
    }

    pub fn imports(&self) -> impl Iterator<Item = &CalendarImport> {
        self.active_import
            .iter()
            .chain(self.retained_imports.iter())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum SourceKind {
    Ics { url: String },
}

/// An event as the provider describes it. Read-only in Clipper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestedEvent {
    pub id: Uuid,
    pub source: SourceId,
    pub import: Option<clipper_api_types::ObjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_fetched_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_import: Option<ObjectId>,
    /// The provider's own identifier. Stable across edits, and the same in
    /// every calendar that carries the meeting.
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
    #[serde(default)]
    pub organizer: Option<String>,
    #[serde(default)]
    pub attendance: Attendance,
    #[serde(default)]
    pub alarm_seconds_before: Vec<u64>,
    #[serde(default)]
    pub alarm_overrides: Vec<AlarmOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlarmOverride {
    pub recurrence_id: RecurrenceId,
    pub status: IngestedStatus,
    pub all_day: bool,
    pub organizer: Option<String>,
    pub attendance: Option<Attendance>,
    pub seconds_before: Vec<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendance {
    pub has_attendees: bool,
    pub owner_partstat: Option<String>,
}

impl IngestedEvent {
    pub fn has_valid_recurrence(&self) -> bool {
        match &self.recurrence {
            Recurrence::Imported { import, uid } => {
                *uid == self.uid
                    && (Some(*import) == self.snapshot() || Some(*import) == self.import)
            }
            Recurrence::Once | Recurrence::Every(_) => self.import.is_some(),
        }
    }

    pub fn snapshot(&self) -> Option<ObjectId> {
        self.raw_import.or(self.import)
    }

    pub fn resolved_recurrence(&self) -> Recurrence {
        let mut recurrence = self.recurrence.clone();
        if let Recurrence::Imported { import, .. } = &mut recurrence
            && let Some(raw) = self.snapshot()
        {
            *import = raw;
        }
        recurrence
    }

    pub fn window_check(
        &self,
        window: &ImportWindow,
    ) -> Result<Option<bool>, crate::engine::EngineError> {
        if !matches!(self.recurrence, Recurrence::Imported { .. }) {
            return self
                .overlaps(window, &crate::RecurrenceEngine::new())
                .map(Some);
        }
        let mut once = self.clone();
        once.recurrence = Recurrence::Once;
        if once.overlaps(window, &crate::RecurrenceEngine::new())? {
            return Ok(Some(true));
        }
        if self.overrides.is_empty() && self.span.resolve(Tz::UTC)?.start() >= window.end {
            return Ok(Some(false));
        }
        Ok(None)
    }

    pub fn overlaps(
        &self,
        window: &ImportWindow,
        engine: &crate::RecurrenceEngine,
    ) -> Result<bool, crate::engine::EngineError> {
        let item = crate::ScheduleItem {
            id: ScheduleItemId(self.id),
            title: self.title.clone(),
            span: self.span.clone(),
            recurrence: self.resolved_recurrence(),
            reference: None,
            alarm: None,
            break_reminders: false,
        };
        let expansion = crate::engine::Expansion {
            window: crate::TimeRange::new(window.start, window.end)?,
            observer: Tz::UTC,
        };
        let overrides: Vec<_> = self
            .overrides
            .iter()
            .filter(|entry| !matches!(entry.change, OverrideChange::Cancelled))
            .cloned()
            .collect();
        Ok(!engine
            .overlapping_occurrences(&item, &overrides, &expansion)?
            .is_empty())
    }

    pub fn rings(&self, owner: Option<&str>) -> bool {
        invitation_rings(
            self.status,
            matches!(self.span, ScheduleSpan::AllDay { .. }),
            self.organizer.as_deref(),
            &self.attendance,
            owner,
        )
    }

    pub fn alarm_offsets_at(&self, recurrence_id: RecurrenceId, owner: Option<&str>) -> &[u64] {
        if let Some(entry) = self
            .alarm_overrides
            .iter()
            .find(|entry| entry.recurrence_id == recurrence_id)
        {
            if invitation_rings(
                entry.status,
                entry.all_day,
                entry.organizer.as_deref(),
                entry.attendance.as_ref().unwrap_or(&self.attendance),
                owner,
            ) {
                &entry.seconds_before
            } else {
                &[]
            }
        } else if self.rings(owner) {
            &self.alarm_seconds_before
        } else {
            &[]
        }
    }

    pub fn alarm_offsets(&self) -> Vec<u64> {
        let mut offsets = self.alarm_seconds_before.clone();
        offsets.extend(
            self.alarm_overrides
                .iter()
                .flat_map(|entry| &entry.seconds_before),
        );
        offsets.sort_unstable();
        offsets.dedup();
        offsets
    }
}

fn invitation_rings(
    status: IngestedStatus,
    all_day: bool,
    organizer: Option<&str>,
    attendance: &Attendance,
    owner: Option<&str>,
) -> bool {
    if status == IngestedStatus::Cancelled || all_day {
        return false;
    }
    if !attendance.has_attendees {
        return true;
    }
    match owner {
        Some(owner) => {
            organizer.is_some_and(|email| email.eq_ignore_ascii_case(owner))
                || attendance.owner_partstat.as_deref() == Some("ACCEPTED")
        }
        None => status != IngestedStatus::Tentative,
    }
}

impl IngestedEvent {
    /// True when the provenance and the recurrence reference name the same
    /// source event.
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
        // A fixed namespace, so two clients ingesting the same feed derive
        // the same ids.
        const NAMESPACE: Uuid = Uuid::from_u128(0x9f2c_4c6e_5d17_4c9b_a1e8_3f0b_7d24_88a1);
        Uuid::new_v5(&NAMESPACE, format!("{source}:{uid}").as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestedStatus {
    Confirmed,
    Tentative,
    /// Cancelled upstream. Kept as a tombstone rather than erased, so time
    /// already logged against the meeting survives.
    Cancelled,
}

/// What one pass over a feed produced.
///
/// Skipped events are reported, never dropped silently, so a caller can see
/// which entries did not parse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestOutcome {
    pub events: Vec<IngestedEvent>,
    pub skipped: Vec<SkippedEvent>,
    pub rules: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEvent {
    pub uid: Option<String>,
    pub reason: String,
}

/// Parse an iCalendar feed into events.
///
/// `import` names the immutable raw file this parse came from. An opaque
/// recurrence rule stores only that snapshot id and its event UID.
pub fn parse_ics(
    text: &str,
    source: SourceId,
    import: ObjectId,
) -> Result<IngestOutcome, IngestError> {
    parse_ics_for_owner(text, source, import, None)
}

pub fn parse_ics_for_owner(
    text: &str,
    source: SourceId,
    import: ObjectId,
    owner: Option<&str>,
) -> Result<IngestOutcome, IngestError> {
    let calendar = parse_calendar(text)?;
    let resolver = calendar.build_tz_resolver();

    let mut outcome = IngestOutcome::default();
    let (masters, mut overrides, missing_override_uids) =
        partition_masters_and_overrides(&calendar);
    for _ in 0..missing_override_uids {
        outcome.skipped.push(SkippedEvent {
            uid: None,
            reason: IngestError::MissingUid.to_string(),
        });
    }

    for (component, uid) in masters {
        let matching = uid
            .as_ref()
            .and_then(|uid| overrides.remove(uid))
            .unwrap_or_default();
        match event_from_component(
            component, &matching, source, import, &resolver, &calendar, owner,
        ) {
            Ok(event) => {
                if matches!(event.recurrence, Recurrence::Imported { .. })
                    && let Some(rule) = rrule_text(component)?
                {
                    outcome.rules.push((event.uid.clone(), rule));
                }
                outcome.events.push(event);
            }
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
    outcome.rules.sort();
    Ok(outcome)
}

/// Reads the validated opaque recurrence rules out of one import snapshot.
///
/// Merge the results for several snapshots to build one [`crate::RecurrenceEngine`]
/// that expands every event in them.
pub fn parse_imported_recurrence_rules(
    text: &str,
    import: ObjectId,
) -> Result<ImportedRuleResolver, IngestError> {
    let calendar = parse_calendar(text)?;
    let mut resolver = ImportedRuleResolver::new();
    let mut master_uids = HashSet::new();
    let (masters, _, _) = partition_masters_and_overrides(&calendar);
    for (component, _) in masters {
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

/// Splits VEVENT components into masters and RECURRENCE-ID overrides.
/// Returns the masters with their UIDs, the overrides by UID, and the
/// count of overrides without a UID.
#[allow(clippy::type_complexity)]
fn partition_masters_and_overrides(
    calendar: &calcard::icalendar::ICalendar,
) -> (
    Vec<(&calcard::icalendar::ICalendarComponent, Option<String>)>,
    HashMap<String, Vec<&calcard::icalendar::ICalendarComponent>>,
    usize,
) {
    use calcard::icalendar::ICalendarComponentType;

    let mut masters = Vec::new();
    let mut overrides: HashMap<String, Vec<&calcard::icalendar::ICalendarComponent>> =
        HashMap::new();
    let mut missing_override_uids = 0;
    for component in &calendar.components {
        if component.component_type != ICalendarComponentType::VEvent {
            continue;
        }
        let uid = text_property(component, "UID");
        if property(component, "RECURRENCE-ID").is_some() {
            match uid {
                Some(uid) => overrides.entry(uid).or_default().push(component),
                None => missing_override_uids += 1,
            }
        } else {
            masters.push((component, uid));
        }
    }
    (masters, overrides, missing_override_uids)
}

fn parse_calendar(text: &str) -> Result<calcard::icalendar::ICalendar, IngestError> {
    use calcard::icalendar::ICalendar;

    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.len() > MAX_ICS_BYTES {
        return Err(IngestError::LimitExceeded("calendar exceeds 8 MiB"));
    }
    // Counted before parsing. The component and property caps below only apply
    // once the parser has allocated the whole tree, which for 8 MiB of
    // two-byte properties is hundreds of megabytes.
    if text.lines().count() > MAX_CONTENT_LINES {
        return Err(IngestError::LimitExceeded("too many calendar lines"));
    }
    validate_calendar_envelope(text)?;
    validate_value_count(text)?;
    validate_rrule_numbers(text)?;
    let mut parser = calcard::Parser::new(text);
    let mut calendar = ICalendar::default();
    loop {
        match parser.entry() {
            calcard::Entry::ICalendar(mut block) => {
                if calendar.components.len() + block.components.len() > MAX_COMPONENTS {
                    return Err(IngestError::LimitExceeded("too many calendar components"));
                }
                let offset = calendar.components.len() as u32;
                for component in &mut block.components {
                    for id in &mut component.component_ids {
                        *id += offset;
                    }
                }
                calendar.components.extend(block.components);
            }
            calcard::Entry::Eof => break,
            error => return Err(IngestError::Malformed(format!("{error:?}"))),
        }
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
    let mut stack = Vec::new();
    let mut calendars = 0;
    let mut has_version = false;
    for line in unfold_content_lines(text) {
        if line.trim().is_empty() {
            continue;
        }
        let raw_name = &line[..line.find([';', ':', ',', '=']).unwrap_or(line.len())];
        let name = name_characters(raw_name);
        if name.eq_ignore_ascii_case("BEGIN") || name.eq_ignore_ascii_case("END") {
            let Some((key, component)) = line.split_once(':') else {
                return Err(IngestError::Malformed("invalid component boundary".into()));
            };
            if key != raw_name || name != raw_name || component != name_characters(component) {
                return Err(IngestError::Malformed("invalid component boundary".into()));
            }
            if name.eq_ignore_ascii_case("BEGIN") {
                if stack.is_empty() {
                    if !component.eq_ignore_ascii_case("VCALENDAR") {
                        return Err(IngestError::Malformed("expected VCALENDAR".into()));
                    }
                    calendars += 1;
                    has_version = false;
                }
                stack.push(component.to_ascii_uppercase());
            } else if !stack
                .pop()
                .is_some_and(|expected| expected.eq_ignore_ascii_case(component))
            {
                return Err(IngestError::Malformed("mismatched component end".into()));
            } else if stack.is_empty() && !has_version {
                return Err(IngestError::Malformed("expected VERSION:2.0".into()));
            }
        } else if stack.is_empty() {
            return Err(IngestError::Malformed("content outside VCALENDAR".into()));
        } else if stack.len() == 1 && line.eq_ignore_ascii_case("VERSION:2.0") {
            has_version = true;
        }
    }
    if calendars == 0 || !stack.is_empty() {
        return Err(IngestError::Malformed(
            "expected a complete VERSION:2.0 VCALENDAR".to_string(),
        ));
    }
    Ok(())
}

fn validate_value_count(text: &str) -> Result<(), IngestError> {
    let mut values = 0usize;
    for line in unfold_content_lines(text) {
        let mut quoted_parameter = false;
        let mut in_value = false;
        let mut escaped = false;
        for byte in line.bytes() {
            if escaped {
                escaped = false;
                continue;
            }
            match byte {
                b'\\' => escaped = true,
                b'"' if !in_value => quoted_parameter = !quoted_parameter,
                b':' if !quoted_parameter && !in_value => {
                    in_value = true;
                    values += 1;
                }
                b',' | b';' if !quoted_parameter => values += 1,
                _ => {}
            }
            if values > MAX_PROPERTIES {
                return Err(IngestError::LimitExceeded("too many calendar values"));
            }
        }
    }
    Ok(())
}

/// Finds the property value separator: the first `:` outside a double-quoted
/// parameter value. A line with an unterminated quote has no value.
fn value_separator(line: &str) -> Option<usize> {
    let mut in_quotes = false;
    for (index, byte) in line.bytes().enumerate() {
        if byte == b'"' {
            in_quotes = !in_quotes;
        } else if byte == b':' && !in_quotes {
            return Some(index);
        }
    }
    None
}

/// Rejects RRULE values calcard would silently narrow.
/// calcard reads every numeric clause through a wider integer and casts it
/// down: INTERVAL to u16 and COUNT to u32 (dropping zero, stripping the sign),
/// BYSECOND, BYMINUTE and BYHOUR to u8, BYMONTHDAY and BYWEEKNO to i8,
/// BYYEARDAY and the BYDAY ordinal to i16, BYSETPOS to i32, and BYMONTH to a
/// saturating i8 with an optional leap suffix. An overflowing value wraps or
/// saturates into a different valid rule instead of failing. A numeric WKST
/// needs no check: the parser only reads weekday names, so one already fails.
/// Check the raw text before parsing, so a value the parser would not preserve
/// exactly is rejected rather than rewritten.
fn validate_rrule_numbers(text: &str) -> Result<(), IngestError> {
    for line in unfold_content_lines(text) {
        let raw_name = &line[..line.find([';', ':', ',', '=']).unwrap_or(line.len())];
        let name = name_characters(raw_name);
        if !name.eq_ignore_ascii_case("RRULE") {
            continue;
        }
        if name != raw_name || line.contains('\\') {
            return Err(IngestError::Malformed(
                "RRULE line must have a plain name and no escapes".to_string(),
            ));
        }
        let Some(colon) = value_separator(&line) else {
            return Err(IngestError::Malformed(
                "RRULE line has no value".to_string(),
            ));
        };
        let value = &line[colon + 1..];
        let mut interval_seen = false;
        let mut count_seen = false;
        for part in value.split(';') {
            let Some((raw_key, raw_value)) = part.split_once('=') else {
                continue;
            };
            if name_characters(raw_key) != raw_key.trim() {
                return Err(IngestError::Malformed(format!(
                    "RRULE has a malformed key {raw_key}"
                )));
            }
            let key = raw_key.trim().to_ascii_uppercase();
            let number = raw_value.trim();
            let valid = match key.as_str() {
                "INTERVAL" | "COUNT" => {
                    let seen = if key == "INTERVAL" {
                        &mut interval_seen
                    } else {
                        &mut count_seen
                    };
                    if *seen {
                        return Err(IngestError::Malformed(format!("RRULE has duplicate {key}")));
                    }
                    *seen = true;
                    let all_zero = !number.is_empty() && number.bytes().all(|byte| byte == b'0');
                    let all_digits =
                        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit());
                    let fits = if key == "INTERVAL" {
                        number.parse::<u16>().is_ok()
                    } else {
                        number.parse::<u32>().is_ok()
                    };
                    all_digits && !all_zero && fits
                }
                "BYSECOND" | "BYMINUTE" | "BYHOUR" => number
                    .split(',')
                    .all(|item| !item.is_empty() && item.parse::<u8>().is_ok() && is_digits(item)),
                "BYMONTHDAY" | "BYWEEKNO" => number
                    .split(',')
                    .all(|item| item.parse::<i8>().is_ok() && is_signed_digits(item)),
                "BYYEARDAY" => number
                    .split(',')
                    .all(|item| item.parse::<i16>().is_ok() && is_signed_digits(item)),
                "BYSETPOS" => number
                    .split(',')
                    .all(|item| item.parse::<i32>().is_ok() && is_signed_digits(item)),
                "BYMONTH" => number.split(',').all(|item| {
                    let digits = item.strip_suffix(['L', 'l']).unwrap_or(item);
                    !digits.is_empty() && is_digits(digits) && digits.parse::<i8>().is_ok()
                }),
                "BYDAY" => number
                    .split(',')
                    .all(|item| !item.is_empty() && byday_ordinal_fits(item)),
                _ => true,
            };
            if !valid {
                return Err(IngestError::Malformed(format!(
                    "RRULE has invalid {key}={raw_value}"
                )));
            }
        }
    }
    Ok(())
}

fn name_characters(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .collect()
}

/// Plain digits, the shape the parser preserves unchanged for an unsigned
/// value. A sign would be stripped or normalized away.
fn is_digits(text: &str) -> bool {
    text.bytes().all(|byte| byte.is_ascii_digit())
}

/// An optional sign followed by digits. The parser reads a leading `+` or `-`
/// and keeps the value, so both are part of the shape.
fn is_signed_digits(text: &str) -> bool {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    !digits.is_empty() && is_digits(digits)
}

/// Whether the ordinal prefix of a BYDAY token survives the parser's i16.
/// A bare weekday has no ordinal and is left for the parser to accept or
/// reject on the weekday itself.
fn byday_ordinal_fits(token: &str) -> bool {
    let (signed, unsigned) = match token.strip_prefix(['+', '-']) {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    let digits = unsigned
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits == 0 {
        // A sign with no digits would be dropped by the parser.
        return !signed;
    }
    unsigned[..digits].parse::<i16>().is_ok()
}

/// Joins folded content lines. A line starting with a space or tab
/// continues the previous line. Handles CRLF and LF.
fn unfold_content_lines(text: &str) -> Vec<String> {
    let mut unfolded: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if (line.starts_with(' ') || line.starts_with('\t')) && !unfolded.is_empty() {
            let continued = &line[1..];
            unfolded
                .last_mut()
                .expect("checked non-empty")
                .push_str(continued);
        } else {
            unfolded.push(line.to_string());
        }
    }
    unfolded
}

fn event_from_component(
    component: &calcard::icalendar::ICalendarComponent,
    overrides: &[&calcard::icalendar::ICalendarComponent],
    source: SourceId,
    import: ObjectId,
    resolver: &calcard::icalendar::timezone::TzResolver<&str>,
    calendar: &calcard::icalendar::ICalendar,
    owner: Option<&str>,
) -> Result<IngestedEvent, IngestError> {
    let uid = text_property(component, "UID").ok_or(IngestError::MissingUid)?;
    let id = IngestedEvent::derive_id(source, &uid);
    let start =
        date_time_property(component, "DTSTART", resolver)?.ok_or(IngestError::MissingStart)?;
    let end = date_time_property(component, "DTEND", resolver)?;
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
    let provider_overrides = overrides;
    let overrides = recurrence_overrides(component, provider_overrides, id, &span, resolver)?;
    let status = event_status(component).unwrap_or(IngestedStatus::Confirmed);
    let organizer = property(component, "ORGANIZER").and_then(email_address);
    let attendance = attendance(component, owner);
    let alarm_seconds_before = alarm_offsets(component, calendar);
    let mut alarm_overrides = std::collections::BTreeMap::new();
    for entry in provider_overrides {
        let time = date_time_property(entry, "RECURRENCE-ID", resolver)?
            .ok_or(IngestError::MissingRecurrenceId)?;
        let recurrence_id = recurrence_id_for(&span, &time)?;
        let all_day = overrides
            .iter()
            .find(|entry| entry.recurrence_id == recurrence_id)
            .map_or(
                matches!(span, ScheduleSpan::AllDay { .. }),
                |entry| match &entry.change {
                    OverrideChange::Rescheduled(span) => {
                        matches!(span, ScheduleSpan::AllDay { .. })
                    }
                    OverrideChange::Cancelled => false,
                },
            );
        alarm_overrides.insert(
            recurrence_id,
            AlarmOverride {
                recurrence_id,
                status: event_status(entry).unwrap_or(status),
                all_day,
                organizer: if property(entry, "ORGANIZER").is_some() {
                    property(entry, "ORGANIZER").and_then(email_address)
                } else {
                    organizer.clone()
                },
                attendance: property(entry, "ATTENDEE").map(|_| self::attendance(entry, owner)),
                seconds_before: if entry.component_ids.iter().any(|id| {
                    calendar.components.get(*id as usize).is_some_and(|entry| {
                        entry.component_type == calcard::icalendar::ICalendarComponentType::VAlarm
                    })
                }) {
                    alarm_offsets(entry, calendar)
                } else {
                    alarm_seconds_before.clone()
                },
            },
        );
    }
    let alarm_overrides = alarm_overrides.into_values().collect();

    Ok(IngestedEvent {
        id,
        source,
        import: Some(import),
        import_fetched_at: None,
        raw_import: None,
        title: text_property(component, "SUMMARY").unwrap_or_else(|| "(no title)".to_string()),
        description: text_property(component, "DESCRIPTION"),
        span,
        recurrence,
        overrides,
        status,
        uid,
        organizer,
        attendance,
        alarm_seconds_before,
        alarm_overrides,
    })
}

fn event_status(component: &calcard::icalendar::ICalendarComponent) -> Option<IngestedStatus> {
    text_property(component, "STATUS").map(|status| match status.as_str() {
        "CANCELLED" => IngestedStatus::Cancelled,
        "TENTATIVE" => IngestedStatus::Tentative,
        _ => IngestedStatus::Confirmed,
    })
}

fn email_address(entry: &calcard::icalendar::ICalendarEntry) -> Option<String> {
    let value = entry.values.first()?.as_text()?;
    let email = value.get(7..).filter(|_| {
        value
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("mailto:"))
    })?;
    Some(email.trim().to_ascii_lowercase())
}

fn attendance(
    component: &calcard::icalendar::ICalendarComponent,
    owner: Option<&str>,
) -> Attendance {
    use calcard::icalendar::{ICalendarParameterName, ICalendarParameterValue};

    let mut attendance = Attendance {
        has_attendees: false,
        owner_partstat: None,
    };
    for entry in component
        .entries
        .iter()
        .filter(|entry| entry.name.as_str().eq_ignore_ascii_case("ATTENDEE"))
    {
        attendance.has_attendees = true;
        if owner.is_some_and(|owner| {
            email_address(entry).is_some_and(|email| email.eq_ignore_ascii_case(owner))
        }) {
            let partstat = entry
                .params
                .iter()
                .filter(|param| param.name == ICalendarParameterName::Partstat)
                .map(|param| &param.value)
                .find_map(|value| match value {
                    ICalendarParameterValue::Partstat(status) => {
                        use calcard::common::IanaString;
                        Some(status.as_str().to_string())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| "NEEDS-ACTION".into());
            if attendance.owner_partstat.as_deref() != Some("ACCEPTED") {
                attendance.owner_partstat = Some(partstat);
            }
        }
    }
    attendance
}

fn alarm_offsets(
    component: &calcard::icalendar::ICalendarComponent,
    calendar: &calcard::icalendar::ICalendar,
) -> Vec<u64> {
    use calcard::icalendar::{
        ICalendarAction, ICalendarComponentType, ICalendarParameterValue, ICalendarRelated,
        ICalendarValue,
    };

    let mut offsets: Vec<_> = component
        .component_ids
        .iter()
        .filter_map(|id| calendar.components.get(*id as usize))
        .filter(|alarm| alarm.component_type == ICalendarComponentType::VAlarm)
        .filter(|alarm| {
            matches!(
                property(alarm, "ACTION").and_then(|entry| entry.values.first()),
                Some(ICalendarValue::Action(
                    ICalendarAction::Display | ICalendarAction::Audio
                ))
            )
        })
        .filter_map(|alarm| {
            let trigger = property(alarm, "TRIGGER")?;
            if trigger
                .params
                .iter()
                .map(|param| &param.value)
                .any(|value| {
                    matches!(
                        value,
                        ICalendarParameterValue::Related(ICalendarRelated::End)
                    )
                })
            {
                return None;
            }
            let ICalendarValue::Duration(duration) = trigger.values.first()? else {
                return None;
            };
            let seconds = u64::from(duration.weeks) * 604800
                + u64::from(duration.days) * 86400
                + u64::from(duration.hours) * 3600
                + u64::from(duration.minutes) * 60
                + u64::from(duration.seconds);
            (duration.neg || seconds == 0).then_some(seconds)
        })
        .collect();
    offsets.sort_unstable();
    offsets.dedup();
    if offsets.is_empty() {
        offsets.push(300);
    }
    offsets
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
        // RFC 5545 makes a timed VEVENT with no DTEND and no DURATION
        // instantaneous. An instant cannot be drawn, so it gets the shortest
        // block the grid can show.
        None => match inherited {
            Some(ScheduleSpan::Timed { duration, .. }) => duration.minutes(),
            _ => 5,
        },
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
    resolver: &calcard::icalendar::timezone::TzResolver<&str>,
) -> Result<Vec<OccurrenceOverrideData>, IngestError> {
    use std::collections::BTreeMap;

    // Counted before building FeedTimes, so one huge EXDATE line cannot
    // allocate hundreds of thousands of overrides first.
    override_count_within_limit(
        recurrence_values_count(master, "RDATE"),
        overrides.len(),
        recurrence_values_count(master, "EXDATE"),
    )?;

    let item = ScheduleItemId(event_id);
    let mut by_recurrence_id = BTreeMap::new();

    for added in recurrence_times(master, "RDATE", resolver)? {
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
        let recurrence_time = feed_time_from_entry(recurrence_entry, resolver)?;
        let recurrence_id = recurrence_id_for(master_span, &recurrence_time)?;
        let change = if text_property(override_data, "STATUS").as_deref() == Some("CANCELLED") {
            OverrideChange::Cancelled
        } else {
            let start = date_time_property(override_data, "DTSTART", resolver)?
                .ok_or(IngestError::MissingStart)?;
            let end = date_time_property(override_data, "DTEND", resolver)?;
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

    // RFC 5545 gives EXDATE precedence over inclusion dates, so apply it last.
    // A duplicated RDATE, or a detached component at the same recurrence
    // position, then cannot bring an excluded date back.
    for excluded in recurrence_times(master, "EXDATE", resolver)? {
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

/// Accepts an event whose RDATE values, detached instances and EXDATE values
/// together stay within [`MAX_OVERRIDES_PER_EVENT`]. A sum that overflows is
/// over the cap.
fn override_count_within_limit(
    rdate: usize,
    detached: usize,
    exdate: usize,
) -> Result<(), IngestError> {
    let within = rdate
        .checked_add(detached)
        .and_then(|total| total.checked_add(exdate))
        .is_some_and(|total| total <= MAX_OVERRIDES_PER_EVENT);
    if within {
        Ok(())
    } else {
        Err(IngestError::LimitExceeded(
            "too many recurrence overrides for one event",
        ))
    }
}

/// Counts comma-separated values without building FeedTimes.
fn recurrence_values_count(
    component: &calcard::icalendar::ICalendarComponent,
    name: &str,
) -> usize {
    component
        .entries
        .iter()
        .filter(|entry| entry.name.as_str().eq_ignore_ascii_case(name))
        .fold(0usize, |total, entry| {
            total.saturating_add(entry.values.len())
        })
}

fn recurrence_times(
    component: &calcard::icalendar::ICalendarComponent,
    name: &str,
    resolver: &calcard::icalendar::timezone::TzResolver<&str>,
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
                    times.push(feed_time_from_partial(entry, partial, resolver)?);
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
        // calcard prints a parsed rule back as RFC 5545 text, which is the
        // form the expansion engine takes.
        Some(ICalendarValue::RecurrenceRule(rule)) => Ok(Some(rule.to_string())),
        Some(ICalendarValue::Text(text)) => Ok(Some(text.clone())),
        _ => Err(IngestError::AmbiguousRecurrenceRule),
    }
}

fn date_time_property(
    component: &calcard::icalendar::ICalendarComponent,
    name: &str,
    resolver: &calcard::icalendar::timezone::TzResolver<&str>,
) -> Result<Option<FeedTime>, IngestError> {
    let Some(entry) = property(component, name) else {
        return Ok(None);
    };
    feed_time_from_entry(entry, resolver).map(Some)
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
    resolver: &calcard::icalendar::timezone::TzResolver<&str>,
) -> Result<FeedTime, IngestError> {
    use calcard::icalendar::ICalendarValue;

    let Some(ICalendarValue::PartialDateTime(partial)) = entry.values.first() else {
        return Err(IngestError::InvalidDateTime);
    };
    feed_time_from_partial(entry, partial, resolver)
}

fn feed_time_from_partial(
    entry: &calcard::icalendar::ICalendarEntry,
    partial: &calcard::common::PartialDateTime,
    resolver: &calcard::icalendar::timezone::TzResolver<&str>,
) -> Result<FeedTime, IngestError> {
    use calcard::icalendar::{ICalendarParameterName, ICalendarParameterValue};

    let date = NaiveDate::from_ymd_opt(
        i32::from(partial.year.ok_or(IngestError::InvalidDateTime)?),
        u32::from(partial.month.ok_or(IngestError::InvalidDateTime)?),
        u32::from(partial.day.ok_or(IngestError::InvalidDateTime)?),
    )
    .ok_or(IngestError::InvalidDateTime)?;
    crate::time::validate_date(&date)?;
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
            ICalendarParameterValue::Text(name) => Some(match name.parse::<Tz>() {
                Ok(zone) => zone,
                Err(_) if name == "UTC-11" => Tz::Etc__GMTPlus11,
                Err(_) if name == "UTC-09" => Tz::Etc__GMTPlus9,
                Err(_) if name == "UTC-08" => Tz::Etc__GMTPlus8,
                Err(_) if name == "UTC-02" => Tz::Etc__GMTPlus2,
                Err(_) if name == "UTC+12" => Tz::Etc__GMTMinus12,
                Err(_) if name == "UTC+13" => Tz::Etc__GMTMinus13,
                Err(_) => resolver
                    .resolve(name)
                    .and_then(|resolved| match resolved {
                        calcard::common::timezone::Tz::Tz(zone)
                            if !zone.name().starts_with("Etc/") =>
                        {
                            Some(zone)
                        }
                        _ => None,
                    })
                    .ok_or_else(|| IngestError::UnknownTimeZone(name.clone()))?,
            }),
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
        // RFC 5545 DATE-TIME allows UTC (`Z`) or a TZID, not a numeric UTC
        // offset. There is no fixed-offset zone in the domain model, so
        // reading one as UTC would move the event.
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
