pub use clipper_gym::{FatigueBand, Muscle, MuscleGroup, SetKind};
use serde::{Deserialize, Serialize};

#[cfg(feature = "uniffi")]
#[uniffi::remote(Enum)]
pub enum Muscle {
    Chest,
    Shoulders,
    Triceps,
    Lats,
    UpperBack,
    Biceps,
    Forearms,
    Quadriceps,
    Hamstrings,
    Glutes,
    Calves,
    Abs,
    Obliques,
    Erectors,
}

#[cfg(feature = "uniffi")]
#[uniffi::remote(Enum)]
pub enum MuscleGroup {
    Push,
    Pull,
    Legs,
    Core,
}

#[cfg(feature = "uniffi")]
#[uniffi::remote(Enum)]
pub enum SetKind {
    WarmUp,
    Working,
}

#[cfg(feature = "uniffi")]
#[uniffi::remote(Enum)]
pub enum FatigueBand {
    Recovered,
    Low,
    Moderate,
    High,
    VeryHigh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum GymStarterLibrary {
    Written,
    NotNeeded,
    WaitingForDownload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymMuscleInfo {
    pub muscle: Muscle,
    pub display_name: String,
    pub group: MuscleGroup,
    pub default_recovery_days: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymMuscleShare {
    pub muscle: Muscle,
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymExercise {
    pub id: String,
    pub name: String,
    pub muscles: Vec<GymMuscleShare>,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymExerciseInput {
    pub name: String,
    pub muscles: Vec<GymMuscleShare>,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymPlannedExercise {
    pub exercise_id: String,
    pub warm_up_sets: u32,
    pub warm_up_rest_seconds: u32,
    pub target_sets: u32,
    pub target_reps: u32,
    pub target_reps_in_reserve: Option<u8>,
    pub rest_seconds: u32,
    pub superset_with_previous: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymTemplate {
    pub id: String,
    pub name: String,
    pub exercises: Vec<GymPlannedExercise>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymSet {
    pub id: String,
    pub exercise_id: String,
    pub order: u32,
    pub kind: SetKind,
    pub weight_kg: Option<f64>,
    pub reps: Option<u32>,
    pub reps_in_reserve: Option<u8>,
    pub completed_at_millis: f64,
    pub estimated_one_rep_max_kg: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymSessionExercise {
    pub exercise_id: String,
    pub name: String,
    pub planned: bool,
    pub plan_index: Option<u32>,
    pub warm_up_sets: u32,
    pub warm_up_rest_seconds: u32,
    pub target_sets: u32,
    pub target_reps: u32,
    pub target_reps_in_reserve: Option<u8>,
    pub rest_seconds: u32,
    pub superset_with_previous: bool,
    pub skipped: bool,
    pub done: bool,
    pub sets: Vec<GymSet>,
    pub last_time: Vec<GymSet>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymSetValues {
    pub weight_kg: Option<f64>,
    pub reps: Option<u32>,
    pub reps_in_reserve: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymRest {
    pub exercise_id: String,
    pub started_at_millis: f64,
    pub ends_at_millis: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymSession {
    pub id: String,
    pub name: String,
    pub started_at_millis: f64,
    pub ended_at_millis: Option<f64>,
    pub exercises: Vec<GymSessionExercise>,
    pub current_exercise_id: Option<String>,
    pub next_set_kind: Option<SetKind>,
    pub next_set_order: u32,
    pub prefill: Option<GymSetValues>,
    pub rest: Option<GymRest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymSessionSummary {
    pub id: String,
    pub name: String,
    pub started_at_millis: f64,
    pub ended_at_millis: Option<f64>,
    pub exercise_names: Vec<String>,
    pub working_sets: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymBodyWeight {
    pub id: String,
    pub time_millis: f64,
    pub kg: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymWeeklyBodyWeight {
    pub week_start: String,
    pub average_kg: f64,
    pub measurements: u32,
    pub change_kg: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymOneRepMax {
    pub session_id: String,
    pub started_at_millis: f64,
    pub kg: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GymMuscleFatigue {
    pub muscle: Muscle,
    pub display_name: String,
    pub group: MuscleGroup,
    pub score: u8,
    pub band: FatigueBand,
    pub recovery_days: f64,
    pub default_recovery_days: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum GymChange {
    SaveExercise {
        id: Option<String>,
        exercise: GymExerciseInput,
    },
    SaveTemplate {
        id: Option<String>,
        name: String,
        exercises: Vec<GymPlannedExercise>,
    },
    DeleteTemplate {
        id: String,
    },
    StartSession {
        template_id: Option<String>,
    },
    CompleteSet {
        session_id: String,
        exercise_id: String,
        kind: SetKind,
        expected_order: u32,
        values: GymSetValues,
    },
    AddSet {
        session_id: String,
        exercise_id: String,
        kind: SetKind,
        values: GymSetValues,
    },
    EditSet {
        set_id: String,
        values: GymSetValues,
    },
    DeleteSet {
        set_id: String,
    },
    DeleteSession {
        session_id: String,
    },
    AddExercise {
        session_id: String,
        exercise_id: String,
    },
    MoveExercise {
        session_id: String,
        exercise_id: String,
        to_index: u32,
    },
    SwitchExercise {
        session_id: String,
        exercise_id: String,
    },
    SkipExercise {
        session_id: String,
        exercise_id: String,
        skipped: bool,
    },
    AddWorkingSet {
        session_id: String,
        exercise_id: String,
    },
    AddWarmUpSet {
        session_id: String,
        exercise_id: String,
    },
    FinishSession {
        session_id: String,
    },
    AddBodyWeight {
        kg: f64,
    },
    DeleteBodyWeight {
        id: String,
    },
    SetRecoveryDays {
        muscle: Muscle,
        recovery_days: f64,
    },
}
