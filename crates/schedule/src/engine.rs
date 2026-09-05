//! Turning a recurrence rule into concrete occurrences.
//!
//! Expansion is client-side and always bounded by an explicit window: the
//! caller decides how far to look, because a phone expanding tomorrow's alarms
//! and a desktop rendering a month have different appetites. There is no
//! unbounded scan anywhere in this module.
//!
//! `rrule` sits behind [`RecurrenceEngine`] rather than being used directly, so
//! the rest of Clipper never sees an RFC 5545 string. The iCalendar text this
//! adapter generates is an implementation detail of the crate boundary.

use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::Arc,
};

use chrono::{DateTime, TimeDelta, Utc, Weekday};
use chrono_tz::Tz;
use clipper_api_types::ObjectId;

use crate::{
    item::{
        Occurrence, OccurrenceOrigin, OccurrenceOverrideData, OverrideChange, RecurrenceId,
        ScheduleItem,
    },
    recurrence::{Cadence, Frequency, MonthlyRule, Recurrence, RecurrenceEnd, ValidatedRrule},
    time::{ScheduleSpan, TimeError, TimedStart},
};

/// A half-open instant range, `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
}

impl Window {
    pub fn new(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Self, EngineError> {
        if to <= from {
            return Err(EngineError::EmptyWindow { from, to });
        }
        Ok(Self { from, to })
    }

    pub fn from(&self) -> DateTime<Utc> {
        self.from
    }

    pub fn to(&self) -> DateTime<Utc> {
        self.to
    }

    pub fn contains(&self, instant: DateTime<Utc>) -> bool {
        instant >= self.from && instant < self.to
    }
}

/// Where and when to expand.
#[derive(Debug, Clone, Copy)]
pub struct Expansion {
    pub window: Window,
    /// Resolves floating and all-day spans, which carry no zone of their own.
    /// A floating 07:00 alarm expands to 07:00 *here*.
    pub observer: Tz,
}

pub trait RecurrenceEngine {
    /// Every occurrence of `item` whose start falls in the expansion window,
    /// with overrides applied, ordered by start.
    ///
    /// `overrides` may contain entries for other items; they are ignored.
    fn occurrences(
        &self,
        item: &ScheduleItem,
        overrides: &[OccurrenceOverrideData],
        expansion: &Expansion,
    ) -> Result<Vec<Occurrence>, EngineError>;

    /// Every occurrence whose half-open span intersects the expansion window.
    ///
    /// Calendar views need this rather than start-in-window semantics: an
    /// overnight block that starts yesterday still occupies time today. The
    /// widened rule window remains bounded by the longest relevant span, and
    /// the regular candidate ceiling still applies to everything it produces.
    fn overlapping_occurrences(
        &self,
        item: &ScheduleItem,
        overrides: &[OccurrenceOverrideData],
        expansion: &Expansion,
    ) -> Result<Vec<Occurrence>, EngineError> {
        let lookback = maximum_lookback(item, overrides)?;
        let from = expansion
            .window
            .from()
            .checked_sub_signed(lookback)
            .ok_or(TimeError::DateOverflow)?;
        let widened = Expansion {
            window: Window::new(from, expansion.window.to())?,
            observer: expansion.observer,
        };
        let mut occurrences = self.occurrences(item, overrides, &widened)?;
        occurrences.retain(|occurrence| {
            occurrence.span.start < expansion.window.to()
                && expansion.window.from() < occurrence.span.end
        });
        Ok(occurrences)
    }

