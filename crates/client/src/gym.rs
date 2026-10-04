use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use clipper_app_types::{
    GymBodyWeight, GymChange, GymExercise, GymExerciseInput, GymMuscleFatigue, GymMuscleInfo,
    GymMuscleShare, GymOneRepMax, GymPlannedExercise, GymRest, GymSession, GymSessionExercise,
    GymSessionSummary, GymSet, GymSetValues, GymStarterLibrary, GymTemplate, GymWeeklyBodyWeight,
};
use clipper_gym::{
    BodyWeight, Exercise, LoggedSet, Muscle, MuscleShare, Recovery, Session, SessionChangeError,
    SessionProgress, Set, SetKind, WorkoutExercise, WorkoutTemplate, best_one_rep_max_by_session,
    estimated_one_rep_max, fatigue_at, last_session_working_sets, starter_exercises,
    starter_templates, weekly_body_weight,
};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

use super::*;

static GYM_WRITES: Mutex<()> = Mutex::const_new(());

impl SyncEngine {
    pub fn gym_move_template_exercise(
        &self,
        exercises: Vec<GymPlannedExercise>,
        from: u32,
        to: u32,
    ) -> Result<Vec<GymPlannedExercise>, ClientError> {
        let mut template = WorkoutTemplate {
            name: String::new(),
            exercises: exercises
                .into_iter()
                .map(planned_value)
                .collect::<Result<_, _>>()?,
        };
        template.move_exercise(
            usize::try_from(from).unwrap_or(usize::MAX),
            usize::try_from(to).unwrap_or(usize::MAX),
        );
        Ok(template.exercises.into_iter().map(planned_view).collect())
    }

    pub fn gym_muscles(&self) -> Vec<GymMuscleInfo> {
        Muscle::ALL
            .into_iter()
            .map(|muscle| GymMuscleInfo {
                muscle,
                display_name: muscle.display_name().to_string(),
                group: muscle.group(),
                default_recovery_days: muscle.default_recovery_days(),
            })
            .collect()
    }

    pub async fn gym_seed_starter_library(&self) -> Result<GymStarterLibrary, ClientError> {
        let _writing = GYM_WRITES.lock().await;
        if !self.app_data_downloaded() {
            return Ok(GymStarterLibrary::WaitingForDownload);
        }
        let existing = self
            .query_app_data(
                "SELECT id FROM gym.exercises UNION ALL SELECT id FROM gym.workouts \
                 UNION ALL SELECT id FROM gym.sessions UNION ALL SELECT id FROM gym.sets LIMIT 1",
            )
            .await?;
        if !existing.is_empty() {
            return Ok(GymStarterLibrary::NotNeeded);
        }
        for (id, exercise) in starter_exercises() {
            self.gym_put(Exercise::COLLECTION_NAME, Some(id), &exercise)
                .await?;
        }
        for (id, template) in starter_templates() {
            self.gym_put(WorkoutTemplate::COLLECTION_NAME, Some(id), &template)
                .await?;
        }
        Ok(GymStarterLibrary::Written)
    }

    pub async fn gym_exercises(&self) -> Result<Vec<GymExercise>, ClientError> {
        let mut exercises: Vec<GymExercise> = self
            .gym_rows::<Exercise>("SELECT id, value FROM gym.exercises")
            .await?
            .into_iter()
            .map(|(id, exercise)| GymExercise {
                id: id.to_string(),
                name: exercise.name,
                muscles: exercise
                    .muscles
                    .into_iter()
                    .map(|target| GymMuscleShare {
                        muscle: target.muscle,
                        share: target.share,
                    })
                    .collect(),
                archived: exercise.archived,
            })
            .collect();
        exercises.sort_by_key(|exercise| exercise.name.to_lowercase());
        Ok(exercises)
    }

    pub async fn gym_save_exercise(
        &self,
        id: Option<&str>,
        exercise: GymExerciseInput,
    ) -> Result<String, ClientError> {
        let id = id.map(parse_gym_id).transpose()?;
        let exercise = Exercise {
            name: exercise.name.trim().to_string(),
            muscles: exercise
                .muscles
                .into_iter()
                .map(|target| MuscleShare {
                    muscle: target.muscle,
                    share: target.share,
                })
                .collect(),
            archived: exercise.archived,
        };
        Ok(self
            .gym_put(Exercise::COLLECTION_NAME, id, &exercise)
            .await?
            .to_string())
    }

