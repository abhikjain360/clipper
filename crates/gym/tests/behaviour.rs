use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, TimeDelta, TimeZone, Utc};
use clipper_gym::{
    BodyWeight, Exercise, FatigueBand, Muscle, MuscleFatigue, MuscleShare, Recovery, Session, Set,
    SetKind, fatigue_at, last_session_working_sets, weekly_body_weight,
};
use uuid::Uuid;

fn id(number: u128) -> Uuid {
    Uuid::from_u128(0x01900000_0000_7000_8000_000000000000 + number)
}

#[test]
fn workouts_default_to_active_and_older_readers_accept_archived_rows() {
    #[derive(serde::Deserialize)]
    struct OlderWorkout {
        name: String,
        exercises: Vec<clipper_gym::WorkoutExercise>,
    }
    let value = serde_json::json!({ "name": "Upper", "exercises": [] });
    let mut workout: clipper_gym::WorkoutTemplate = serde_json::from_value(value).unwrap();
    assert!(!workout.archived);
    workout.archived = true;
    workout.validate().unwrap();
    let older: OlderWorkout =
        serde_json::from_value(serde_json::to_value(&workout).unwrap()).unwrap();
    assert_eq!(older.name, workout.name);
    assert_eq!(older.exercises, workout.exercises);
}

fn time(month: u32, day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, month, day, 12, 0, 0).unwrap()
}

fn exercise() -> Exercise {
    Exercise {
        name: "Bench press".into(),
        muscles: vec![
            MuscleShare {
                muscle: Muscle::Chest,
                share: 1.0,
            },
            MuscleShare {
                muscle: Muscle::Triceps,
                share: 0.5,
            },
        ],
        archived: false,
    }
}

fn working_set(session_id: Uuid, completed_at: DateTime<Utc>) -> Set {
    Set {
        session_id,
        exercise_id: id(1),
        order: 0,
        kind: SetKind::Working,
        weight_kg: Some(60.0),
        reps: Some(8),
        reps_in_reserve: Some(0),
        completed_at,
    }
}

fn session(started_at: DateTime<Utc>) -> Session {
    Session {
        started_at,
        ended_at: Some(started_at + TimeDelta::hours(1)),
        template_id: None,
        notes: String::new(),
        exercises: Vec::new(),
        current_exercise_id: None,
    }
}

fn fatigue(
    now: DateTime<Utc>,
    sets: &[Set],
    recovery: &[Recovery],
    muscle: Muscle,
) -> MuscleFatigue {
    fatigue_at(
        now,
        &BTreeMap::from([(id(1), exercise())]),
        &BTreeMap::from([(id(2), session(now))]),
        sets,
        recovery,
    )
    .unwrap()
    .into_iter()
    .find(|row| row.muscle == muscle)
    .unwrap()
}

#[test]
fn fatigue_rises_with_recent_hard_sets_and_reaches_zero_after_recovery() {
    let completed = time(10, 7);
    let set = working_set(id(2), completed);
    let recent = fatigue(completed, std::slice::from_ref(&set), &[], Muscle::Chest);
    assert_eq!(recent.score, 15);
    assert_eq!(
        fatigue(completed, std::slice::from_ref(&set), &[], Muscle::Triceps).score,
        8
    );

    let sets = vec![set.clone(), set.clone()];
    assert_eq!(fatigue(completed, &sets, &[], Muscle::Chest).score, 30);
    assert_eq!(fatigue(completed, &sets, &[], Muscle::Triceps).score, 15);
    let halfway = completed + TimeDelta::hours(30);
    assert_eq!(fatigue(halfway, &sets, &[], Muscle::Chest).score, 15);
    let recovered = completed + TimeDelta::hours(60);
    assert_eq!(fatigue(recovered, &sets, &[], Muscle::Chest).score, 0);
    assert_eq!(
        fatigue(recovered + TimeDelta::days(1), &sets, &[], Muscle::Chest).score,
        0
    );

    let mut easier = set.clone();
    easier.reps_in_reserve = Some(8);
    assert_eq!(fatigue(completed, &[easier], &[], Muscle::Chest).score, 3);
    let mut unspecified = set.clone();
    unspecified.reps_in_reserve = None;
    assert_eq!(
        fatigue(completed, &[unspecified], &[], Muscle::Chest).score,
        12
    );
    let mut no_effort = set;
    no_effort.reps_in_reserve = Some(10);
    assert_eq!(
        fatigue(completed, &[no_effort], &[], Muscle::Chest).score,
        0
    );
}