    /// The first occurrence starting strictly after `after`, looking at most
    /// `within` ahead.
    ///
    /// The bound is a parameter rather than a default because an unbounded
    /// "next occurrence ever" cannot be answered for a rule with no end.
    fn next_after(
        &self,
        item: &ScheduleItem,
        overrides: &[OccurrenceOverrideData],
        after: DateTime<Utc>,
        within: TimeDelta,
        observer: Tz,
    ) -> Result<Option<Occurrence>, EngineError> {
        let expansion = Expansion {
            window: Window::new(after, after + within)?,
            observer,
        };
        Ok(self
            .occurrences(item, overrides, &expansion)?
            .into_iter()
            .find(|occurrence| occurrence.span.start > after))
    }
}

/// A conservative elapsed-time bound for how far before a view an occurrence
/// may start and still overlap it.
fn maximum_lookback(
    item: &ScheduleItem,
    overrides: &[OccurrenceOverrideData],
) -> Result<TimeDelta, TimeError> {
    let mut lookback = span_lookback(&item.span)?;
    for entry in overrides.iter().filter(|entry| entry.item == item.id) {
        let OverrideChange::Rescheduled(span) = &entry.change else {
            continue;
        };
        lookback = lookback.max(span_lookback(span)?);
    }
    Ok(lookback)
}

fn span_lookback(span: &ScheduleSpan) -> Result<TimeDelta, TimeError> {
    match span {
        ScheduleSpan::Timed { duration, .. } => {
            TimeDelta::try_minutes(i64::from(duration.minutes())).ok_or(TimeError::DateOverflow)
        }
        ScheduleSpan::AllDay { days, .. } => {
            // Local-midnight intervals vary at offset transitions. Two extra
            // days conservatively cover the full IANA offset range, including
            // date-line changes such as Samoa's skipped day.
            let days = i64::from(days.get())
                .checked_add(2)
                .ok_or(TimeError::DateOverflow)?;
            TimeDelta::try_days(days).ok_or(TimeError::DateOverflow)
        }
    }
}

/// [`RecurrenceEngine`] backed by the `rrule` crate.
///
/// Chosen over `calcard` on measured behaviour, not reputation: both pass the
/// same 13-case corpus (see `tests/corpus.rs`), and they diverge only at a DST
/// gap, where `rrule` shifts forward the way `java.time` does. That matches the
/// alarm behaviour already in daily use.
#[derive(Debug, Clone)]
pub struct RruleEngine {
    /// Hard ceiling on rule-generated candidates per expansion. Guards against
    /// a dense rule and a wide window producing unbounded work; exceeding it is
    /// an error rather than a silent truncation.
    max_candidates: usize,
    imported_rules: Arc<ImportedRuleResolver>,
}

impl Default for RruleEngine {
    fn default() -> Self {
        Self {
            max_candidates: 10_000,
            imported_rules: Arc::new(ImportedRuleResolver::default()),
        }
    }
}

impl RruleEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_candidates(max_candidates: usize) -> Self {
        Self {
            max_candidates,
            imported_rules: Arc::new(ImportedRuleResolver::default()),
        }
    }

    /// Builds an engine over rules recovered from one or more raw import
    /// snapshots. The resolver is owned so the same engine can be cached and
    /// reused for every event in those snapshots.
    pub fn with_imported_rules(imported_rules: ImportedRuleResolver) -> Self {
        Self {
            imported_rules: Arc::new(imported_rules),
            ..Self::default()
        }
    }

    /// Rule-generated spans whose start lies in the window, before overrides.
    fn rule_spans(
        &self,
        item: &ScheduleItem,
        expansion: &Expansion,
    ) -> Result<Vec<(RecurrenceId, ScheduleSpan)>, EngineError> {
        let rule_line = match &item.recurrence {
            // A one-off needs no expansion library at all.
            Recurrence::Once => {
                let resolved = item.span.resolve(expansion.observer)?;
                return Ok(if expansion.window.contains(resolved.start) {
                    vec![(
                        recurrence_id(item, item.span.local_start(), expansion),
                        item.span.clone(),
                    )]
                } else {
                    Vec::new()
                });
            }
            Recurrence::Every(cadence) => rrule_line(cadence),
            Recurrence::Imported { import, uid } => self
                .imported_rules
                .lookup(*import, uid)
                .ok_or_else(|| EngineError::MissingImportedRule {
                    import: *import,
                    uid: uid.clone(),
                })?
                .as_str()
                .to_string(),
        };

        let zone = effective_zone(item, expansion.observer);
        let text = format!(
            "DTSTART;TZID={}:{}\nRRULE:{}",
            zone.name(),
            item.span.local_start().format("%Y%m%dT%H%M%S"),
            rule_line,
        );
        let set = rrule::RRuleSet::from_str(&text)
            .map_err(|source| EngineError::RuleRejected(source.to_string()))?;

        // rrule's `after` filter does not fast-forward: it still generates all
        // history. Collect from DTSTART through the requested end under the
        // library's public hard cap and reject any truncation it reports.
        // rrule 0.14 unfortunately does not propagate a nested rule iterator's
        // empty-period stop into this bit; impossible sparse raw rules remain
        // an upstream limitation, but generated history is strictly bounded.
        const MAX_SCANNED_CANDIDATES: u16 = u16::MAX;
        let before = expansion
            .window
            .to()
            .checked_sub_signed(TimeDelta::nanoseconds(1))
            .expect("a non-empty window cannot end at chrono's minimum")
            .with_timezone(&set.get_dt_start().timezone());
        let result = set.before(before).all(MAX_SCANNED_CANDIDATES);
        if result.limited {
            return Err(EngineError::ScanLimitExceeded {
                limit: usize::from(MAX_SCANNED_CANDIDATES),
            });
        }

        let mut spans = Vec::new();
        for occurrence in result.dates {
            let instant = occurrence.with_timezone(&Utc);
            if instant < expansion.window.from() {
                continue;
            }
            if spans.len() >= self.max_candidates {
                return Err(EngineError::ExpansionLimitExceeded {
                    limit: self.max_candidates,
                });
            }
            let local = occurrence.naive_local();
            spans.push((
                recurrence_id(item, local, expansion),
                span_at(item, local, zone),
            ));
        }
        Ok(spans)
    }
}