    pub async fn gym_templates(&self) -> Result<Vec<GymTemplate>, ClientError> {
        let mut templates: Vec<GymTemplate> = self
            .gym_rows::<WorkoutTemplate>("SELECT id, value FROM gym.workouts")
            .await?
            .into_iter()
            .map(|(id, template)| GymTemplate {
                id: id.to_string(),
                name: template.name,
                exercises: template.exercises.into_iter().map(planned_view).collect(),
            })
            .collect();
        templates.sort_by_key(|template| template.name.to_lowercase());
        Ok(templates)
    }

    pub async fn gym_save_template(
        &self,
        id: Option<&str>,
        name: &str,
        exercises: Vec<GymPlannedExercise>,
    ) -> Result<String, ClientError> {
        let id = id.map(parse_gym_id).transpose()?;
        let template = WorkoutTemplate {
            name: name.trim().to_string(),
            exercises: exercises
                .into_iter()
                .map(planned_value)
                .collect::<Result<_, _>>()?,
        };
        Ok(self
            .gym_put(WorkoutTemplate::COLLECTION_NAME, id, &template)
            .await?
            .to_string())
    }

    pub async fn gym_delete_template(&self, id: &str) -> Result<(), ClientError> {
        self.gym_delete(WorkoutTemplate::COLLECTION_NAME, parse_gym_id(id)?)
            .await
    }

    pub async fn gym_open_session(&self) -> Result<Option<GymSession>, ClientError> {
        let sessions = self.gym_stored_sessions().await?;
        let Some((id, session)) = open_session(&sessions) else {
            return Ok(None);
        };
        Ok(Some(
            self.gym_session_view(id, session.clone(), &sessions)
                .await?,
        ))
    }

    pub async fn gym_session(&self, session_id: &str) -> Result<GymSession, ClientError> {
        let id = parse_gym_id(session_id)?;
        let sessions = self.gym_stored_sessions().await?;
        let session = sessions
            .get(&id)
            .cloned()
            .ok_or_else(|| ClientError::Other("this session no longer exists".into()))?;
        self.gym_session_view(id, session, &sessions).await
    }

    pub async fn gym_sessions(&self) -> Result<Vec<GymSessionSummary>, ClientError> {
        let sessions = self.gym_stored_sessions().await?;
        let names = self.gym_exercise_names().await?;
        let templates = self.gym_template_names().await?;
        let counts: BTreeMap<String, u32> = self
            .query_app_data(
                "SELECT json_extract(value, '$.session_id') AS session_id, count(*) AS working \
                 FROM gym.sets WHERE json_extract(value, '$.kind') = 'working' GROUP BY 1",
            )
            .await?
            .into_iter()
            .filter_map(|row| {
                let session_id = row.get("session_id")?.as_str()?.to_string();
                let working = u32::try_from(row.get("working")?.as_u64()?).ok()?;
                Some((session_id, working))
            })
            .collect();
        let mut summaries: Vec<GymSessionSummary> = sessions
            .iter()
            .map(|(id, session)| GymSessionSummary {
                id: id.to_string(),
                name: session_name(session, &templates),
                started_at_millis: millis(session.started_at),
                ended_at_millis: session.ended_at.map(millis),
                exercise_names: session
                    .exercises
                    .iter()
                    .filter(|planned| !planned.skipped)
                    .map(|planned| exercise_name(&names, planned.exercise_id))
                    .collect(),
                working_sets: counts.get(&id.to_string()).copied().unwrap_or(0),
            })
            .collect();
        summaries.sort_by(|a, b| b.started_at_millis.total_cmp(&a.started_at_millis));
        Ok(summaries)
    }

