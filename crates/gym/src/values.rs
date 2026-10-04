use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Muscle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictRule {
    AppendOnly,
    LastWriteWins,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exercise {
    pub name: String,
    pub muscles: Vec<MuscleShare>,
    pub archived: bool,
}

impl Exercise {
    pub const COLLECTION_NAME: &'static str = "gym.exercises";
    pub const CONFLICT_RULE: ConflictRule = ConflictRule::LastWriteWins;

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_name(&self.name)?;
        let mut muscles = BTreeSet::new();
        for target in &self.muscles {
            target.validate()?;
            if !muscles.insert(target.muscle) {
                return Err(ValidationError::DuplicateMuscle(target.muscle));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MuscleShare {
    pub muscle: Muscle,
    pub share: f64,
}

impl MuscleShare {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !self.share.is_finite() || !(0.0..=1.0).contains(&self.share) {
            return Err(ValidationError::InvalidMuscleShare {
                muscle: self.muscle,
                share: self.share,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkoutTemplate {
    pub name: String,
    pub exercises: Vec<WorkoutExercise>,
}

impl WorkoutTemplate {
    pub const COLLECTION_NAME: &'static str = "gym.workouts";
    pub const CONFLICT_RULE: ConflictRule = ConflictRule::LastWriteWins;

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_name(&self.name)?;
        for exercise in &self.exercises {
            exercise.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkoutExercise {
    pub exercise_id: Uuid,
    pub target_sets: u32,
    pub target_reps: u32,
    pub target_reps_in_reserve: Option<u8>,
    pub rest_seconds: u32,
}

impl WorkoutExercise {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_exercise_id(self.exercise_id)?;
        if self.target_sets == 0 {
            return Err(ValidationError::ZeroTargetSets);
        }
        if self.target_reps == 0 {
            return Err(ValidationError::ZeroTargetReps);
        }
        validate_reps_in_reserve(self.target_reps_in_reserve)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub template_id: Option<Uuid>,
    pub notes: String,
}

impl Session {
    pub const COLLECTION_NAME: &'static str = "gym.sessions";
    pub const CONFLICT_RULE: ConflictRule = ConflictRule::LastWriteWins;

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.ended_at.is_some_and(|end| end < self.started_at) {
            return Err(ValidationError::EndBeforeStart);
        }
        if let Some(id) = self.template_id
            && id.get_version() != Some(uuid::Version::SortRand)
        {
            return Err(ValidationError::InvalidTemplateId(id));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetKind {
    WarmUp,
    Working,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Set {
    pub session_id: Uuid,
    pub exercise_id: Uuid,
    pub order: u32,
    pub kind: SetKind,
    pub weight_kg: Option<f64>,
    pub reps: Option<u32>,
    pub reps_in_reserve: Option<u8>,
    pub completed_at: DateTime<Utc>,
}

impl Set {
    pub const COLLECTION_NAME: &'static str = "gym.sets";
    pub const CONFLICT_RULE: ConflictRule = ConflictRule::AppendOnly;

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.session_id.get_version() != Some(uuid::Version::SortRand) {
            return Err(ValidationError::InvalidSessionId(self.session_id));
        }
        validate_exercise_id(self.exercise_id)?;
        if let Some(kg) = self.weight_kg {
            validate_weight(kg)?;
        }
        validate_reps_in_reserve(self.reps_in_reserve)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BodyWeight {
    pub time: DateTime<Utc>,
    pub kg: f64,
}

impl BodyWeight {
    pub const COLLECTION_NAME: &'static str = "gym.body_weight";
    pub const CONFLICT_RULE: ConflictRule = ConflictRule::AppendOnly;

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_weight(self.kg)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Recovery {
    pub muscle: Muscle,
    pub recovery_days: f64,
}

impl Recovery {
    pub const COLLECTION_NAME: &'static str = "gym.recovery";
    pub const CONFLICT_RULE: ConflictRule = ConflictRule::LastWriteWins;

    pub fn validate(&self) -> Result<(), ValidationError> {
        if !self.recovery_days.is_finite() || !(0.5..=14.0).contains(&self.recovery_days) {
            return Err(ValidationError::InvalidRecoveryDays(self.recovery_days));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("name must not be empty")]
    EmptyName,
    #[error("{muscle} share must be between 0 and 1, got {share}")]
    InvalidMuscleShare { muscle: Muscle, share: f64 },
    #[error("{0} appears more than once")]
    DuplicateMuscle(Muscle),
    #[error("weight must be finite and positive, got {0}")]
    InvalidWeight(f64),
    #[error("reps in reserve must be between 0 and 10, got {0}")]
    InvalidRepsInReserve(u8),
    #[error("recovery days must be between 0.5 and 14, got {0}")]
    InvalidRecoveryDays(f64),
    #[error("target sets must be positive")]
    ZeroTargetSets,
    #[error("target reps must be positive")]
    ZeroTargetReps,
    #[error("session end must not precede its start")]
    EndBeforeStart,
    #[error("exercise id must be UUIDv7, got {0}")]
    InvalidExerciseId(Uuid),
    #[error("session id must be UUIDv7, got {0}")]
    InvalidSessionId(Uuid),
    #[error("template id must be UUIDv7, got {0}")]
    InvalidTemplateId(Uuid),
    #[error("week start is outside the supported date range")]
    WeekStartOutOfRange,
}

fn validate_name(name: &str) -> Result<(), ValidationError> {
    if name.trim().is_empty() {
        return Err(ValidationError::EmptyName);
    }
    Ok(())
}

fn validate_exercise_id(id: Uuid) -> Result<(), ValidationError> {
    if id.get_version() != Some(uuid::Version::SortRand) {
        return Err(ValidationError::InvalidExerciseId(id));
    }
    Ok(())
}

fn validate_weight(kg: f64) -> Result<(), ValidationError> {
    if !kg.is_finite() || kg <= 0.0 {
        return Err(ValidationError::InvalidWeight(kg));
    }
    Ok(())
}

fn validate_reps_in_reserve(reps: Option<u8>) -> Result<(), ValidationError> {
    if let Some(reps) = reps
        && reps > 10
    {
        return Err(ValidationError::InvalidRepsInReserve(reps));
    }
    Ok(())
}