/// Validated opaque recurrence rules recovered from immutable import files.
///
/// This map is deliberately runtime-only. [`Recurrence::Imported`] persists a
/// snapshot reference and provider UID; the rule text is parsed from that
/// snapshot when a client prepares an expansion engine.
#[derive(Debug, Clone, Default)]
pub struct ImportedRuleResolver {
    rules: HashMap<(ObjectId, String), ValidatedRrule>,
}

impl ImportedRuleResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates and adds one provider rule.
    pub fn insert(
        &mut self,
        import: ObjectId,
        uid: impl Into<String>,
        rule: impl Into<String>,
    ) -> Result<(), crate::recurrence::RecurrenceError> {
        self.rules
            .insert((import, uid.into()), ValidatedRrule::new(rule)?);
        Ok(())
    }

    /// Adds all rules from another parsed snapshot set.
    pub fn merge(&mut self, other: Self) {
        self.rules.extend(other.rules);
    }

    pub fn lookup(&self, import: ObjectId, uid: &str) -> Option<&ValidatedRrule> {
        self.rules.get(&(import, uid.to_owned()))
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

impl RecurrenceEngine for RruleEngine {
    fn occurrences(
        &self,
        item: &ScheduleItem,
        overrides: &[OccurrenceOverrideData],
        expansion: &Expansion,
    ) -> Result<Vec<Occurrence>, EngineError> {
        let relevant: HashMap<RecurrenceId, &OccurrenceOverrideData> = overrides
            .iter()
            .filter(|entry| entry.item == item.id)
            .map(|entry| (entry.recurrence_id, entry))
            .collect();

        let mut out = Vec::new();
        let mut handled: HashSet<RecurrenceId> = HashSet::new();

        for (recurrence_id, span) in self.rule_spans(item, expansion)? {
            handled.insert(recurrence_id);
            match relevant.get(&recurrence_id) {
                Some(entry) => match &entry.change {
                    OverrideChange::Cancelled => continue,
                    OverrideChange::Rescheduled(moved) => {
                        let resolved = moved.resolve(expansion.observer)?;
                        if expansion.window.contains(resolved.start) {
                            out.push(Occurrence {
                                item: item.id,
                                recurrence_id,
                                span: resolved,
                                origin: OccurrenceOrigin::Overridden(entry.id),
                            });
                        }
                    }
                },
                None => out.push(Occurrence {
                    item: item.id,
                    recurrence_id,
                    span: span.resolve(expansion.observer)?,
                    origin: OccurrenceOrigin::Rule,
                }),
            }
        }

        // An occurrence can be moved *into* the window from a rule position
        // outside it. Those are invisible to the loop above, which only walks
        // rule positions the window contains.
        for entry in relevant.values() {
            if handled.contains(&entry.recurrence_id) {
                continue;
            }
            let OverrideChange::Rescheduled(moved) = &entry.change else {
                continue;
            };
            let resolved = moved.resolve(expansion.observer)?;
            if expansion.window.contains(resolved.start) {
                out.push(Occurrence {
                    item: item.id,
                    recurrence_id: entry.recurrence_id,
                    span: resolved,
                    origin: OccurrenceOrigin::Overridden(entry.id),
                });
            }
        }

        out.sort_by_key(|occurrence| occurrence.span.start);
        Ok(out)
    }
}

/// The zone a rule expands in.
///
/// Floating and all-day series expand in the observer's zone, which is what
/// makes a 07:00 alarm ring at 07:00 after you fly. A zoned series expands in
/// its own zone and does not move.
fn effective_zone(item: &ScheduleItem, observer: Tz) -> Tz {
    match &item.span {
        ScheduleSpan::Timed {
            start: TimedStart::Zoned { zone, .. },
            ..
        } => *zone,
        ScheduleSpan::Timed {
            start: TimedStart::Floating(_),
            ..
        }
        | ScheduleSpan::AllDay { .. } => observer,
    }
}

/// Identify an occurrence in a way that does not depend on who is looking.
fn recurrence_id(
    item: &ScheduleItem,
    local: chrono::NaiveDateTime,
    expansion: &Expansion,
) -> RecurrenceId {
    match &item.span {
        ScheduleSpan::Timed {
            start: TimedStart::Floating(_),
            ..
        } => RecurrenceId::Floating(local),
        ScheduleSpan::Timed {
            start: TimedStart::Zoned { zone, .. },
            ..
        } => {
            let start = TimedStart::Zoned { local, zone: *zone };
            match start.resolve(expansion.observer) {
                Ok(instant) => RecurrenceId::Instant(instant),
                // `resolve` shifts unresolvable local times rather than
                // failing, so this arm is unreachable in practice — but it must
                // not panic if that ever changes.
                Err(_) => RecurrenceId::Floating(local),
            }
        }
        ScheduleSpan::AllDay { .. } => RecurrenceId::Date(local.date()),
    }
}

/// Rebuild the item's span at a new start, preserving its kind and length.
fn span_at(item: &ScheduleItem, local: chrono::NaiveDateTime, zone: Tz) -> ScheduleSpan {
    match &item.span {
        ScheduleSpan::Timed {
            start: TimedStart::Floating(_),
            duration,
        } => ScheduleSpan::Timed {
            start: TimedStart::Floating(local),
            duration: *duration,
        },
        ScheduleSpan::Timed {
            start: TimedStart::Zoned { .. },
            duration,
        } => ScheduleSpan::Timed {
            start: TimedStart::Zoned { local, zone },
            duration: *duration,
        },
        ScheduleSpan::AllDay { days, .. } => ScheduleSpan::AllDay {
            start: local.date(),
            days: *days,
        },
    }
}

fn rrule_line(cadence: &Cadence) -> String {
    // RFC 5545 requires FREQ first.
    let mut parts = Vec::new();
    match &cadence.frequency {
        Frequency::Daily => parts.push("FREQ=DAILY".to_string()),
        Frequency::Weekly { weekdays } => {
            parts.push("FREQ=WEEKLY".to_string());
            let days: Vec<&str> = weekdays.iter().map(ical_weekday).collect();
            parts.push(format!("BYDAY={}", days.join(",")));
            parts.push("WKST=MO".to_string());
        }
        Frequency::Monthly(rule) => {
            parts.push("FREQ=MONTHLY".to_string());
            match rule {
                MonthlyRule::OnDay(day) => parts.push(format!("BYMONTHDAY={}", day.as_ical())),
                MonthlyRule::OnWeekday { nth, weekday } => {
                    parts.push(format!("BYDAY={}{}", nth.as_ical(), ical_weekday(*weekday)));
                }
            }
        }
        Frequency::Yearly { month, day } => {
            parts.push("FREQ=YEARLY".to_string());
            parts.push(format!("BYMONTH={}", month.number_from_month()));
            parts.push(format!("BYMONTHDAY={}", day.as_ical()));
        }
    }
    parts.push(format!("INTERVAL={}", cadence.interval));
    match cadence.end {
        RecurrenceEnd::Never => {}
        RecurrenceEnd::After(count) => parts.push(format!("COUNT={count}")),
        RecurrenceEnd::On(instant) => {
            parts.push(format!("UNTIL={}", instant.format("%Y%m%dT%H%M%SZ")));
        }
    }
    parts.join(";")
}

fn ical_weekday(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error("expansion window is empty: {from} is not before {to}")]
    EmptyWindow {
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    },
    #[error("recurrence rule was rejected by the expansion library: {0}")]
    RuleRejected(String),
    #[error("imported recurrence rule {uid:?} is unavailable in snapshot {import}")]
    MissingImportedRule { import: ObjectId, uid: String },
    #[error("expansion produced more than {limit} candidates; narrow the window")]
    ExpansionLimitExceeded { limit: usize },
    #[error("recurrence expansion scanned more than {limit} historical candidates")]
    ScanLimitExceeded { limit: usize },
    #[error(transparent)]
    Time(#[from] TimeError),
}
