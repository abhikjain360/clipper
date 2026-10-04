use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use clipper_app_types::AppDataWrite;
use clipper_gym::{
    BodyWeight, Exercise, FatigueBand, LoggedSet, Muscle, MuscleGroup, MuscleShare, Recovery,
    Session, SessionChangeError, SessionProgress, Set, SetKind, WorkoutExercise, WorkoutTemplate,
    best_one_rep_max_by_session, estimated_one_rep_max, fatigue_at, last_session_working_sets,
    starter_exercises, starter_templates, weekly_body_weight,
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{MobileClipperClient, MobileError};

static GYM_WRITES: Mutex<()> = Mutex::const_new(());

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

#[uniffi::remote(Enum)]
pub enum MuscleGroup {
    Push,
    Pull,
    Legs,
    Core,
}

#[uniffi::remote(Enum)]
pub enum SetKind {
    WarmUp,
    Working,
}

#[uniffi::remote(Enum)]
pub enum FatigueBand {
    Recovered,
    Low,
    Moderate,
    High,
    VeryHigh,
}

#[derive(uniffi::Enum)]
pub enum GymStarterLibrary {
    Written,
    NotNeeded,
    WaitingForDownload,
}

#[derive(uniffi::Record)]
pub struct GymMuscleInfo {
    pub muscle: Muscle,
    pub display_name: String,
    pub group: MuscleGroup,
    pub default_recovery_days: f64,
}

#[derive(uniffi::Record)]
pub struct GymMuscleShare {
    pub muscle: Muscle,
    pub share: f64,
}

#[derive(uniffi::Record)]
pub struct GymExercise {
    pub id: String,
    pub name: String,
    pub muscles: Vec<GymMuscleShare>,
    pub archived: bool,
}

#[derive(uniffi::Record)]
pub struct GymExerciseInput {
    pub name: String,
    pub muscles: Vec<GymMuscleShare>,
    pub archived: bool,
}

#[derive(uniffi::Record)]
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

#[derive(uniffi::Record)]
pub struct GymTemplate {
    pub id: String,
    pub name: String,
    pub exercises: Vec<GymPlannedExercise>,
}

#[derive(uniffi::Record)]
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

#[derive(uniffi::Record)]
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

#[derive(uniffi::Record)]
pub struct GymSetValues {
    pub weight_kg: Option<f64>,
    pub reps: Option<u32>,
    pub reps_in_reserve: Option<u8>,
}

#[derive(uniffi::Record)]
pub struct GymRest {
    pub exercise_id: String,
    pub started_at_millis: f64,
    pub ends_at_millis: f64,
}

#[derive(uniffi::Record)]
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

#[derive(uniffi::Record)]
pub struct GymSessionSummary {
    pub id: String,
    pub name: String,
    pub started_at_millis: f64,
    pub ended_at_millis: Option<f64>,
    pub exercise_names: Vec<String>,
    pub working_sets: u32,
}

#[derive(uniffi::Record)]
pub struct GymBodyWeight {
    pub id: String,
    pub time_millis: f64,
    pub kg: f64,
}

#[derive(uniffi::Record)]
pub struct GymWeeklyBodyWeight {
    pub week_start: String,
    pub average_kg: f64,
    pub measurements: u32,
    pub change_kg: Option<f64>,
}

#[derive(uniffi::Record)]
pub struct GymOneRepMax {
    pub session_id: String,
    pub started_at_millis: f64,
    pub kg: f64,
}

#[derive(uniffi::Record)]
pub struct GymMuscleFatigue {
    pub muscle: Muscle,
    pub display_name: String,
    pub group: MuscleGroup,
    pub score: u8,
    pub band: FatigueBand,
    pub recovery_days: f64,
    pub default_recovery_days: f64,
}

#[uniffi::export(async_runtime = "tokio")]
impl MobileClipperClient {
    pub fn gym_move_template_exercise(
        &self,
        exercises: Vec<GymPlannedExercise>,
        from: u32,
        to: u32,
    ) -> Result<Vec<GymPlannedExercise>, MobileError> {
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

    pub async fn gym_seed_starter_library(&self) -> Result<GymStarterLibrary, MobileError> {
        let _writing = GYM_WRITES.lock().await;
        if !self.engine.app_data_downloaded() {
            return Ok(GymStarterLibrary::WaitingForDownload);
        }
        let existing = self
            .engine
            .query_app_data(
                "SELECT id FROM gym.exercises UNION ALL SELECT id FROM gym.workouts \
                 UNION ALL SELECT id FROM gym.sessions UNION ALL SELECT id FROM gym.sets LIMIT 1",
            )
            .await?;
        if !existing.is_empty() {
            return Ok(GymStarterLibrary::NotNeeded);
        }
        for (id, exercise) in starter_exercises() {
            self.put(Exercise::COLLECTION_NAME, Some(id), &exercise)
                .await?;
        }
        for (id, template) in starter_templates() {
            self.put(WorkoutTemplate::COLLECTION_NAME, Some(id), &template)
                .await?;
        }
        Ok(GymStarterLibrary::Written)
    }

    pub async fn gym_exercises(&self) -> Result<Vec<GymExercise>, MobileError> {
        let mut exercises: Vec<GymExercise> = self
            .rows::<Exercise>("SELECT id, value FROM gym.exercises")
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
        id: Option<String>,
        exercise: GymExerciseInput,
    ) -> Result<String, MobileError> {
        let id = id.as_deref().map(parse_id).transpose()?;
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
            .put(Exercise::COLLECTION_NAME, id, &exercise)
            .await?
            .to_string())
    }

    pub async fn gym_templates(&self) -> Result<Vec<GymTemplate>, MobileError> {
        let mut templates: Vec<GymTemplate> = self
            .rows::<WorkoutTemplate>("SELECT id, value FROM gym.workouts")
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
        id: Option<String>,
        name: String,
        exercises: Vec<GymPlannedExercise>,
    ) -> Result<String, MobileError> {
        let id = id.as_deref().map(parse_id).transpose()?;
        let template = WorkoutTemplate {
            name: name.trim().to_string(),
            exercises: exercises
                .into_iter()
                .map(planned_value)
                .collect::<Result<_, _>>()?,
        };
        Ok(self
            .put(WorkoutTemplate::COLLECTION_NAME, id, &template)
            .await?
            .to_string())
    }

    pub async fn gym_delete_template(&self, id: String) -> Result<(), MobileError> {
        self.delete(WorkoutTemplate::COLLECTION_NAME, parse_id(&id)?)
            .await
    }

    pub async fn gym_open_session(&self) -> Result<Option<GymSession>, MobileError> {
        let sessions = self.sessions().await?;
        let Some((id, session)) = open_session(&sessions) else {
            return Ok(None);
        };
        Ok(Some(
            self.session_view(id, session.clone(), &sessions).await?,
        ))
    }

    pub async fn gym_session(&self, session_id: String) -> Result<GymSession, MobileError> {
        let id = parse_id(&session_id)?;
        let sessions = self.sessions().await?;
        let session = sessions
            .get(&id)
            .cloned()
            .ok_or_else(|| MobileError::Client("this session no longer exists".into()))?;
        self.session_view(id, session, &sessions).await
    }

    pub async fn gym_sessions(&self) -> Result<Vec<GymSessionSummary>, MobileError> {
        let sessions = self.sessions().await?;
        let names = self.exercise_names().await?;
        let templates = self.template_names().await?;
        let counts: BTreeMap<String, u32> = self
            .engine
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
        template_id: Option<String>,
    ) -> Result<String, MobileError> {
        let _writing = GYM_WRITES.lock().await;
        let sessions = self.sessions().await?;
        if let Some((id, _)) = open_session(&sessions) {
            return Ok(id.to_string());
        }
        let now = Utc::now();
        let session = match template_id {
            Some(template_id) => {
                let template_id = parse_id(&template_id)?;
                let template: WorkoutTemplate = self
                    .row(WorkoutTemplate::COLLECTION_NAME, template_id)
                    .await?;
                Session::from_template(now, template_id, &template)
            }
            None => Session::blank(now),
        };
        Ok(self
            .put(Session::COLLECTION_NAME, None, &session)
            .await?
            .to_string())
    }

    pub async fn gym_complete_set(
        &self,
        session_id: String,
        exercise_id: String,
        kind: SetKind,
        expected_order: u32,
        values: GymSetValues,
    ) -> Result<GymSession, MobileError> {
        let writing = GYM_WRITES.lock().await;
        let session_id = parse_id(&session_id)?;
        let exercise_id = parse_id(&exercise_id)?;
        let session: Session = self.row(Session::COLLECTION_NAME, session_id).await?;
        let sets = self.session_sets(session_id).await?;
        let progress = SessionProgress::new(&session, session_id, &sets);
        let current = progress.current_exercise();
        if progress.next_order != expected_order
            || current.is_none_or(|current| {
                current.plan.exercise_id != exercise_id || current.next_kind() != kind
            })
        {
            return Err(MobileError::Client(
                "the session changed since this screen was shown; check the sets and try again"
                    .into(),
            ));
        }
        let mut order = progress.next_order;
        while self
            .engine
            .app_data_row_deleted(Set::COLLECTION_NAME, Set::row_id(session_id, order))
            .await?
        {
            order = order
                .checked_add(1)
                .ok_or_else(|| MobileError::Client("this session has no free set order".into()))?;
        }
        let set = Set {
            session_id,
            exercise_id,
            order,
            kind,
            weight_kg: values.weight_kg,
            reps: values.reps,
            reps_in_reserve: values.reps_in_reserve,
            completed_at: Utc::now(),
        };
        self.put(Set::COLLECTION_NAME, None, &set).await?;
        drop(writing);
        let sessions = self.sessions().await?;
        self.session_view(session_id, session, &sessions).await
    }

    pub async fn gym_edit_set(
        &self,
        set_id: String,
        values: GymSetValues,
    ) -> Result<(), MobileError> {
        let _writing = GYM_WRITES.lock().await;
        let id = parse_id(&set_id)?;
        let mut set: Set = self.row(Set::COLLECTION_NAME, id).await?;
        set.weight_kg = values.weight_kg;
        set.reps = values.reps;
        set.reps_in_reserve = values.reps_in_reserve;
        self.put(Set::COLLECTION_NAME, Some(id), &set).await?;
        Ok(())
    }

    pub async fn gym_delete_set(&self, set_id: String) -> Result<(), MobileError> {
        let _writing = GYM_WRITES.lock().await;
        self.delete(Set::COLLECTION_NAME, parse_id(&set_id)?).await
    }

    pub async fn gym_delete_session(&self, session_id: String) -> Result<(), MobileError> {
        let _writing = GYM_WRITES.lock().await;
        let id = parse_id(&session_id)?;
        for logged in self.session_sets(id).await? {
            self.delete(Set::COLLECTION_NAME, logged.id).await?;
        }
        self.delete(Session::COLLECTION_NAME, id).await
    }

    pub async fn gym_add_exercise(
        &self,
        session_id: String,
        exercise_id: String,
    ) -> Result<(), MobileError> {
        let exercise_id = parse_id(&exercise_id)?;
        self.change_session(&session_id, |session| session.add_exercise(exercise_id))
            .await
    }

    pub async fn gym_move_exercise(
        &self,
        session_id: String,
        exercise_id: String,
        to_index: u32,
    ) -> Result<(), MobileError> {
        let exercise_id = parse_id(&exercise_id)?;
        let to = usize::try_from(to_index).unwrap_or(usize::MAX);
        self.change_session(&session_id, |session| {
            session.move_exercise(exercise_id, to)
        })
        .await
    }

    pub async fn gym_switch_exercise(
        &self,
        session_id: String,
        exercise_id: String,
    ) -> Result<(), MobileError> {
        let exercise_id = parse_id(&exercise_id)?;
        self.change_session(&session_id, |session| session.switch_to(exercise_id))
            .await
    }

    pub async fn gym_skip_exercise(
        &self,
        session_id: String,
        exercise_id: String,
        skipped: bool,
    ) -> Result<(), MobileError> {
        let exercise_id = parse_id(&exercise_id)?;
        self.change_session(&session_id, |session| {
            session.set_skipped(exercise_id, skipped)
        })
        .await
    }

    pub async fn gym_add_working_set(
        &self,
        session_id: String,
        exercise_id: String,
    ) -> Result<(), MobileError> {
        let exercise_id = parse_id(&exercise_id)?;
        self.change_session(&session_id, |session| session.add_working_set(exercise_id))
            .await
    }

    pub async fn gym_add_warm_up_set(
        &self,
        session_id: String,
        exercise_id: String,
    ) -> Result<(), MobileError> {
        let exercise_id = parse_id(&exercise_id)?;
        self.change_session(&session_id, |session| session.add_warm_up_set(exercise_id))
            .await
    }

    pub async fn gym_finish_session(&self, session_id: String) -> Result<(), MobileError> {
        self.change_session(&session_id, |session| session.finish(Utc::now()))
            .await
    }

    pub async fn gym_body_weights(&self) -> Result<Vec<GymBodyWeight>, MobileError> {
        let mut entries: Vec<GymBodyWeight> = self
            .rows::<BodyWeight>("SELECT id, value FROM gym.body_weight")
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

    pub async fn gym_add_body_weight(&self, kg: f64) -> Result<String, MobileError> {
        let entry = BodyWeight {
            time: Utc::now(),
            kg,
        };
        Ok(self
            .put(BodyWeight::COLLECTION_NAME, None, &entry)
            .await?
            .to_string())
    }

    pub async fn gym_delete_body_weight(&self, id: String) -> Result<(), MobileError> {
        self.delete(BodyWeight::COLLECTION_NAME, parse_id(&id)?)
            .await
    }

    pub async fn gym_weekly_body_weight(
        &self,
        zone: String,
    ) -> Result<Vec<GymWeeklyBodyWeight>, MobileError> {
        let zone: Tz = zone
            .parse()
            .map_err(|_| MobileError::Client(format!("{zone} is not a known time zone")))?;
        let entries: Vec<BodyWeight> = self
            .rows::<BodyWeight>("SELECT id, value FROM gym.body_weight")
            .await?
            .into_iter()
            .map(|(_, entry)| entry)
            .collect();
        Ok(weekly_body_weight(&entries, zone)
            .map_err(|error| MobileError::Client(error.to_string()))?
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
        exercise_id: String,
    ) -> Result<Vec<GymOneRepMax>, MobileError> {
        let exercise_id = parse_id(&exercise_id)?;
        let sessions = self.sessions().await?;
        let sets = self.working_sets_of(&[exercise_id]).await?;
        Ok(best_one_rep_max_by_session(exercise_id, &sessions, &sets)
            .into_iter()
            .map(|best| GymOneRepMax {
                session_id: best.session_id.to_string(),
                started_at_millis: millis(best.started_at),
                kg: best.kg,
            })
            .collect())
    }

    pub async fn gym_fatigue(&self) -> Result<Vec<GymMuscleFatigue>, MobileError> {
        let exercises: BTreeMap<Uuid, Exercise> = self
            .rows::<Exercise>("SELECT id, value FROM gym.exercises")
            .await?
            .into_iter()
            .collect();
        let sets: Vec<Set> = self
            .rows::<Set>(
                "SELECT id, value FROM gym.sets WHERE json_extract(value, '$.kind') = 'working'",
            )
            .await?
            .into_iter()
            .map(|(_, set)| set)
            .collect();
        let recovery = self.recovery().await?;
        let sessions = self.sessions().await?;
        Ok(
            fatigue_at(Utc::now(), &exercises, &sessions, &sets, &recovery)
                .map_err(|error| MobileError::Client(error.to_string()))?
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
    ) -> Result<(), MobileError> {
        let recovery = Recovery {
            muscle,
            recovery_days,
        };
        self.put(
            Recovery::COLLECTION_NAME,
            Some(Recovery::row_id(muscle)),
            &recovery,
        )
        .await?;
        Ok(())
    }
}

impl MobileClipperClient {
    async fn rows<T: DeserializeOwned>(&self, sql: &str) -> Result<Vec<(Uuid, T)>, MobileError> {
        Ok(self
            .engine
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

    async fn row<T: DeserializeOwned>(&self, collection: &str, id: Uuid) -> Result<T, MobileError> {
        self.rows(&format!(
            "SELECT id, value FROM {collection} WHERE id = '{id}'"
        ))
        .await?
        .into_iter()
        .next()
        .map(|(_, value)| value)
        .ok_or_else(|| MobileError::Client(format!("row {id} of {collection} was not found")))
    }

    async fn put<T: Serialize>(
        &self,
        collection: &str,
        id: Option<Uuid>,
        value: &T,
    ) -> Result<Uuid, MobileError> {
        let value =
            serde_json::to_value(value).map_err(|error| MobileError::Client(error.to_string()))?;
        let id = id.map(|id| id.to_string());
        let written = self
            .engine
            .write_app_data(collection, id.as_deref(), AppDataWrite::Value(value))
            .await?;
        parse_id(&written)
    }

    async fn delete(&self, collection: &str, id: Uuid) -> Result<(), MobileError> {
        self.engine
            .write_app_data(collection, Some(&id.to_string()), AppDataWrite::Delete)
            .await?;
        Ok(())
    }

    async fn sessions(&self) -> Result<BTreeMap<Uuid, Session>, MobileError> {
        Ok(self
            .rows::<Session>("SELECT id, value FROM gym.sessions")
            .await?
            .into_iter()
            .collect())
    }

    async fn session_sets(&self, session_id: Uuid) -> Result<Vec<LoggedSet>, MobileError> {
        Ok(self
            .rows::<Set>(&format!(
                "SELECT id, value FROM gym.sets WHERE json_extract(value, '$.session_id') = \
                 '{session_id}'"
            ))
            .await?
            .into_iter()
            .map(|(id, set)| LoggedSet { id, set })
            .collect())
    }

    async fn working_sets_of(&self, exercise_ids: &[Uuid]) -> Result<Vec<Set>, MobileError> {
        if exercise_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = exercise_ids
            .iter()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(self
            .rows::<Set>(&format!(
                "SELECT id, value FROM gym.sets WHERE json_extract(value, '$.exercise_id') IN \
                 ({ids}) AND json_extract(value, '$.kind') = 'working'"
            ))
            .await?
            .into_iter()
            .map(|(_, set)| set)
            .collect())
    }

    async fn recovery(&self) -> Result<Vec<Recovery>, MobileError> {
        Ok(self
            .rows::<Recovery>("SELECT id, value FROM gym.recovery")
            .await?
            .into_iter()
            .map(|(_, recovery)| recovery)
            .collect())
    }

    async fn exercise_names(&self) -> Result<BTreeMap<Uuid, String>, MobileError> {
        Ok(self
            .rows::<Exercise>("SELECT id, value FROM gym.exercises")
            .await?
            .into_iter()
            .map(|(id, exercise)| (id, exercise.name))
            .collect())
    }

    async fn template_names(&self) -> Result<BTreeMap<Uuid, String>, MobileError> {
        Ok(self
            .rows::<WorkoutTemplate>("SELECT id, value FROM gym.workouts")
            .await?
            .into_iter()
            .map(|(id, template)| (id, template.name))
            .collect())
    }

    async fn change_session(
        &self,
        session_id: &str,
        change: impl FnOnce(&mut Session) -> Result<(), SessionChangeError>,
    ) -> Result<(), MobileError> {
        let _writing = GYM_WRITES.lock().await;
        let id = parse_id(session_id)?;
        let mut session: Session = self.row(Session::COLLECTION_NAME, id).await?;
        change(&mut session).map_err(|error| MobileError::Client(error.to_string()))?;
        self.put(Session::COLLECTION_NAME, Some(id), &session)
            .await?;
        Ok(())
    }

    async fn session_view(
        &self,
        id: Uuid,
        session: Session,
        sessions: &BTreeMap<Uuid, Session>,
    ) -> Result<GymSession, MobileError> {
        let names = self.exercise_names().await?;
        let templates = self.template_names().await?;
        let sets = self.session_sets(id).await?;
        let progress = SessionProgress::new(&session, id, &sets);
        let exercise_ids: Vec<Uuid> = progress
            .exercises
            .iter()
            .map(|exercise| exercise.plan.exercise_id)
            .collect();
        let history = self.working_sets_of(&exercise_ids).await?;
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

fn planned_value(planned: GymPlannedExercise) -> Result<WorkoutExercise, MobileError> {
    Ok(WorkoutExercise {
        exercise_id: parse_id(&planned.exercise_id)?,
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

fn parse_id(id: &str) -> Result<Uuid, MobileError> {
    id.parse()
        .map_err(|_| MobileError::Client(format!("{id} is not a valid id")))
}
