use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, NaiveDate, Utc, Weekday};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{BodyWeight, Session, Set, SetKind, ValidationError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastSessionSets {
    pub session_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub sets: Vec<Set>,
}

pub fn last_session_working_sets(
    exercise_id: Uuid,
    sessions: &BTreeMap<Uuid, Session>,
    sets: &[Set],
) -> Option<LastSessionSets> {
    let mut working_sets = BTreeMap::<Uuid, Vec<Set>>::new();
    for set in sets {
        if set.exercise_id == exercise_id && set.kind == SetKind::Working {
            working_sets
                .entry(set.session_id)
                .or_default()
                .push(set.clone());
        }
    }
    let (session_id, session) = sessions
        .iter()
        .filter(|(id, _)| working_sets.contains_key(id))
        .max_by_key(|(id, session)| (session.started_at, **id))?;
    let mut sets = working_sets.remove(session_id)?;
    sets.sort_by_key(|set| (set.order, set.completed_at));
    Some(LastSessionSets {
        session_id: *session_id,
        started_at: session.started_at,
        sets,
    })
}

pub fn estimated_one_rep_max(set: &Set) -> Option<f64> {
    let weight = set.weight_kg?;
    let reps = set.reps?;
    if !weight.is_finite() || weight <= 0.0 || reps == 0 {
        return None;
    }
    let kg = weight * (1.0 + f64::from(reps) / 30.0);
    kg.is_finite().then_some(kg)
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SessionOneRepMax {
    pub session_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub kg: f64,
}

pub fn best_one_rep_max_by_session(
    exercise_id: Uuid,
    sessions: &BTreeMap<Uuid, Session>,
    sets: &[Set],
) -> Vec<SessionOneRepMax> {
    let mut best = BTreeMap::<Uuid, f64>::new();
    for set in sets {
        if set.exercise_id != exercise_id || set.kind != SetKind::Working {
            continue;
        }
        if let Some(kg) = estimated_one_rep_max(set) {
            let current = best.entry(set.session_id).or_insert(kg);
            *current = current.max(kg);
        }
    }
    let mut progress: Vec<_> = best
        .into_iter()
        .filter_map(|(session_id, kg)| {
            sessions.get(&session_id).map(|session| SessionOneRepMax {
                session_id,
                started_at: session.started_at,
                kg,
            })
        })
        .collect();
    progress.sort_by_key(|session| (session.started_at, session.session_id));
    progress
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WeeklyBodyWeight {
    pub week_start: NaiveDate,
    pub average_kg: f64,
    pub measurements: usize,
    pub change_kg: Option<f64>,
}

pub fn weekly_body_weight(
    entries: &[BodyWeight],
) -> Result<Vec<WeeklyBodyWeight>, ValidationError> {
    let mut weeks = BTreeMap::<NaiveDate, (f64, usize)>::new();
    for entry in entries {
        entry.validate()?;
        let week = entry.time.iso_week();
        let start = NaiveDate::from_isoywd_opt(week.year(), week.week(), Weekday::Mon)
            .ok_or(ValidationError::WeekStartOutOfRange)?;
        let (average, count) = weeks.entry(start).or_default();
        *count += 1;
        *average += (entry.kg - *average) / *count as f64;
    }
    let mut previous: Option<(NaiveDate, f64)> = None;
    Ok(weeks
        .into_iter()
        .map(|(week_start, (average_kg, measurements))| {
            let change_kg = previous.and_then(|(start, average)| {
                (week_start.signed_duration_since(start).num_days() == 7)
                    .then_some(average_kg - average)
            });
            previous = Some((week_start, average_kg));
            WeeklyBodyWeight {
                week_start,
                average_kg,
                measurements,
                change_kg,
            }
        })
        .collect())
}
