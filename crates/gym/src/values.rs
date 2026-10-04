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
        validate_distinct_exercises(self.exercises.iter().map(|exercise| exercise.exercise_id))
    }
}

pub const DEFAULT_WARM_UP_REST_SECONDS: u32 = 60;

fn default_warm_up_rest_seconds() -> u32 {
    DEFAULT_WARM_UP_REST_SECONDS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkoutExercise {
    pub exercise_id: Uuid,
    pub warm_up_sets: u32,
    #[serde(default = "default_warm_up_rest_seconds")]
    pub warm_up_rest_seconds: u32,
    pub target_sets: u32,
    pub target_reps: u32,
    pub target_reps_in_reserve: Option<u8>,
    pub rest_seconds: u32,
    pub superset_with_previous: bool,
}

impl WorkoutExercise {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_targets(
            self.exercise_id,
            self.target_sets,
            self.target_reps,
            self.target_reps_in_reserve,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub template_id: Option<Uuid>,
    pub notes: String,
    pub exercises: Vec<SessionExercise>,
    #[serde(default)]
    pub current_exercise_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionExercise {
    pub exercise_id: Uuid,
    pub warm_up_sets: u32,
    #[serde(default = "default_warm_up_rest_seconds")]
    pub warm_up_rest_seconds: u32,
    pub target_sets: u32,
    pub target_reps: u32,
    pub target_reps_in_reserve: Option<u8>,
    pub rest_seconds: u32,
    pub superset_with_previous: bool,
    pub skipped: bool,
}

impl SessionExercise {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_targets(
            self.exercise_id,
            self.target_sets,
            self.target_reps,
            self.target_reps_in_reserve,
        )
    }
}

impl From<WorkoutExercise> for SessionExercise {
    fn from(planned: WorkoutExercise) -> Self {
        Self {
            exercise_id: planned.exercise_id,
            warm_up_sets: planned.warm_up_sets,
            warm_up_rest_seconds: planned.warm_up_rest_seconds,
            target_sets: planned.target_sets,
            target_reps: planned.target_reps,
            target_reps_in_reserve: planned.target_reps_in_reserve,
            rest_seconds: planned.rest_seconds,
            superset_with_previous: planned.superset_with_previous,
            skipped: false,
        }
    }
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
        for exercise in &self.exercises {
            exercise.validate()?;
        }
        validate_distinct_exercises(self.exercises.iter().map(|exercise| exercise.exercise_id))
    }

    pub fn keeping_end_of(mut self, other: &Session) -> Session {
        if self.ended_at.is_none() {
            self.ended_at = other.ended_at;
        }
        self
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
    pub const CONFLICT_RULE: ConflictRule = ConflictRule::LastWriteWins;
    const ROW_ID_NAMESPACE: Uuid = Uuid::from_u128(0x2c4e_91d7_63a8_4f0b_8d15_e9b2_7a46_c3f1);

    pub fn row_id(session_id: Uuid, order: u32) -> Uuid {
        let mut name = session_id.as_bytes().to_vec();
        name.extend_from_slice(&order.to_be_bytes());
        Uuid::new_v5(&Self::ROW_ID_NAMESPACE, &name)
    }

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
    const ROW_ID_NAMESPACE: Uuid = Uuid::from_u128(0x5f0c_2b8e_9d41_4a37_b6e2_7c18_a3d9_0e54);

    pub fn row_id(muscle: Muscle) -> Uuid {
        Uuid::new_v5(&Self::ROW_ID_NAMESPACE, muscle.name().as_bytes())
    }

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
    #[error("exercise {0} appears more than once")]
    DuplicateExercise(Uuid),
}

fn validate_name(name: &str) -> Result<(), ValidationError> {
    if name.trim().is_empty() {
        return Err(ValidationError::EmptyName);
    }
    Ok(())
}

fn validate_targets(
    exercise_id: Uuid,
    target_sets: u32,
    target_reps: u32,
    target_reps_in_reserve: Option<u8>,
) -> Result<(), ValidationError> {
    validate_exercise_id(exercise_id)?;
    if target_sets == 0 {
        return Err(ValidationError::ZeroTargetSets);
    }
    if target_reps == 0 {
        return Err(ValidationError::ZeroTargetReps);
    }
    validate_reps_in_reserve(target_reps_in_reserve)
}

fn validate_distinct_exercises(ids: impl Iterator<Item = Uuid>) -> Result<(), ValidationError> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(ValidationError::DuplicateExercise(id));
        }
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