    pub async fn gym_start_session(
        &self,
        template_id: Option<&str>,
    ) -> Result<String, ClientError> {
        let _writing = GYM_WRITES.lock().await;
        let sessions = self.gym_stored_sessions().await?;
        if let Some((id, _)) = open_session(&sessions) {
            return Ok(id.to_string());
        }
        let now = Utc::now();
        let session = match template_id {
            Some(template_id) => {
                let template_id = parse_gym_id(template_id)?;
                let template: WorkoutTemplate = self
                    .gym_row(WorkoutTemplate::COLLECTION_NAME, template_id)
                    .await?;
                Session::from_template(now, template_id, &template)
            }
            None => Session::blank(now),
        };
        Ok(self
            .gym_put(Session::COLLECTION_NAME, None, &session)
            .await?
            .to_string())
    }

    pub async fn gym_complete_set(
        &self,
        session_id: &str,
        exercise_id: &str,
        kind: SetKind,
        expected_order: u32,
        values: GymSetValues,
    ) -> Result<GymSession, ClientError> {
        let writing = GYM_WRITES.lock().await;
        let session_id = parse_gym_id(session_id)?;
        let exercise_id = parse_gym_id(exercise_id)?;
        let session: Session = self.gym_row(Session::COLLECTION_NAME, session_id).await?;
        let sets = self.gym_session_sets(session_id).await?;
        let progress = SessionProgress::new(&session, session_id, &sets);
        let current = progress.current_exercise();
        if progress.next_order != expected_order
            || current.is_none_or(|current| {
                current.plan.exercise_id != exercise_id || current.next_kind() != kind
            })
        {
            return Err(ClientError::Other(
                "the session changed since this screen was shown; check the sets and try again"
                    .into(),
            ));
        }
        let set = Set {
            session_id,
            exercise_id,
            order: self
                .gym_free_set_order(session_id, progress.next_order)
                .await?,
            kind,
            weight_kg: values.weight_kg,
            reps: values.reps,
            reps_in_reserve: values.reps_in_reserve,
            completed_at: Utc::now(),
        };
        self.gym_put(Set::COLLECTION_NAME, None, &set).await?;
        drop(writing);
        let sessions = self.gym_stored_sessions().await?;
        self.gym_session_view(session_id, session, &sessions).await
    }

    pub async fn gym_add_set(
        &self,
        session_id: &str,
        exercise_id: &str,
        kind: SetKind,
        values: GymSetValues,
    ) -> Result<(), ClientError> {
        let _writing = GYM_WRITES.lock().await;
        let session_id = parse_gym_id(session_id)?;
        let exercise_id = parse_gym_id(exercise_id)?;
        let session: Session = self.gym_row(Session::COLLECTION_NAME, session_id).await?;
        let sets = self.gym_session_sets(session_id).await?;
        let progress = SessionProgress::new(&session, session_id, &sets);
        let set = Set {
            session_id,
            exercise_id,
            order: self
                .gym_free_set_order(session_id, progress.next_order)
                .await?,
            kind,
            weight_kg: values.weight_kg,
            reps: values.reps,
            reps_in_reserve: values.reps_in_reserve,
            completed_at: session.ended_at.unwrap_or_else(Utc::now),
        };
        self.gym_put(Set::COLLECTION_NAME, None, &set).await?;
        Ok(())
    }

    pub async fn gym_edit_set(
        &self,
        set_id: &str,
        values: GymSetValues,
    ) -> Result<(), ClientError> {
        let _writing = GYM_WRITES.lock().await;
        let id = parse_gym_id(set_id)?;
        let mut set: Set = self.gym_row(Set::COLLECTION_NAME, id).await?;
        set.weight_kg = values.weight_kg;
        set.reps = values.reps;
        set.reps_in_reserve = values.reps_in_reserve;
        self.gym_put(Set::COLLECTION_NAME, Some(id), &set).await?;
        Ok(())
    }

    pub async fn gym_delete_set(&self, set_id: &str) -> Result<(), ClientError> {
        let _writing = GYM_WRITES.lock().await;
        self.gym_delete(Set::COLLECTION_NAME, parse_gym_id(set_id)?)
            .await
    }

