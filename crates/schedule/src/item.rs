//! The stored schedule entities.
//!
//! Three record types: a series definition, an override for one occurrence
//! that differs from it, and an actual for what happened. They stay separate
//! because they have different writers and different lifetimes. An actual
//! outlives the meeting it was logged against.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;
use clipper_api_types::ObjectId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    recurrence::Recurrence,
    time::{ScheduleSpan, TimeRange},
};

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id_type!(
    /// Identifies a series definition.
    ScheduleItemId
);
id_type!(
    /// Identifies a single-occurrence override.
    OverrideId
);
id_type!(
    /// Identifies a record of time actually spent.
    ActualId
);

/// A planned block, and the rule for how it repeats.
///
/// One record per series, not per occurrence. A daily alarm for a year is this
/// struct once, not 365 times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleItem {
    pub id: ScheduleItemId,
    pub title: String,
    /// The span of the first occurrence. Later occurrences take their start
    /// from the recurrence rule and their length from here.
    pub span: ScheduleSpan,
    pub recurrence: Recurrence,
    /// The Clipper object this block is time for. Absent means bare labelled
    /// time, so the schedule works without any task subsystem.
    #[serde(default)]
    pub reference: Option<ObjectId>,
    /// When this block rings. Absent means silent.
    #[serde(default)]
    pub alarm: Option<crate::alarm::AlarmPolicy>,
}

impl ScheduleItem {
    /// Whether overrides written against this definition still apply to
    /// `other` unchanged. A title or alarm edit leaves the occurrence structure
    /// intact. A span or recurrence edit does not, and needs a decision from
    /// the caller.
    pub fn overrides_compatible_with(&self, other: &Self) -> bool {
        self.id == other.id && self.span == other.span && self.recurrence == other.recurrence
    }
}

/// The exact immutable envelope accepted for an encrypted schedule object.
/// `body_hash` separates two signed replacements that share a revision number.
/// It hashes the envelope body, not the decrypted schedule data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectRevisionRef {
    pub object_id: ObjectId,
    pub revision: u64,
    pub body_hash: [u8; 32],
}

/// An override stored on its own, anchored to the definition it was written
/// against. Callers check compatibility before applying it to a newer
/// definition, so the recurrence engine only ever receives checked overrides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrenceOverride {
    pub base: ObjectRevisionRef,
    pub override_data: OccurrenceOverrideData,
}

/// Which occurrence of a series something refers to.
///
/// RFC 5545's `RECURRENCE-ID`, not a bare instant. A floating series is
/// identified by wall-clock time, which holds still when the observer moves. A
/// zoned series is identified by its instant. With an instant for both, an
/// override recorded in Berlin would not match the same occurrence expanded in
/// Tokyo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "at", rename_all = "snake_case")]
pub enum RecurrenceId {
    Floating(NaiveDateTime),
    Instant(DateTime<Utc>),
    Date(NaiveDate),
}

/// One occurrence that differs from its series.
///
/// This is what the engine expands against. An imported calendar object embeds
/// it and it shares that object's revision. A locally authored override is
/// stored on its own as [`OccurrenceOverride`], which adds a base revision.
///
/// Only a differing occurrence gets one. The other 364 days of the year have
/// no record at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrenceOverrideData {
    pub id: OverrideId,
    pub item: ScheduleItemId,
    /// The occurrence this replaces, named by where the rule put it. Moving
    /// the occurrence does not change it, so the two still match up.
    pub recurrence_id: RecurrenceId,
    pub change: OverrideChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", content = "span", rename_all = "snake_case")]
pub enum OverrideChange {
    /// This occurrence does not happen. RFC 5545's `EXDATE`.
    Cancelled,
    /// This occurrence happens, with a different span.
    Rescheduled(ScheduleSpan),
}

/// Time actually spent, as opposed to time planned.
///
/// Always a concrete one-off, never a rule. It sits apart from the plan, so
/// logged time survives the meeting being cancelled and editing a plan does
/// not rewrite history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActualRecord {
    pub id: ActualId,
    /// The occurrence this time was spent against. Absent for unplanned work,
    /// which is still worth recording.
    pub planned: Option<PlannedRef>,
    pub span: ActualSpan,
}

/// The occurrence an actual was logged against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedRef {
    pub item: ScheduleItemId,
    pub recurrence_id: RecurrenceId,
    /// Schedule definition accepted when recording began. For an imported
    /// event, this also pins its embedded provider overrides.
    pub schedule: ObjectRevisionRef,
    /// Present only when a standalone, locally authored override applied.
    pub override_revision: Option<ObjectRevisionRef>,
    /// Floating and date-only plans depend on the observer's timezone.
    pub observer: Tz,
    /// The planned bounds in effect when recording began. Later travel and
    /// timezone database changes do not move them.
    pub span: TimeRange,
}

/// A timer writes twice and only twice: once on start, once on stop.
///
/// Writing progress on every tick would add a retained revision every minute.
/// Elapsed time for a running timer is computed from `started`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ActualSpan {
    Running { started: DateTime<Utc> },
    Complete(TimeRange),
}

/// A computed instance of a series. Never stored: a client expands what it
/// needs for the window it is showing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub item: ScheduleItemId,
    pub recurrence_id: RecurrenceId,
    pub span: TimeRange,
    pub origin: OccurrenceOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", content = "override_id", rename_all = "snake_case")]
pub enum OccurrenceOrigin {
    /// Straight from the recurrence rule.
    Rule,
    /// An override supplied this span (including provider-added dates).
    Overridden(OverrideId),
}