#[test]
fn fatigue_ignores_warm_ups_future_old_and_orphaned_sets_and_caps_the_total() {
    let now = time(10, 7);
    let mut warm_up = working_set(id(2), now);
    warm_up.kind = SetKind::WarmUp;
    let ignored = [
        warm_up,
        working_set(id(2), now + TimeDelta::seconds(1)),
        working_set(id(2), now - TimeDelta::days(15)),
        working_set(id(20), now),
    ];
    assert_eq!(fatigue(now, &ignored, &[], Muscle::Chest).score, 0);
    let sets = vec![working_set(id(2), now); 8];
    let result = fatigue(now, &sets, &[], Muscle::Chest);
    assert_eq!(result.score, 100);
    assert_eq!(result.band, FatigueBand::VeryHigh);
}

#[test]
fn user_recovery_overrides_the_default_in_both_directions() {
    let completed = time(10, 1);
    let sets = [working_set(id(2), completed)];
    let now = completed + TimeDelta::days(3);
    assert_eq!(fatigue(now, &sets, &[], Muscle::Chest).score, 0);
    let longer = [Recovery {
        muscle: Muscle::Chest,
        recovery_days: 6.0,
    }];
    let overridden = fatigue(now, &sets, &longer, Muscle::Chest);
    assert_eq!(overridden.score, 8);
    assert_eq!(overridden.recovery_days, 6.0);
    assert_eq!(
        fatigue(
            completed + TimeDelta::days(6),
            &sets,
            &longer,
            Muscle::Chest
        )
        .score,
        0
    );

    let shorter = [Recovery {
        muscle: Muscle::Chest,
        recovery_days: 0.5,
    }];
    let now = completed + TimeDelta::hours(12);
    assert!(fatigue(now, &sets, &[], Muscle::Chest).score > 0);
    assert_eq!(fatigue(now, &sets, &shorter, Muscle::Chest).score, 0);
    assert!(fatigue(now, &sets, &shorter, Muscle::Triceps).score > 0);
}

#[test]
fn fatigue_bands_include_their_upper_limits() {
    for (scores, band) in [
        ([0, 20], FatigueBand::Recovered),
        ([21, 40], FatigueBand::Low),
        ([41, 60], FatigueBand::Moderate),
        ([61, 80], FatigueBand::High),
        ([81, 100], FatigueBand::VeryHigh),
    ] {
        for score in scores {
            assert_eq!(FatigueBand::from_score(score), band);
        }
    }
}

#[test]
fn last_session_lookup_uses_session_start_and_returns_ordered_working_sets() {
    let older_id = id(6);
    let newer_id = id(2);
    let warm_up_only_id = id(7);
    let other_exercise_id = id(8);
    let empty_id = id(9);
    let sessions = BTreeMap::from([
        (newer_id, session(time(10, 5))),
        (older_id, session(time(10, 3))),
        (warm_up_only_id, session(time(10, 6))),
        (other_exercise_id, session(time(10, 7))),
        (empty_id, session(time(10, 8))),
    ]);
    let mut second = working_set(newer_id, time(10, 5));
    second.order = 2;
    second.weight_kg = Some(65.0);
    let mut first = working_set(newer_id, time(10, 5));
    first.order = 1;
    let mut warm_up = working_set(newer_id, time(10, 5));
    warm_up.kind = SetKind::WarmUp;
    let mut warm_up_only = working_set(warm_up_only_id, time(10, 6));
    warm_up_only.kind = SetKind::WarmUp;
    let mut other_exercise = working_set(other_exercise_id, time(10, 7));
    other_exercise.exercise_id = id(10);
    let sets = vec![
        second.clone(),
        working_set(older_id, time(10, 9)),
        warm_up,
        first.clone(),
        warm_up_only,
        other_exercise,
        working_set(id(11), time(10, 10)),
    ];
    let last = last_session_working_sets(id(1), &sessions, &sets).unwrap();
    assert_eq!(last.session_id, newer_id);
    assert_eq!(last.sets, vec![first, second]);
    assert!(last_session_working_sets(id(12), &sessions, &sets).is_none());
}