    pub async fn gym_delete_session(&self, session_id: &str) -> Result<(), ClientError> {
        let _writing = GYM_WRITES.lock().await;
        let id = parse_gym_id(session_id)?;
        for logged in self.gym_session_sets(id).await? {
            self.gym_delete(Set::COLLECTION_NAME, logged.id).await?;
        }
        self.gym_delete(Session::COLLECTION_NAME, id).await
    }

    pub async fn gym_add_exercise(
        &self,
        session_id: &str,
        exercise_id: &str,
    ) -> Result<(), ClientError> {
        let exercise_id = parse_gym_id(exercise_id)?;
        self.gym_change_session(session_id, |session| session.add_exercise(exercise_id))
            .await
    }

    pub async fn gym_move_exercise(
        &self,
        session_id: &str,
        exercise_id: &str,
        to_index: u32,
    ) -> Result<(), ClientError> {
        let exercise_id = parse_gym_id(exercise_id)?;
        let to = usize::try_from(to_index).unwrap_or(usize::MAX);
        self.gym_change_session(session_id, |session| session.move_exercise(exercise_id, to))
            .await
    }

    pub async fn gym_switch_exercise(
        &self,
        session_id: &str,
        exercise_id: &str,
    ) -> Result<(), ClientError> {
        let exercise_id = parse_gym_id(exercise_id)?;
        self.gym_change_session(session_id, |session| session.switch_to(exercise_id))
            .await
    }

    pub async fn gym_skip_exercise(
        &self,
        session_id: &str,
        exercise_id: &str,
        skipped: bool,
    ) -> Result<(), ClientError> {
        let exercise_id = parse_gym_id(exercise_id)?;
        self.gym_change_session(session_id, |session| {
            session.set_skipped(exercise_id, skipped)
        })
        .await
    }

    pub async fn gym_add_working_set(
        &self,
        session_id: &str,
        exercise_id: &str,
    ) -> Result<(), ClientError> {
        let exercise_id = parse_gym_id(exercise_id)?;
        self.gym_change_session(session_id, |session| session.add_working_set(exercise_id))
            .await
    }

    pub async fn gym_add_warm_up_set(
        &self,
        session_id: &str,
        exercise_id: &str,
    ) -> Result<(), ClientError> {
        let exercise_id = parse_gym_id(exercise_id)?;
        self.gym_change_session(session_id, |session| session.add_warm_up_set(exercise_id))
            .await
    }

    pub async fn gym_finish_session(&self, session_id: &str) -> Result<(), ClientError> {
        self.gym_change_session(session_id, |session| session.finish(Utc::now()))
            .await
    }

    pub async fn gym_body_weights(&self) -> Result<Vec<GymBodyWeight>, ClientError> {
        let mut entries: Vec<GymBodyWeight> = self
            .gym_rows::<BodyWeight>("SELECT id, value FROM gym.body_weight")
            .await?
            .into_iter()
            .map(|(id, entry)| GymBodyWeight {
                id: id.to_string(),
                time_millis: millis(entry.time),
                kg: entry.kg,
            })
            .collect();
        entries.sort_by(|a, b| b.time_millis.total_cmp(&a.time_millis));
        Ok(entries)
    }

    pub async fn gym_add_body_weight(&self, kg: f64) -> Result<String, ClientError> {
        let entry = BodyWeight {
            time: Utc::now(),
            kg,
        };
        Ok(self
            .gym_put(BodyWeight::COLLECTION_NAME, None, &entry)
            .await?
            .to_string())
    }

    pub async fn gym_delete_body_weight(&self, id: &str) -> Result<(), ClientError> {
        self.gym_delete(BodyWeight::COLLECTION_NAME, parse_gym_id(id)?)
            .await
    }

    pub async fn gym_weekly_body_weight(
        &self,
        zone: &str,
    ) -> Result<Vec<GymWeeklyBodyWeight>, ClientError> {
        let zone: Tz = zone.parse().map_err(|_| {
            ClientError::InvalidArgument(format!("{zone} is not a known time zone"))
        })?;
        let entries: Vec<BodyWeight> = self
            .gym_rows::<BodyWeight>("SELECT id, value FROM gym.body_weight")
            .await?
            .into_iter()
            .map(|(_, entry)| entry)
            .collect();
        Ok(weekly_body_weight(&entries, zone)
            .map_err(|error| ClientError::Other(error.to_string()))?
            .into_iter()
            .map(|week| GymWeeklyBodyWeight {
                week_start: week.week_start.to_string(),
                average_kg: week.average_kg,
                measurements: u32::try_from(week.measurements).unwrap_or(u32::MAX),
                change_kg: week.change_kg,
            })
            .collect())
    }

