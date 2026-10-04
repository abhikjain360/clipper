use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use clipper_gym::{
    LoggedSet, RestTimer, Session, SessionChangeError, SessionProgress, Set, SetKind,
    WorkoutExercise, WorkoutTemplate,
};
use uuid::Uuid;

fn id(number: u128) -> Uuid {
    Uuid::from_u128(0x01900000_0000_7000_8000_000000000000 + number)
}

fn time(seconds: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 7, 18, 0, 0).unwrap() + TimeDelta::seconds(seconds)
}

const SESSION: u128 = 100;
const SQUAT: u128 = 1;
const BENCH: u128 = 2;
const ROW: u128 = 3;

fn planned(
    exercise: u128,
    warm_up_sets: u32,
    target_sets: u32,
    rest_seconds: u32,
    superset_with_previous: bool,
) -> WorkoutExercise {
    WorkoutExercise {
        exercise_id: id(exercise),
        warm_up_sets,
        target_sets,
        target_reps: 5,
        target_reps_in_reserve: Some(2),
        rest_seconds,
        superset_with_previous,
    }
}

fn session(exercises: Vec<WorkoutExercise>) -> Session {
    let template = WorkoutTemplate {
        name: "Full body".into(),
        exercises,
    };
    template.validate().unwrap();
    Session::from_template(time(0), id(200), &template)
}

fn logged(row: u128, exercise: u128, order: u32, kind: SetKind, completed_at: i64) -> LoggedSet {
    LoggedSet {
        id: id(row),
        set: Set {
            session_id: id(SESSION),
            exercise_id: id(exercise),
            order,
            kind,
            weight_kg: Some(100.0),
            reps: Some(5),
            reps_in_reserve: Some(2),
            completed_at: time(completed_at),
        },
    }
}

fn progress(session: &Session, sets: &[LoggedSet]) -> SessionProgress {
    SessionProgress::new(session, id(SESSION), sets)
}

fn current(progress: &SessionProgress) -> Option<(Uuid, SetKind)> {
    progress
        .current_exercise()
        .map(|exercise| (exercise.plan.exercise_id, exercise.next_kind()))
}

#[test]
fn sets_appear_in_stored_order_and_the_next_set_follows_from_stored_rows() {
    let session = session(vec![
        planned(SQUAT, 1, 2, 180, false),
        planned(BENCH, 0, 1, 180, false),
    ]);
    let empty = progress(&session, &[]);
    assert_eq!(current(&empty), Some((id(SQUAT), SetKind::WarmUp)));
    assert_eq!(empty.next_order, 1);

    let warm_up = logged(10, SQUAT, 1, SetKind::WarmUp, 60);
    let first = logged(11, SQUAT, 2, SetKind::Working, 300);
    let second = logged(12, SQUAT, 3, SetKind::Working, 300);
    let after_warm_up = progress(&session, std::slice::from_ref(&warm_up));
    assert_eq!(current(&after_warm_up), Some((id(SQUAT), SetKind::Working)));
    assert_eq!(after_warm_up.next_order, 2);

    let stored = [second.clone(), warm_up.clone(), first.clone()];
    let done = progress(&session, &stored);
    let squat_sets: Vec<Uuid> = done.exercises[0]
        .sets
        .iter()
        .map(|logged| logged.id)
        .collect();
    assert_eq!(squat_sets, [warm_up.id, first.id, second.id]);
    assert_eq!(done.exercises[0].working_done, 2);
    assert_eq!(current(&done), Some((id(BENCH), SetKind::Working)));
    assert_eq!(done.next_order, 4);

    let earlier_order_logged_later = logged(13, SQUAT, 1, SetKind::Working, 900);
    let reordered = progress(
        &session,
        &[first.clone(), earlier_order_logged_later.clone()],
    );
    let order: Vec<Uuid> = reordered.exercises[0]
        .sets
        .iter()
        .map(|logged| logged.id)
        .collect();
    assert_eq!(order, [earlier_order_logged_later.id, first.id]);
}

#[test]
fn rest_runs_from_the_stored_completion_time_and_survives_a_reload() {
    let session = session(vec![
        planned(SQUAT, 0, 3, 180, false),
        planned(BENCH, 0, 2, 60, false),
        planned(ROW, 0, 2, 90, true),
    ]);
    let squat = logged(10, SQUAT, 1, SetKind::Working, 600);
    let restored = progress(&session, std::slice::from_ref(&squat));
    assert_eq!(
        restored.rest,
        Some(RestTimer {
            exercise_id: id(SQUAT),
            started_at: time(600),
            ends_at: time(780),
        })
    );

    let session_json = serde_json::to_string(&session).unwrap();
    let set_json = serde_json::to_string(&squat.set).unwrap();
    let reloaded_session: Session = serde_json::from_str(&session_json).unwrap();
    let reloaded_set = LoggedSet {
        id: squat.id,
        set: serde_json::from_str(&set_json).unwrap(),
    };
    assert_eq!(progress(&reloaded_session, &[reloaded_set]), restored);

    let squats_done = [
        squat.clone(),
        logged(11, SQUAT, 2, SetKind::Working, 800),
        logged(12, SQUAT, 3, SetKind::Working, 1000),
    ];
    let bench = logged(13, BENCH, 4, SetKind::Working, 1100);
    let mut sets = squats_done.to_vec();
    sets.push(bench);
    let into_superset = progress(&session, &sets);
    assert_eq!(current(&into_superset), Some((id(ROW), SetKind::Working)));
    assert_eq!(into_superset.rest, None);

    sets.push(logged(14, ROW, 5, SetKind::Working, 1150));
    let round_done = progress(&session, &sets);
    assert_eq!(current(&round_done), Some((id(BENCH), SetKind::Working)));
    assert_eq!(
        round_done.rest.map(|rest| (rest.started_at, rest.ends_at)),
        Some((time(1150), time(1240)))
    );

    let mut finished = session.clone();
    finished.finish(time(1200)).unwrap();
    let after_finish = progress(&finished, &sets);
    assert_eq!(after_finish.current, None);
    assert_eq!(after_finish.rest, None);

    sets.push(logged(15, BENCH, 6, SetKind::Working, 1300));
    sets.push(logged(16, ROW, 7, SetKind::Working, 1350));
    let all_done = progress(&session, &sets);
    assert_eq!(all_done.current, None);
    assert_eq!(all_done.rest, None);
}

#[test]
fn skipping_and_moving_exercises_changes_which_exercise_is_next() {
    let mut session = session(vec![
        planned(SQUAT, 0, 1, 120, false),
        planned(BENCH, 0, 1, 120, false),
        planned(ROW, 0, 1, 120, false),
    ]);
    session.set_skipped(id(SQUAT), true).unwrap();
    assert_eq!(
        current(&progress(&session, &[])),
        Some((id(BENCH), SetKind::Working))
    );

    session.move_exercise(id(ROW), 0).unwrap();
    assert_eq!(
        current(&progress(&session, &[])),
        Some((id(ROW), SetKind::Working))
    );

    let row = logged(10, ROW, 1, SetKind::Working, 60);
    session.add_working_set(id(ROW)).unwrap();
    assert_eq!(
        current(&progress(&session, std::slice::from_ref(&row))),
        Some((id(ROW), SetKind::Working))
    );

    session.add_exercise(id(4)).unwrap();
    assert_eq!(session.exercises.last().unwrap().exercise_id, id(4));
    session.validate().unwrap();

    session.finish(time(600)).unwrap();
    assert_eq!(
        session.add_working_set(id(BENCH)),
        Err(SessionChangeError::Finished)
    );
}
