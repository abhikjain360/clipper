use clipper_app_types::{
    GymBodyWeight, GymChange, GymExercise, GymMuscleFatigue, GymMuscleInfo, GymOneRepMax,
    GymPlannedExercise, GymSession, GymSessionSummary, GymStarterLibrary, GymTemplate,
    GymWeeklyBodyWeight,
};
use clipper_daemon_types::{
    DaemonCommand, GymMoveTemplateExerciseParams, GymOneRepMaxProgressParams, GymOpenSessionResult,
    GymSessionParams, GymWeeklyBodyWeightParams,
};
use tauri::State;

use crate::{CommandResult, DesktopBackend};

#[tauri::command]
pub async fn gym_muscles(backend: State<'_, DesktopBackend>) -> CommandResult<Vec<GymMuscleInfo>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::GymMuscles)
        .await?)
}

#[tauri::command]
pub async fn gym_move_template_exercise(
    backend: State<'_, DesktopBackend>,
    exercises: Vec<GymPlannedExercise>,
    from: u32,
    to: u32,
) -> CommandResult<Vec<GymPlannedExercise>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::GymMoveTemplateExercise(
            GymMoveTemplateExerciseParams {
                exercises,
                from,
                to,
            },
        ))
        .await?)
}

#[tauri::command]
pub async fn gym_seed_starter_library(
    backend: State<'_, DesktopBackend>,
) -> CommandResult<GymStarterLibrary> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::GymSeedStarterLibrary)
        .await?)
}

#[tauri::command]
pub async fn gym_exercises(backend: State<'_, DesktopBackend>) -> CommandResult<Vec<GymExercise>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::GymExercises)
        .await?)
}

#[tauri::command]
pub async fn gym_templates(backend: State<'_, DesktopBackend>) -> CommandResult<Vec<GymTemplate>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::GymTemplates)
        .await?)
}

#[tauri::command]
pub async fn gym_open_session(
    backend: State<'_, DesktopBackend>,
) -> CommandResult<Option<GymSession>> {
    let result: GymOpenSessionResult = backend
        .daemon
        .send_result(DaemonCommand::GymOpenSession)
        .await?;
    Ok(result.session)
}

#[tauri::command]
pub async fn gym_session(
    backend: State<'_, DesktopBackend>,
    session_id: String,
) -> CommandResult<GymSession> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::GymSession(GymSessionParams { session_id }))
        .await?)
}

#[tauri::command]
pub async fn gym_sessions(
    backend: State<'_, DesktopBackend>,
) -> CommandResult<Vec<GymSessionSummary>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::GymSessions)
        .await?)
}

#[tauri::command]
pub async fn gym_body_weights(
    backend: State<'_, DesktopBackend>,
) -> CommandResult<Vec<GymBodyWeight>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::GymBodyWeights)
        .await?)
}

#[tauri::command]
pub async fn gym_weekly_body_weight(
    backend: State<'_, DesktopBackend>,
    zone: String,
) -> CommandResult<Vec<GymWeeklyBodyWeight>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::GymWeeklyBodyWeight(
            GymWeeklyBodyWeightParams { zone },
        ))
        .await?)
}

#[tauri::command]
pub async fn gym_one_rep_max_progress(
    backend: State<'_, DesktopBackend>,
    exercise_id: String,
) -> CommandResult<Vec<GymOneRepMax>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::GymOneRepMaxProgress(
            GymOneRepMaxProgressParams { exercise_id },
        ))
        .await?)
}

#[tauri::command]
pub async fn gym_fatigue(
    backend: State<'_, DesktopBackend>,
) -> CommandResult<Vec<GymMuscleFatigue>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::GymFatigue)
        .await?)
}

#[tauri::command]
pub async fn gym_change(
    backend: State<'_, DesktopBackend>,
    change: GymChange,
) -> CommandResult<()> {
    backend
        .daemon
        .send_ok(DaemonCommand::GymChange(change))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn gym_schedule_rest_end(ends_at_millis: f64, title: String, body: String) {
    #[cfg(target_os = "macos")]
    {
        if !crate::notifications::request_permission().await {
            return;
        }
        let seconds = (ends_at_millis - chrono::Utc::now().timestamp_millis() as f64) / 1000.0;
        if seconds > 0.0 {
            crate::notifications::schedule_rest_end(seconds, &title, &body);
        } else {
            crate::notifications::cancel_rest_end();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (ends_at_millis, title, body);
}

#[tauri::command]
pub fn gym_cancel_rest_end() {
    #[cfg(target_os = "macos")]
    crate::notifications::cancel_rest_end();
}
