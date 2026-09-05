//! Scheduling domain types and recurrence expansion for Clipper.
//!
//! No I/O, no crypto, no storage. Every result follows from its inputs, so
//! recurrence and time behavior test without a server or a platform UI.
//!
//! Plan, deviation and outcome are three separate types:
//!
//! - [`ScheduleItem`] is a series, stored once however often it repeats.
//! - [`OccurrenceOverride`] exists only for an occurrence that differs from
//!   the series.
//! - [`ActualRecord`] is what happened. It sits apart from the plan, so logged
//!   time survives an edit to the series.
//!
//! Occurrences are computed on demand inside a caller-supplied window, never
//! stored.

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
