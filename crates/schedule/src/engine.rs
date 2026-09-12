//! Turns a recurrence rule into concrete occurrences.
//!
//! Expansion runs on the client and always takes an explicit window. The caller
//! picks how far to look: a phone expands tomorrow's alarms, a desktop renders a
//! month. Nothing here scans without a bound.
//!
//! `rrule` stays behind [`RecurrenceEngine`], so no RFC 5545 string leaves this
//! module. The iCalendar text built below is internal to that boundary.

use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::Arc,
};

use chrono::{DateTime, Days, TimeDelta, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use clipper_api_types::ObjectId;

use crate::{
    item::{
        Occurrence, OccurrenceOrigin, OccurrenceOverrideData, OverrideChange, RecurrenceId,
        ScheduleItem,
    },
    recurrence::{
        Cadence, Frequency, MonthlyRule, Recurrence, RecurrenceEnd, ValidatedRrule,
        until_wall_clock,
    },
    time::{ScheduleSpan, TimeError, TimeRange, TimedStart},
};

/// Where and when to expand.
#[derive(Debug, Clone, Copy)]
pub struct Expansion {
    pub window: TimeRange,
    /// Zone for floating and all-day spans, which carry no zone of their own.
    /// A floating 07:00 alarm expands to 07:00 in this zone.
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
    /// Calendar views use this instead of [`Self::occurrences`]: an overnight
    /// block that starts yesterday still occupies time today. Widening the rule
    /// window by the longest span keeps it bounded, and the candidate ceiling
    /// still applies.
    fn overlapping_occurrences(
        &self,
        item: &ScheduleItem,
        overrides: &[OccurrenceOverrideData],
        expansion: &Expansion,
    ) -> Result<Vec<Occurrence>, EngineError> {
        let lookback = maximum_lookback(item, overrides)?;
        let from = expansion
            .window
            .start()
            .checked_sub_signed(lookback)
            .ok_or(TimeError::DateOverflow)?;
        let widened = Expansion {
            window: TimeRange::new(from, expansion.window.end())?,
            observer: expansion.observer,
        };
        let mut occurrences = self.occurrences(item, overrides, &widened)?;
        occurrences.retain(|occurrence| occurrence.span.overlaps(&expansion.window));
        Ok(occurrences)
    }

    /// The first occurrence starting strictly after `after`, looking at most
    /// `within` ahead.
    ///
    /// `within` is required. A rule with no end may have its next occurrence
    /// arbitrarily far away, so an unbounded search has nothing to stop it.
    fn next_after(
        &self,
        item: &ScheduleItem,
        overrides: &[OccurrenceOverrideData],
        after: DateTime<Utc>,
        within: TimeDelta,
        observer: Tz,
    ) -> Result<Option<Occurrence>, EngineError> {
        let expansion = Expansion {
            window: TimeRange::new(after, after + within)?,
            observer,
        };
        Ok(self
            .occurrences(item, overrides, &expansion)?
            .into_iter()
            .find(|occurrence| occurrence.span.start() > after))
    }
}

/// How far before a window an occurrence may start and still overlap it.
/// Rounded up, never down.
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
            // A local-midnight day is not always 24 hours. Two extra days
            // cover the whole IANA offset range, including date-line moves
            // such as Samoa's skipped day.
            let days = i64::from(days.get())
                .checked_add(2)
                .ok_or(TimeError::DateOverflow)?;
            TimeDelta::try_days(days).ok_or(TimeError::DateOverflow)
        }
    }
}

/// [`RecurrenceEngine`] backed by the `rrule` crate.
///
/// At a DST gap an occurrence shifts forward, not back, matching platform
/// alarm clocks.
#[derive(Debug, Clone)]
pub struct RruleEngine {
    /// Most rule-generated candidates one expansion may produce. A dense rule
    /// over a wide window hits this and errors; it never truncates silently.
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

    /// Builds an engine over rules parsed from import snapshots. The engine
    /// owns the resolver, so one engine serves every event in those snapshots.
    pub fn with_imported_rules(imported_rules: ImportedRuleResolver) -> Self {
        Self {
            imported_rules: Arc::new(imported_rules),
            ..Self::default()
        }
    }