    pub async fn gym_one_rep_max_progress(
        &self,
        exercise_id: &str,
    ) -> Result<Vec<GymOneRepMax>, ClientError> {
        let exercise_id = parse_gym_id(exercise_id)?;
        let sessions = self.gym_stored_sessions().await?;
        let sets = self.gym_working_sets_of(&[exercise_id]).await?;
        Ok(best_one_rep_max_by_session(exercise_id, &sessions, &sets)
            .into_iter()
            .map(|best| GymOneRepMax {
                session_id: best.session_id.to_string(),
                started_at_millis: millis(best.started_at),
                kg: best.kg,
            })
            .collect())
    }

    pub async fn gym_fatigue(&self) -> Result<Vec<GymMuscleFatigue>, ClientError> {
        let exercises: BTreeMap<Uuid, Exercise> = self
            .gym_rows::<Exercise>("SELECT id, value FROM gym.exercises")
            .await?
            .into_iter()
            .collect();
        let sets: Vec<Set> = self
            .gym_rows::<Set>(
                "SELECT id, value FROM gym.sets WHERE json_extract(value, '$.kind') = 'working'",
            )
            .await?
            .into_iter()
            .map(|(_, set)| set)
            .collect();
        let recovery = self.gym_recovery().await?;
        let sessions = self.gym_stored_sessions().await?;
        Ok(
            fatigue_at(Utc::now(), &exercises, &sessions, &sets, &recovery)
                .map_err(|error| ClientError::Other(error.to_string()))?
                .into_iter()
                .map(|fatigue| GymMuscleFatigue {
                    muscle: fatigue.muscle,
                    display_name: fatigue.muscle.display_name().to_string(),
                    group: fatigue.muscle.group(),
                    score: fatigue.score,
                    band: fatigue.band,
                    recovery_days: fatigue.recovery_days,
                    default_recovery_days: fatigue.muscle.default_recovery_days(),
                })
                .collect(),
        )
    }

    pub async fn gym_set_recovery_days(
        &self,
        muscle: Muscle,
        recovery_days: f64,
    ) -> Result<(), ClientError> {
        let recovery = Recovery {
            muscle,
            recovery_days,
        };
        self.gym_put(
            Recovery::COLLECTION_NAME,
            Some(Recovery::row_id(muscle)),
            &recovery,
        )
        .await?;
        Ok(())
    }

