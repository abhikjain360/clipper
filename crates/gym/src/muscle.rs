use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MuscleGroup {
    Push,
    Pull,
    Legs,
    Core,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

impl Muscle {
    pub const ALL: [Self; 14] = [
        Self::Chest,
        Self::Shoulders,
        Self::Triceps,
        Self::Lats,
        Self::UpperBack,
        Self::Biceps,
        Self::Forearms,
        Self::Quadriceps,
        Self::Hamstrings,
        Self::Glutes,
        Self::Calves,
        Self::Abs,
        Self::Obliques,
        Self::Erectors,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Chest => "chest",
            Self::Shoulders => "shoulders",
            Self::Triceps => "triceps",
            Self::Lats => "lats",
            Self::UpperBack => "upper_back",
            Self::Biceps => "biceps",
            Self::Forearms => "forearms",
            Self::Quadriceps => "quadriceps",
            Self::Hamstrings => "hamstrings",
            Self::Glutes => "glutes",
            Self::Calves => "calves",
            Self::Abs => "abs",
            Self::Obliques => "obliques",
            Self::Erectors => "erectors",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Chest => "Chest",
            Self::Shoulders => "Shoulders",
            Self::Triceps => "Triceps",
            Self::Lats => "Lats",
            Self::UpperBack => "Upper Back",
            Self::Biceps => "Biceps",
            Self::Forearms => "Forearms",
            Self::Quadriceps => "Quadriceps",
            Self::Hamstrings => "Hamstrings",
            Self::Glutes => "Glutes",
            Self::Calves => "Calves",
            Self::Abs => "Abs",
            Self::Obliques => "Obliques",
            Self::Erectors => "Erectors",
        }
    }

    pub const fn group(self) -> MuscleGroup {
        match self {
            Self::Chest | Self::Shoulders | Self::Triceps => MuscleGroup::Push,
            Self::Lats | Self::UpperBack | Self::Biceps | Self::Forearms => MuscleGroup::Pull,
            Self::Quadriceps | Self::Hamstrings | Self::Glutes | Self::Calves => MuscleGroup::Legs,
            Self::Abs | Self::Obliques | Self::Erectors => MuscleGroup::Core,
        }
    }

    pub const fn default_recovery_days(self) -> f64 {
        match self {
            Self::Chest | Self::UpperBack => 2.5,
            Self::Shoulders | Self::Triceps | Self::Biceps | Self::Calves => 2.0,
            Self::Lats | Self::Hamstrings | Self::Erectors => 3.0,
            Self::Forearms | Self::Abs | Self::Obliques => 1.5,
            Self::Quadriceps | Self::Glutes => 3.5,
        }
    }
}

impl std::fmt::Display for Muscle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}
