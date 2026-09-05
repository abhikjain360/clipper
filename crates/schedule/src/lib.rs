//! Scheduling domain types and recurrence expansion for Clipper.
//!
//! Pure: no I/O, no crypto, no storage. Everything here is computable from its
//! inputs, so recurrence and time behavior can be tested without running a
//! server or a platform UI.
//!
//! Three shapes matter, and they are separate types on purpose:
//!
//! - [`ScheduleItem`] is a *series*, stored once however often it repeats.
//! - [`OccurrenceOverride`] exists only for occurrences that deviate.
//! - [`ActualRecord`] is what really happened, kept apart from what was planned
//!   so that logged time survives the plan changing under it.
//!
//! Occurrences are computed, never stored, and always within a caller-supplied
//! window.

pub mod alarm;
pub mod engine;
pub mod ingest;
pub mod item;
pub mod recurrence;
pub mod summary;
pub mod time;

pub use alarm::{AlarmPolicy, PlannedAlarm, plan_alarms};
pub use engine::{EngineError, Expansion, ImportedRuleResolver, RecurrenceEngine, RruleEngine};
pub use ingest::{
    CalendarSource, IngestError, IngestOutcome, IngestedEvent, IngestedStatus, SkippedEvent,
    SourceId, SourceKind, parse_ics, parse_imported_recurrence_rules,
};
pub use item::{
    ActualId, ActualRecord, ActualSpan, ObjectRevisionRef, Occurrence, OccurrenceOrigin,
    OccurrenceOverride, OccurrenceOverrideData, OverrideChange, OverrideId, PlannedRef,
    RecurrenceId, ScheduleItem, ScheduleItemId,
};
pub use recurrence::{
    Cadence, Frequency, MonthDay, MonthlyRule, NthWeekday, Recurrence, RecurrenceEnd,
    RecurrenceError, ValidatedRrule, WeekdaySet,
};
pub use time::{BlockDuration, ScheduleSpan, TimeError, TimeRange, TimedStart};
