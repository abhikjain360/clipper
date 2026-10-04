use uuid::Uuid;

use crate::{
    DEFAULT_WARM_UP_REST_SECONDS, Exercise, Muscle, MuscleShare, WorkoutExercise, WorkoutTemplate,
};

pub const BENCH_PRESS: Uuid = starter_id(0x01);
pub const BACK_SQUAT: Uuid = starter_id(0x02);
pub const DEADLIFT: Uuid = starter_id(0x03);
pub const OVERHEAD_PRESS: Uuid = starter_id(0x04);
pub const BARBELL_ROW: Uuid = starter_id(0x05);
pub const ROMANIAN_DEADLIFT: Uuid = starter_id(0x06);
pub const PULL_UP: Uuid = starter_id(0x07);
pub const INCLINE_DUMBBELL_PRESS: Uuid = starter_id(0x08);
pub const DUMBBELL_SHOULDER_PRESS: Uuid = starter_id(0x09);
pub const ONE_ARM_DUMBBELL_ROW: Uuid = starter_id(0x0a);
pub const DUMBBELL_CURL: Uuid = starter_id(0x0b);
pub const DUMBBELL_LATERAL_RAISE: Uuid = starter_id(0x0c);
pub const LAT_PULLDOWN: Uuid = starter_id(0x0d);
pub const SEATED_CABLE_ROW: Uuid = starter_id(0x0e);
pub const LEG_PRESS: Uuid = starter_id(0x0f);
pub const LEG_CURL: Uuid = starter_id(0x10);
pub const LEG_EXTENSION: Uuid = starter_id(0x11);
pub const CALF_RAISE: Uuid = starter_id(0x12);
pub const TRICEPS_PUSHDOWN: Uuid = starter_id(0x13);
pub const CABLE_CRUNCH: Uuid = starter_id(0x14);
pub const HIP_THRUST: Uuid = starter_id(0x15);
pub const CHEST_FLY_MACHINE: Uuid = starter_id(0x16);

pub const FULL_BODY_A: Uuid = starter_id(0x101);
pub const FULL_BODY_B: Uuid = starter_id(0x102);

const fn starter_id(number: u128) -> Uuid {
    Uuid::from_u128(0x0199_5eed_0000_7000_8000_0000_0000_0000 + number)
}

