use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Exercise, Muscle, Recovery, Set, SetKind, ValidationError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FatigueBand {
    Recovered,
    Low,
    Moderate,
    High,
    VeryHigh,
}

impl FatigueBand {
    pub const fn from_score(score: u8) -> Self {
        match score {
            0..=20 => Self::Recovered,
            21..=40 => Self::Low,
            41..=60 => Self::Moderate,
            61..=80 => Self::High,
            _ => Self::VeryHigh,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MuscleFatigue {
    pub muscle: Muscle,
    pub score: u8,
    pub band: FatigueBand,
    pub recovery_days: f64,
}

pub fn fatigue_at(
    now: DateTime<Utc>,
    exercises: &BTreeMap<Uuid, Exercise>,
    sets: &[Set],
    recovery: &[Recovery],
) -> Result<Vec<MuscleFatigue>, ValidationError> {
    let mut recovery_days = BTreeMap::new();
    for row in recovery {
        row.validate()?;
        recovery_days.insert(row.muscle, row.recovery_days);
    }
    for exercise in exercises.values() {
        exercise.validate()?;
    }

    let mut totals = BTreeMap::<Muscle, f64>::new();
    for set in sets {
        if set.kind != SetKind::Working || set.completed_at > now {
            continue;
        }
        let days_since = (now - set.completed_at).as_seconds_f64() / 86_400.0;
        if days_since > 14.0 {
            continue;
        }
        set.validate()?;
        let Some(exercise) = exercises.get(&set.exercise_id) else {
            continue;
        };
        let effort = f64::from(10 - set.reps_in_reserve.unwrap_or(2)) / 10.0;
        for target in &exercise.muscles {
            let days = recovery_days
                .get(&target.muscle)
                .copied()
                .unwrap_or_else(|| target.muscle.default_recovery_days());
            let remaining = (1.0 - days_since / days).max(0.0);
            *totals.entry(target.muscle).or_default() += 15.0 * target.share * effort * remaining;
        }
    }

    Ok(Muscle::ALL
        .into_iter()
        .map(|muscle| {
            let score = totals
                .get(&muscle)
                .copied()
                .unwrap_or(0.0)
                .round()
                .min(100.0) as u8;
            MuscleFatigue {
                muscle,
                score,
                band: FatigueBand::from_score(score),
                recovery_days: recovery_days
                    .get(&muscle)
                    .copied()
                    .unwrap_or_else(|| muscle.default_recovery_days()),
            }
        })
        .collect())
}
