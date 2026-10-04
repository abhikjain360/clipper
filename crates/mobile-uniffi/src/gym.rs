use clipper_app_types::{
    GymBodyWeight, GymExercise, GymExerciseInput, GymMuscleFatigue, GymMuscleInfo, GymOneRepMax,
    GymPlannedExercise, GymSession, GymSessionSummary, GymSetValues, GymStarterLibrary,
    GymTemplate, GymWeeklyBodyWeight, Muscle, SetKind,
};

use crate::{MobileClipperClient, MobileError};

uniffi::use_remote_type!(clipper_app_types::Muscle);
uniffi::use_remote_type!(clipper_app_types::SetKind);

#[uniffi::export(async_runtime = "tokio")]
impl MobileClipperClient {
    pub fn gym_move_template_exercise(
        &self,
        exercises: Vec<GymPlannedExercise>,
        from: u32,
        to: u32,
    ) -> Result<Vec<GymPlannedExercise>, MobileError> {
        Ok(self
            .engine
            .gym_move_template_exercise(exercises, from, to)?)
    }

    pub fn gym_muscles(&self) -> Vec<GymMuscleInfo> {
        self.engine.gym_muscles()
    }

    pub async fn gym_seed_starter_library(&self) -> Result<GymStarterLibrary, MobileError> {
        Ok(self.engine.gym_seed_starter_library().await?)
    }

    pub async fn gym_exercises(&self) -> Result<Vec<GymExercise>, MobileError> {
        Ok(self.engine.gym_exercises().await?)
    }

    pub async fn gym_save_exercise(
        &self,
        id: Option<String>,
        exercise: GymExerciseInput,
    ) -> Result<String, MobileError> {
        Ok(self
            .engine
            .gym_save_exercise(id.as_deref(), exercise)
            .await?)
    }

    pub async fn gym_templates(&self) -> Result<Vec<GymTemplate>, MobileError> {
        Ok(self.engine.gym_templates().await?)
    }

    pub async fn gym_save_template(
        &self,
        id: Option<String>,
        name: String,
        exercises: Vec<GymPlannedExercise>,
    ) -> Result<String, MobileError> {
        Ok(self
            .engine
            .gym_save_template(id.as_deref(), &name, exercises)
            .await?)
    }

    pub async fn gym_delete_template(&self, id: String) -> Result<(), MobileError> {
        Ok(self.engine.gym_delete_template(&id).await?)
    }

    pub async fn gym_open_session(&self) -> Result<Option<GymSession>, MobileError> {
        Ok(self.engine.gym_open_session().await?)
    }

    pub async fn gym_session(&self, session_id: String) -> Result<GymSession, MobileError> {
        Ok(self.engine.gym_session(&session_id).await?)
    }

    pub async fn gym_sessions(&self) -> Result<Vec<GymSessionSummary>, MobileError> {
        Ok(self.engine.gym_sessions().await?)
    }

    pub async fn gym_start_session(
        &self,
        template_id: Option<String>,
    ) -> Result<String, MobileError> {
        Ok(self
            .engine
            .gym_start_session(template_id.as_deref())
            .await?)
    }

    pub async fn gym_complete_set(
        &self,
        session_id: String,
        exercise_id: String,
        kind: SetKind,
        expected_order: u32,
        values: GymSetValues,
    ) -> Result<GymSession, MobileError> {
        Ok(self
            .engine
            .gym_complete_set(&session_id, &exercise_id, kind, expected_order, values)
            .await?)
    }

    pub async fn gym_edit_set(
        &self,
        set_id: String,
        values: GymSetValues,
    ) -> Result<(), MobileError> {
        Ok(self.engine.gym_edit_set(&set_id, values).await?)
    }

    pub async fn gym_delete_set(&self, set_id: String) -> Result<(), MobileError> {
        Ok(self.engine.gym_delete_set(&set_id).await?)
    }

    pub async fn gym_delete_session(&self, session_id: String) -> Result<(), MobileError> {
        Ok(self.engine.gym_delete_session(&session_id).await?)
    }

    pub async fn gym_add_exercise(
        &self,
        session_id: String,
        exercise_id: String,
    ) -> Result<(), MobileError> {
        Ok(self
            .engine
            .gym_add_exercise(&session_id, &exercise_id)
            .await?)
    }

    pub async fn gym_move_exercise(
        &self,
        session_id: String,
        exercise_id: String,
        to_index: u32,
    ) -> Result<(), MobileError> {
        Ok(self
            .engine
            .gym_move_exercise(&session_id, &exercise_id, to_index)
            .await?)
    }

    pub async fn gym_switch_exercise(
        &self,
        session_id: String,
        exercise_id: String,
    ) -> Result<(), MobileError> {
        Ok(self
            .engine
            .gym_switch_exercise(&session_id, &exercise_id)
            .await?)
    }

    pub async fn gym_skip_exercise(
        &self,
        session_id: String,
        exercise_id: String,
        skipped: bool,
    ) -> Result<(), MobileError> {
        Ok(self
            .engine
            .gym_skip_exercise(&session_id, &exercise_id, skipped)
            .await?)
    }

    pub async fn gym_add_working_set(
        &self,
        session_id: String,
        exercise_id: String,
    ) -> Result<(), MobileError> {
        Ok(self
            .engine
            .gym_add_working_set(&session_id, &exercise_id)
            .await?)
    }

    pub async fn gym_add_warm_up_set(
        &self,
        session_id: String,
        exercise_id: String,
    ) -> Result<(), MobileError> {
        Ok(self
            .engine
            .gym_add_warm_up_set(&session_id, &exercise_id)
            .await?)
    }

    pub async fn gym_finish_session(&self, session_id: String) -> Result<(), MobileError> {
        Ok(self.engine.gym_finish_session(&session_id).await?)
    }

    pub async fn gym_body_weights(&self) -> Result<Vec<GymBodyWeight>, MobileError> {
        Ok(self.engine.gym_body_weights().await?)
    }

    pub async fn gym_add_body_weight(&self, kg: f64) -> Result<String, MobileError> {
        Ok(self.engine.gym_add_body_weight(kg).await?)
    }

    pub async fn gym_delete_body_weight(&self, id: String) -> Result<(), MobileError> {
        Ok(self.engine.gym_delete_body_weight(&id).await?)
    }

    pub async fn gym_weekly_body_weight(
        &self,
        zone: String,
    ) -> Result<Vec<GymWeeklyBodyWeight>, MobileError> {
        Ok(self.engine.gym_weekly_body_weight(&zone).await?)
    }

    pub async fn gym_one_rep_max_progress(
        &self,
        exercise_id: String,
    ) -> Result<Vec<GymOneRepMax>, MobileError> {
        Ok(self.engine.gym_one_rep_max_progress(&exercise_id).await?)
    }

    pub async fn gym_fatigue(&self) -> Result<Vec<GymMuscleFatigue>, MobileError> {
        Ok(self.engine.gym_fatigue().await?)
    }

    pub async fn gym_set_recovery_days(
        &self,
        muscle: Muscle,
        recovery_days: f64,
    ) -> Result<(), MobileError> {
        Ok(self
            .engine
            .gym_set_recovery_days(muscle, recovery_days)
            .await?)
    }
}