pub fn starter_exercises() -> Vec<(Uuid, Exercise)> {
    use Muscle::*;
    [
        (
            BENCH_PRESS,
            "Barbell bench press",
            &[(Chest, 1.0), (Triceps, 0.5), (Shoulders, 0.4)][..],
        ),
        (
            BACK_SQUAT,
            "Barbell back squat",
            &[(Quadriceps, 1.0), (Glutes, 0.6), (Erectors, 0.3)],
        ),
        (
            DEADLIFT,
            "Barbell deadlift",
            &[
                (Erectors, 1.0),
                (Glutes, 0.8),
                (Hamstrings, 0.7),
                (UpperBack, 0.4),
                (Forearms, 0.4),
                (Quadriceps, 0.3),
            ],
        ),
        (
            OVERHEAD_PRESS,
            "Barbell overhead press",
            &[(Shoulders, 1.0), (Triceps, 0.6)],
        ),
        (
            BARBELL_ROW,
            "Barbell row",
            &[
                (UpperBack, 1.0),
                (Lats, 0.7),
                (Biceps, 0.4),
                (Erectors, 0.3),
            ],
        ),
        (
            ROMANIAN_DEADLIFT,
            "Romanian deadlift",
            &[(Hamstrings, 1.0), (Glutes, 0.6), (Erectors, 0.5)],
        ),
        (
            PULL_UP,
            "Pull-up",
            &[(Lats, 1.0), (Biceps, 0.5), (UpperBack, 0.4)],
        ),
        (
            INCLINE_DUMBBELL_PRESS,
            "Incline dumbbell press",
            &[(Chest, 1.0), (Shoulders, 0.5), (Triceps, 0.4)],
        ),
        (
            DUMBBELL_SHOULDER_PRESS,
            "Dumbbell shoulder press",
            &[(Shoulders, 1.0), (Triceps, 0.5)],
        ),
        (
            ONE_ARM_DUMBBELL_ROW,
            "One-arm dumbbell row",
            &[(Lats, 1.0), (UpperBack, 0.6), (Biceps, 0.4)],
        ),
        (
            DUMBBELL_CURL,
            "Dumbbell curl",
            &[(Biceps, 1.0), (Forearms, 0.3)],
        ),
        (
            DUMBBELL_LATERAL_RAISE,
            "Dumbbell lateral raise",
            &[(Shoulders, 1.0)],
        ),
        (
            LAT_PULLDOWN,
            "Lat pulldown",
            &[(Lats, 1.0), (Biceps, 0.4), (UpperBack, 0.3)],
        ),
        (
            SEATED_CABLE_ROW,
            "Seated cable row",
            &[(UpperBack, 1.0), (Lats, 0.6), (Biceps, 0.4)],
        ),
        (LEG_PRESS, "Leg press", &[(Quadriceps, 1.0), (Glutes, 0.5)]),
        (LEG_CURL, "Leg curl", &[(Hamstrings, 1.0)]),
        (LEG_EXTENSION, "Leg extension", &[(Quadriceps, 1.0)]),
        (CALF_RAISE, "Calf raise", &[(Calves, 1.0)]),
        (TRICEPS_PUSHDOWN, "Triceps pushdown", &[(Triceps, 1.0)]),
        (CABLE_CRUNCH, "Cable crunch", &[(Abs, 1.0), (Obliques, 0.3)]),
        (
            HIP_THRUST,
            "Barbell hip thrust",
            &[(Glutes, 1.0), (Hamstrings, 0.3)],
        ),
        (CHEST_FLY_MACHINE, "Chest fly machine", &[(Chest, 1.0)]),
    ]
    .into_iter()
    .map(|(id, name, muscles)| {
        (
            id,
            Exercise {
                name: name.into(),
                muscles: muscles
                    .iter()
                    .map(|(muscle, share)| MuscleShare {
                        muscle: *muscle,
                        share: *share,
                    })
                    .collect(),
                archived: false,
            },
        )
    })
    .collect()
}

pub fn starter_templates() -> Vec<(Uuid, WorkoutTemplate)> {
    vec![
        (
            FULL_BODY_A,
            WorkoutTemplate {
                name: "Full body A".into(),
                archived: false,
                exercises: vec![
                    planned(BACK_SQUAT, 2, 3, 5, 180, false),
                    planned(BENCH_PRESS, 2, 3, 5, 180, false),
                    planned(BARBELL_ROW, 1, 3, 8, 120, false),
                    planned(DUMBBELL_LATERAL_RAISE, 0, 3, 12, 90, false),
                    planned(DUMBBELL_CURL, 0, 3, 12, 90, true),
                ],
            },
        ),
        (
            FULL_BODY_B,
            WorkoutTemplate {
                name: "Full body B".into(),
                archived: false,
                exercises: vec![
                    planned(DEADLIFT, 2, 3, 5, 180, false),
                    planned(OVERHEAD_PRESS, 2, 3, 6, 150, false),
                    planned(LAT_PULLDOWN, 0, 3, 10, 90, false),
                    planned(LEG_CURL, 0, 3, 12, 90, false),
                    planned(TRICEPS_PUSHDOWN, 0, 3, 12, 90, true),
                ],
            },
        ),
    ]
}

fn planned(
    exercise_id: Uuid,
    warm_up_sets: u32,
    target_sets: u32,
    target_reps: u32,
    rest_seconds: u32,
    superset_with_previous: bool,
) -> WorkoutExercise {
    WorkoutExercise {
        exercise_id,
        warm_up_sets,
        warm_up_rest_seconds: DEFAULT_WARM_UP_REST_SECONDS,
        target_sets,
        target_reps,
        target_reps_in_reserve: Some(2),
        rest_seconds,
        superset_with_previous,
    }
}