    /// Rule-generated occurrences whose start lies in the window, before
    /// overrides. Each one is already resolved to an absolute interval.
    fn rule_spans(
        &self,
        item: &ScheduleItem,
        expansion: &Expansion,
    ) -> Result<Vec<(RecurrenceId, TimeRange)>, EngineError> {
        let zone = effective_zone(item, expansion.observer);
        let rule_line = match &item.recurrence {
            // A one-off has no rule to expand.
            Recurrence::Once => {
                let resolved = item.span.resolve(expansion.observer)?;
                return Ok(if expansion.window.contains(resolved.start()) {
                    vec![(
                        recurrence_id(item, item.span.local_start(), expansion),
                        resolved,
                    )]
                } else {
                    Vec::new()
                });
            }
            Recurrence::Every(cadence) => rrule_line(cadence, zone),
            Recurrence::Imported { import, uid } => until_wall_clock(
                self.imported_rules
                    .lookup(*import, uid)
                    .ok_or_else(|| EngineError::MissingImportedRule {
                        import: *import,
                        uid: uid.clone(),
                    })?
                    .as_str(),
                zone,
            ),
        };

        // Give `rrule` a UTC wall-clock DTSTART. It then does pure calendar
        // arithmetic and never resolves a local time itself, so a series whose
        // own start falls in a DST gap still expands. Every candidate comes
        // back as a wall clock, and `ScheduleSpan::resolve` applies this
        // crate's gap and fold policy to it below.
        let text = format!(
            "DTSTART:{}Z\nRRULE:{}",
            item.span.local_start().format("%Y%m%dT%H%M%S"),
            rule_line,
        );
        let set = rrule::RRuleSet::from_str(&text)
            .map_err(|source| EngineError::RuleRejected(source.to_string()))?;

        // rrule's `after` filter does not skip ahead; it still generates every
        // occurrence from DTSTART. So collect from DTSTART to the window end
        // under the library's hard cap and treat a truncated result as an
        // error.
        //
        // rrule 0.14 does not set `limited` when a nested iterator gives up on
        // an impossible sparse rule, so such a rule yields nothing instead of
        // erroring. The work it does stays capped either way.
        const MAX_SCANNED_CANDIDATES: u16 = u16::MAX;
        // The bound is a wall clock too, so it is the window end read in the
        // expansion zone. One day of slack covers the offset between a
        // candidate's wall clock and the instant it resolves to; the window
        // check below is on the instant and decides the real edge.
        let before_local = expansion
            .window
            .end()
            .with_timezone(&zone)
            .naive_local()
            .checked_add_days(Days::new(1))
            .ok_or(TimeError::DateOverflow)?;
        let before = Utc
            .from_utc_datetime(&before_local)
            .with_timezone(&rrule::Tz::UTC);
        let result = set.before(before).all(MAX_SCANNED_CANDIDATES);
        if result.limited {
            return Err(EngineError::ScanLimitExceeded {
                limit: usize::from(MAX_SCANNED_CANDIDATES),
            });
        }

        let mut spans = Vec::new();
        for occurrence in result.dates {
            // DTSTART was UTC, so this carries a wall clock, not an instant.
            let local = occurrence.naive_utc();
            let resolved = span_at(item, local, zone).resolve(expansion.observer)?;
            if !expansion.window.contains(resolved.start()) {
                continue;
            }
            if spans.len() >= self.max_candidates {
                return Err(EngineError::ExpansionLimitExceeded {
                    limit: self.max_candidates,
                });
            }
            spans.push((recurrence_id(item, local, expansion), resolved));
        }
        Ok(spans)
    }
}

/// Validated recurrence rules parsed from import snapshots.
///
/// Runtime-only, never persisted. [`Recurrence::Imported`] stores a snapshot
/// reference and a provider UID; the rule text is read back out of the snapshot
/// each time a client builds an engine.
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

    /// Adds every rule from another resolver.
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
                        if expansion.window.contains(resolved.start()) {
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
                    span,
                    origin: OccurrenceOrigin::Rule,
                }),
            }
        }

        // An override can move an occurrence into the window from a rule
        // position outside it. The loop above only walks rule positions inside
        // the window, so it misses those.
        //
        // This is also the `RDATE` path: an override naming an identity the
        // rule never generates adds that occurrence. The caller is responsible
        // for passing only overrides it trusts.
        for entry in relevant.values() {
            if handled.contains(&entry.recurrence_id) {
                continue;
            }
            let OverrideChange::Rescheduled(moved) = &entry.change else {
                continue;
            };
            let resolved = moved.resolve(expansion.observer)?;
            if expansion.window.contains(resolved.start()) {
                out.push(Occurrence {
                    item: item.id,
                    recurrence_id: entry.recurrence_id,
                    span: resolved,
                    origin: OccurrenceOrigin::Overridden(entry.id),
                });
            }
        }

        out.sort_by_key(|occurrence| occurrence.span.start());
        Ok(out)
    }
}

/// The zone a rule expands in.
///
/// Floating and all-day series expand in the observer's zone, so a 07:00 alarm
/// rings at 07:00 after the observer changes zone. A zoned series expands in
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

/// An occurrence identity that is the same for every observer.
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
                // `resolve` shifts a local time that does not exist rather
                // than failing, so this arm never runs today. Fall back
                // instead of panicking if that changes.
                Err(_) => RecurrenceId::Floating(local),
            }
        }
        ScheduleSpan::AllDay { .. } => RecurrenceId::Date(local.date()),
    }
}

/// Rebuilds the item's span at a new start, keeping its kind and length.
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

/// Builds the `RRULE` value for a cadence. `zone` is the zone the rule expands
/// in; it only affects `UNTIL`.
fn rrule_line(cadence: &Cadence, zone: Tz) -> String {
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
            // DTSTART is a UTC wall clock, so UNTIL must be one too: write the
            // wall clock this instant shows in the expansion zone. Inside a
            // fall-back hour that comparison can differ from the instant
            // comparison by at most that hour.
            parts.push(format!(
                "UNTIL={}",
                instant
                    .with_timezone(&zone)
                    .naive_local()
                    .format("%Y%m%dT%H%M%SZ")
            ));
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
