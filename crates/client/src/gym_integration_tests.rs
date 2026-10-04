use clipper_app_types::{
    GymChange, GymExerciseInput, GymMuscleShare, GymPlannedExercise, GymSetValues, Muscle, SetKind,
};

use super::{app_data_integration_tests::signed_in, schedule_integration_tests::start_server};

fn exercise(name: &str, muscle: Muscle) -> GymExerciseInput {
    GymExerciseInput {
        name: name.into(),
        muscles: vec![GymMuscleShare { muscle, share: 1.0 }],
        archived: false,
    }
}

fn values(weight_kg: f64, reps: u32) -> GymSetValues {
    GymSetValues {
        weight_kg: Some(weight_kg),
        reps: Some(reps),
        reps_in_reserve: Some(2),
    }
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn a_set_added_after_a_workout_is_logged_at_the_workout_end() {
    crate::ensure_crypto_provider();
    let directory = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(directory.path()).await;
    let engine = signed_in(
        &format!("http://{address}"),
        directory.path(),
        "lifter",
        true,
    )
    .await;
    let bench = engine
        .gym_save_exercise(None, exercise("Bench press", Muscle::Chest))
        .await
        .unwrap();
    let row = engine
        .gym_save_exercise(None, exercise("Row", Muscle::UpperBack))
        .await
        .unwrap();
    let template = engine
        .gym_save_template(
            None,
            "Upper",
            vec![GymPlannedExercise {
                exercise_id: bench.clone(),
                warm_up_sets: 0,
                warm_up_rest_seconds: 60,
                target_sets: 3,
                target_reps: 8,
                target_reps_in_reserve: Some(2),
                rest_seconds: 120,
                superset_with_previous: false,
            }],
        )
        .await
        .unwrap();
    let session = engine.gym_start_session(Some(&template)).await.unwrap();
    engine
        .gym_complete_set(&session, &bench, SetKind::Working, 1, values(60.0, 8))
        .await
        .unwrap();
    engine
        .gym_change(GymChange::FinishSession {
            session_id: session.clone(),
        })
        .await
        .unwrap();
    engine
        .gym_change(GymChange::AddSet {
            session_id: session.clone(),
            exercise_id: row.clone(),
            kind: SetKind::Working,
            values: values(50.0, 10),
        })
        .await
        .unwrap();

    let sessions = engine.gym_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].working_sets, 2);
    assert_eq!(sessions[0].exercise_names, vec!["Bench press", "Row"]);
    let finished = engine.gym_session(&session).await.unwrap();
    let added = finished
        .exercises
        .iter()
        .find(|exercise| exercise.exercise_id == row)
        .unwrap();
    assert!(!added.planned);
    assert_eq!(added.sets.len(), 1);
    assert_eq!(added.sets[0].weight_kg, Some(50.0));
    assert_eq!(
        Some(added.sets[0].completed_at_millis),
        finished.ended_at_millis
    );
    assert!(engine.gym_open_session().await.unwrap().is_none());
}