    pub async fn gym_change(&self, change: GymChange) -> Result<(), ClientError> {
        match change {
            GymChange::SaveExercise { id, exercise } => {
                self.gym_save_exercise(id.as_deref(), exercise).await?;
            }
            GymChange::SaveTemplate {
                id,
                name,
                exercises,
            } => {
                self.gym_save_template(id.as_deref(), &name, exercises)
                    .await?;
            }
            GymChange::DeleteTemplate { id } => self.gym_delete_template(&id).await?,
            GymChange::StartSession { template_id } => {
                self.gym_start_session(template_id.as_deref()).await?;
            }
            GymChange::CompleteSet {
                session_id,
                exercise_id,
                kind,
                expected_order,
                values,
            } => {
                self.gym_complete_set(&session_id, &exercise_id, kind, expected_order, values)
                    .await?;
            }
            GymChange::AddSet {
                session_id,
                exercise_id,
                kind,
                values,
            } => {
                self.gym_add_set(&session_id, &exercise_id, kind, values)
                    .await?;
            }
            GymChange::EditSet { set_id, values } => self.gym_edit_set(&set_id, values).await?,
            GymChange::DeleteSet { set_id } => self.gym_delete_set(&set_id).await?,
            GymChange::DeleteSession { session_id } => {
                self.gym_delete_session(&session_id).await?;
            }
            GymChange::AddExercise {
                session_id,
                exercise_id,
            } => self.gym_add_exercise(&session_id, &exercise_id).await?,
            GymChange::MoveExercise {
                session_id,
                exercise_id,
                to_index,
            } => {
                self.gym_move_exercise(&session_id, &exercise_id, to_index)
                    .await?;
            }
            GymChange::SwitchExercise {
                session_id,
                exercise_id,
            } => self.gym_switch_exercise(&session_id, &exercise_id).await?,
            GymChange::SkipExercise {
                session_id,
                exercise_id,
                skipped,
            } => {
                self.gym_skip_exercise(&session_id, &exercise_id, skipped)
                    .await?;
            }
            GymChange::AddWorkingSet {
                session_id,
                exercise_id,
            } => self.gym_add_working_set(&session_id, &exercise_id).await?,
            GymChange::AddWarmUpSet {
                session_id,
                exercise_id,
            } => self.gym_add_warm_up_set(&session_id, &exercise_id).await?,
            GymChange::FinishSession { session_id } => {
                self.gym_finish_session(&session_id).await?;
            }
            GymChange::AddBodyWeight { kg } => {
                self.gym_add_body_weight(kg).await?;
            }
            GymChange::DeleteBodyWeight { id } => self.gym_delete_body_weight(&id).await?,
            GymChange::SetRecoveryDays {
                muscle,
                recovery_days,
            } => self.gym_set_recovery_days(muscle, recovery_days).await?,
        }
        Ok(())
    }

    async fn gym_rows<T: DeserializeOwned>(
        &self,
        sql: &str,
    ) -> Result<Vec<(Uuid, T)>, ClientError> {
        Ok(self
            .query_app_data(sql)
            .await?
            .into_iter()
            .filter_map(|row| {
                let id = row.get("id")?.as_str()?.parse().ok()?;
                let value = serde_json::from_str(row.get("value")?.as_str()?).ok()?;
                Some((id, value))
            })
            .collect())
    }

    async fn gym_row<T: DeserializeOwned>(
        &self,
        collection: &str,
        id: Uuid,
    ) -> Result<T, ClientError> {
        self.gym_rows(&format!(
            "SELECT id, value FROM {collection} WHERE id = '{id}'"
        ))
        .await?
        .into_iter()
        .next()
        .map(|(_, value)| value)
        .ok_or_else(|| ClientError::Other(format!("row {id} of {collection} was not found")))
    }

    async fn gym_put<T: Serialize>(
        &self,
        collection: &str,
        id: Option<Uuid>,
        value: &T,
    ) -> Result<Uuid, ClientError> {
        let value =
            serde_json::to_value(value).map_err(|error| ClientError::Other(error.to_string()))?;
        let id = id.map(|id| id.to_string());
        let written = self
            .write_app_data(collection, id.as_deref(), AppDataWrite::Value(value))
            .await?;
        parse_gym_id(&written)
    }

    async fn gym_delete(&self, collection: &str, id: Uuid) -> Result<(), ClientError> {
        self.write_app_data(collection, Some(&id.to_string()), AppDataWrite::Delete)
            .await?;
        Ok(())
    }

    async fn gym_stored_sessions(&self) -> Result<BTreeMap<Uuid, Session>, ClientError> {
        Ok(self
            .gym_rows::<Session>("SELECT id, value FROM gym.sessions")
            .await?
            .into_iter()
            .collect())
    }

    async fn gym_session_sets(&self, session_id: Uuid) -> Result<Vec<LoggedSet>, ClientError> {
        Ok(self
            .gym_rows::<Set>(&format!(
                "SELECT id, value FROM gym.sets WHERE json_extract(value, '$.session_id') = \
                 '{session_id}'"
            ))
            .await?
            .into_iter()
            .map(|(id, set)| LoggedSet { id, set })
            .collect())
    }

    async fn gym_free_set_order(&self, session_id: Uuid, from: u32) -> Result<u32, ClientError> {
        let mut order = from;
        while self
            .app_data_row_deleted(Set::COLLECTION_NAME, Set::row_id(session_id, order))
            .await?
        {
            order = order
                .checked_add(1)
                .ok_or_else(|| ClientError::Other("this session has no free set order".into()))?;
        }
        Ok(order)
    }