#[test]
fn weekly_averages_use_monday_boundaries_and_consecutive_week_changes() {
    let entries = [
        BodyWeight {
            time: time(10, 6),
            kg: 83.0,
        },
        BodyWeight {
            time: time(10, 4),
            kg: 82.0,
        },
        BodyWeight {
            time: time(10, 19),
            kg: 79.0,
        },
        BodyWeight {
            time: time(9, 28),
            kg: 80.0,
        },
        BodyWeight {
            time: time(10, 5),
            kg: 81.0,
        },
        BodyWeight {
            time: time(10, 26),
            kg: 78.0,
        },
    ];
    let weeks = weekly_body_weight(&entries, chrono_tz::UTC).unwrap();
    assert_eq!(weeks.len(), 4);
    assert_eq!(
        weeks[0].week_start,
        NaiveDate::from_ymd_opt(2026, 9, 28).unwrap()
    );
    assert_eq!(weeks[0].average_kg, 81.0);
    assert_eq!(weeks[0].measurements, 2);
    assert_eq!(weeks[0].change_kg, None);
    assert_eq!(
        weeks[1].week_start,
        NaiveDate::from_ymd_opt(2026, 10, 5).unwrap()
    );
    assert_eq!(weeks[1].average_kg, 82.0);
    assert_eq!(weeks[1].measurements, 2);
    assert_eq!(weeks[1].change_kg, Some(1.0));
    assert_eq!(weeks[2].average_kg, 79.0);
    assert_eq!(weeks[2].change_kg, None);
    assert_eq!(weeks[3].average_kg, 78.0);
    assert_eq!(weeks[3].change_kg, Some(-1.0));
    assert!(weekly_body_weight(&[], chrono_tz::UTC).unwrap().is_empty());
}

#[test]
fn weekly_averages_cross_years_without_splitting_a_week() {
    let entries = [
        BodyWeight {
            time: Utc.with_ymd_and_hms(2027, 1, 3, 23, 59, 59).unwrap(),
            kg: 82.0,
        },
        BodyWeight {
            time: Utc.with_ymd_and_hms(2026, 12, 31, 12, 0, 0).unwrap(),
            kg: 80.0,
        },
        BodyWeight {
            time: Utc.with_ymd_and_hms(2027, 1, 4, 0, 0, 0).unwrap(),
            kg: 83.0,
        },
    ];
    let weeks = weekly_body_weight(&entries, chrono_tz::UTC).unwrap();
    assert_eq!(weeks.len(), 2);
    assert_eq!(
        weeks[0].week_start,
        NaiveDate::from_ymd_opt(2026, 12, 28).unwrap()
    );
    assert_eq!(weeks[0].average_kg, 81.0);
    assert_eq!(
        weeks[1].week_start,
        NaiveDate::from_ymd_opt(2027, 1, 4).unwrap()
    );
    assert_eq!(weeks[1].average_kg, 83.0);
    assert_eq!(weeks[1].change_kg, Some(2.0));
}

#[test]
fn weekly_averages_follow_the_local_calendar_week() {
    let sunday_evening_in_new_york = BodyWeight {
        time: Utc.with_ymd_and_hms(2026, 10, 5, 2, 0, 0).unwrap(),
        kg: 80.0,
    };
    let local = weekly_body_weight(
        std::slice::from_ref(&sunday_evening_in_new_york),
        chrono_tz::America::New_York,
    )
    .unwrap();
    assert_eq!(
        local[0].week_start,
        NaiveDate::from_ymd_opt(2026, 9, 28).unwrap()
    );
    let utc = weekly_body_weight(&[sunday_evening_in_new_york], chrono_tz::UTC).unwrap();
    assert_eq!(
        utc[0].week_start,
        NaiveDate::from_ymd_opt(2026, 10, 5).unwrap()
    );
}
