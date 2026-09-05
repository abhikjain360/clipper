//! The stored schedule entities.
//!
//! Three record types: a series definition, an override
//! for the occurrences that deviate from it, and an actual for what really
//! happened. They are separate because they have different writers and
//! different lifetimes — an actual outlives the meeting it was logged against.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;
use clipper_api_types::ObjectId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    recurrence::Recurrence,
    time::{ResolvedSpan, ScheduleSpan},
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
/// One record per *series*, not per occurrence. A daily alarm for a year
/// is this struct once, not 365 times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleItem {
    pub id: ScheduleItemId,
    pub title: String,
    /// The span of the first occurrence. Later occurrences take their start
    /// from the recurrence rule and their length from here.
    pub span: ScheduleSpan,
    pub recurrence: Recurrence,
    /// A block may point at another Clipper object, or be bare labelled
    /// time. Optional so that no task subsystem is required for the schedule to
    /// be useful.
    pub reference: Option<ObjectId>,
    /// When this block should raise an alarm. Absent means silent, which is the
    /// default — most blocks are a record of intent, not a reason to wake
    /// someone.
    #[serde(default)]
    pub alarm: Option<crate::alarm::AlarmPolicy>,
}

impl ScheduleItem {
    /// Whether overrides authored against this definition can apply unchanged
    /// to another definition. Cosmetic and alarm edits preserve the occurrence
    /// structure; changing the span or recurrence requires an explicit decision.
    pub fn overrides_compatible_with(&self, other: &Self) -> bool {
        self.id == other.id && self.span == other.span && self.recurrence == other.recurrence
    }
}

/// The exact immutable envelope accepted for an encrypted schedule object.
/// The hash disambiguates signed replacements using the same revision number;
/// it is the envelope body hash, not a hash of the decrypted schedule data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectRevisionRef {
    pub object_id: ObjectId,
    pub revision: u64,
    pub body_hash: [u8; 32],
}

/// A separately stored override, anchored to the definition it was authored
/// against. Callers must check compatibility before applying it to a newer
/// definition; the recurrence engine receives only validated overrides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrenceOverride {
    pub base: ObjectRevisionRef,
    pub override_data: OccurrenceOverrideData,
}

/// Which occurrence of a series something refers to.
///
/// RFC 5545's `RECURRENCE-ID`, and deliberately not a bare instant. A floating
/// series is identified by wall-clock time because that is what is stable when
/// the observer moves; a zoned series by its absolute instant. Using an instant
/// for both would make an override recorded in Berlin fail to match the same
/// occurrence expanded in Tokyo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "at", rename_all = "snake_case")]
pub enum RecurrenceId {
    Floating(NaiveDateTime),
    Instant(DateTime<Utc>),
    Date(NaiveDate),
}

/// One occurrence that deviates from its series.
///
/// This is the pure expansion input, also embedded in imported calendar
/// objects. Standalone persisted overrides use [`OccurrenceOverride`] to
/// retain their base revision; embedded provider overrides share the imported
/// object’s revision.
///
/// Exists only for occurrences that actually differ — the other 364 days of the
/// year have no record at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrenceOverrideData {
    pub id: OverrideId,
    pub item: ScheduleItemId,
    /// The occurrence this replaces, identified by where the *rule* put it.
    /// Stays fixed when the override moves the occurrence, which is what lets
    /// the two be matched back up.
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
/// Always a concrete one-off, never a rule. Separate from the plan so that
/// logged time survives the meeting being cancelled, and so that a plan can be
/// edited without rewriting history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActualRecord {
    pub id: ActualId,
    /// What this was time *against*, if anything. Unplanned work is still worth
    /// recording, so this is optional.
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
    /// Effective planned bounds when recording began. Retained independently
    /// of later travel and timezone database changes.
    pub span: ResolvedSpan,
}

/// A timer writes twice and only twice: once on start, once on stop.
///
/// Persisting progress on a tick would multiply retained revisions by the
/// minute. Elapsed time for a running timer is derived from `started`, not
/// stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ActualSpan {
    Running { started: DateTime<Utc> },
    Complete(ResolvedSpan),
}

/// A computed instance of a series. Never stored — clients expand what they
/// need for the window they are showing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub item: ScheduleItemId,
    pub recurrence_id: RecurrenceId,
    pub span: ResolvedSpan,
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