    async fn gym_working_sets_of(&self, exercise_ids: &[Uuid]) -> Result<Vec<Set>, ClientError> {
        if exercise_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = exercise_ids
            .iter()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(self
            .gym_rows::<Set>(&format!(
                "SELECT id, value FROM gym.sets WHERE json_extract(value, '$.exercise_id') IN \
                 ({ids}) AND json_extract(value, '$.kind') = 'working'"
            ))
            .await?
            .into_iter()
            .map(|(_, set)| set)
            .collect())
    }

    async fn gym_recovery(&self) -> Result<Vec<Recovery>, ClientError> {
        Ok(self
            .gym_rows::<Recovery>("SELECT id, value FROM gym.recovery")
            .await?
            .into_iter()
            .map(|(_, recovery)| recovery)
            .collect())
    }

    async fn gym_exercise_names(&self) -> Result<BTreeMap<Uuid, String>, ClientError> {
        Ok(self
            .gym_rows::<Exercise>("SELECT id, value FROM gym.exercises")
            .await?
            .into_iter()
            .map(|(id, exercise)| (id, exercise.name))
            .collect())
    }

    async fn gym_template_names(&self) -> Result<BTreeMap<Uuid, String>, ClientError> {
        Ok(self
            .gym_rows::<WorkoutTemplate>("SELECT id, value FROM gym.workouts")
            .await?
            .into_iter()
            .map(|(id, template)| (id, template.name))
            .collect())
    }

    async fn gym_change_session(
        &self,
        session_id: &str,
        change: impl FnOnce(&mut Session) -> Result<(), SessionChangeError>,
    ) -> Result<(), ClientError> {
        let _writing = GYM_WRITES.lock().await;
        let id = parse_gym_id(session_id)?;
        let mut session: Session = self.gym_row(Session::COLLECTION_NAME, id).await?;
        change(&mut session).map_err(|error| ClientError::Other(error.to_string()))?;
        self.gym_put(Session::COLLECTION_NAME, Some(id), &session)
            .await?;
        Ok(())
    }

    async fn gym_session_view(
        &self,
        id: Uuid,
        session: Session,
        sessions: &BTreeMap<Uuid, Session>,
    ) -> Result<GymSession, ClientError> {
        let names = self.gym_exercise_names().await?;
        let templates = self.gym_template_names().await?;
        let sets = self.gym_session_sets(id).await?;
        let progress = SessionProgress::new(&session, id, &sets);
        let exercise_ids: Vec<Uuid> = progress
            .exercises
            .iter()
            .map(|exercise| exercise.plan.exercise_id)
            .collect();
        let history = self.gym_working_sets_of(&exercise_ids).await?;
        let earlier: BTreeMap<Uuid, Session> = sessions
            .iter()
            .filter(|(other, earlier)| **other != id && earlier.started_at < session.started_at)
            .map(|(other, earlier)| (*other, earlier.clone()))
            .collect();
        let last_times: Vec<_> = progress
            .exercises
            .iter()
            .map(|exercise| {
                last_session_working_sets(exercise.plan.exercise_id, &earlier, &history)
            })
            .collect();
        let current = progress.current_exercise();
        let prefill = progress.current.map(|index| {
            let prefill = progress.exercises[index].prefill(last_times[index].as_ref());
            GymSetValues {
                weight_kg: prefill.weight_kg,
                reps: prefill.reps,
                reps_in_reserve: prefill.reps_in_reserve,
            }
        });
        Ok(GymSession {
            id: id.to_string(),
            name: session_name(&session, &templates),
            started_at_millis: millis(session.started_at),
            ended_at_millis: session.ended_at.map(millis),
            current_exercise_id: current.map(|exercise| exercise.plan.exercise_id.to_string()),
            next_set_kind: current.map(|exercise| exercise.next_kind()),
            next_set_order: progress.next_order,
            prefill,
            rest: progress.rest.map(|rest| GymRest {
                exercise_id: rest.exercise_id.to_string(),
                started_at_millis: millis(rest.started_at),
                ends_at_millis: millis(rest.ends_at),
            }),
            exercises: progress
                .display_order
                .iter()
                .map(|index| (*index, &progress.exercises[*index], &last_times[*index]))
                .map(|(index, exercise, last_time)| GymSessionExercise {
                    exercise_id: exercise.plan.exercise_id.to_string(),
                    name: exercise_name(&names, exercise.plan.exercise_id),
                    planned: exercise.planned,
                    plan_index: exercise
                        .planned
                        .then(|| u32::try_from(index).unwrap_or(u32::MAX)),
                    warm_up_sets: exercise.plan.warm_up_sets,
                    warm_up_rest_seconds: exercise.plan.warm_up_rest_seconds,
                    target_sets: exercise.plan.target_sets,
                    target_reps: exercise.plan.target_reps,
                    target_reps_in_reserve: exercise.plan.target_reps_in_reserve,
                    rest_seconds: exercise.plan.rest_seconds,
                    superset_with_previous: exercise.plan.superset_with_previous,
                    skipped: exercise.plan.skipped,
                    done: !exercise.is_open(),
                    sets: exercise
                        .sets
                        .iter()
                        .map(|logged| set_view(logged.id, &logged.set))
                        .collect(),
                    last_time: last_time
                        .as_ref()
                        .map(|last| {
                            last.sets
                                .iter()
                                .map(|set| set_view(Uuid::nil(), set))
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect(),
        })
    }
}

fn open_session(sessions: &BTreeMap<Uuid, Session>) -> Option<(Uuid, &Session)> {
    sessions
        .iter()
        .filter(|(_, session)| session.ended_at.is_none())
        .max_by_key(|(id, session)| (session.started_at, **id))
        .map(|(id, session)| (*id, session))
}

fn planned_view(planned: WorkoutExercise) -> GymPlannedExercise {
    GymPlannedExercise {
        exercise_id: planned.exercise_id.to_string(),
        warm_up_sets: planned.warm_up_sets,
        warm_up_rest_seconds: planned.warm_up_rest_seconds,
        target_sets: planned.target_sets,
        target_reps: planned.target_reps,
        target_reps_in_reserve: planned.target_reps_in_reserve,
        rest_seconds: planned.rest_seconds,
        superset_with_previous: planned.superset_with_previous,
    }
}

fn planned_value(planned: GymPlannedExercise) -> Result<WorkoutExercise, ClientError> {
    Ok(WorkoutExercise {
        exercise_id: parse_gym_id(&planned.exercise_id)?,
        warm_up_sets: planned.warm_up_sets,
        warm_up_rest_seconds: planned.warm_up_rest_seconds,
        target_sets: planned.target_sets,
        target_reps: planned.target_reps,
        target_reps_in_reserve: planned.target_reps_in_reserve,
        rest_seconds: planned.rest_seconds,
        superset_with_previous: planned.superset_with_previous,
    })
}

fn set_view(id: Uuid, set: &Set) -> GymSet {
    GymSet {
        id: id.to_string(),
        exercise_id: set.exercise_id.to_string(),
        order: set.order,
        kind: set.kind,
        weight_kg: set.weight_kg,
        reps: set.reps,
        reps_in_reserve: set.reps_in_reserve,
        completed_at_millis: millis(set.completed_at),
        estimated_one_rep_max_kg: estimated_one_rep_max(set),
    }
}

fn session_name(session: &Session, templates: &BTreeMap<Uuid, String>) -> String {
    session
        .template_id
        .and_then(|id| templates.get(&id).cloned())
        .unwrap_or_else(|| "Workout".to_string())
}

fn exercise_name(names: &BTreeMap<Uuid, String>, id: Uuid) -> String {
    names
        .get(&id)
        .cloned()
        .unwrap_or_else(|| "Unknown exercise".to_string())
}

fn millis(time: DateTime<Utc>) -> f64 {
    time.timestamp_millis() as f64
}

fn parse_gym_id(id: &str) -> Result<Uuid, ClientError> {
    id.parse().map_err(|source| ClientError::InvalidId {
        kind: "gym id",
        source,
    })
}
