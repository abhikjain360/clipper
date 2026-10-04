use chrono::{DateTime, TimeDelta, Utc};
use uuid::Uuid;

use crate::{
    DEFAULT_WARM_UP_REST_SECONDS, LastSessionSets, Session, SessionExercise, Set, SetKind,
    WorkoutExercise, WorkoutTemplate,
};

pub const ADDED_EXERCISE_SETS: u32 = 3;
pub const ADDED_EXERCISE_REPS: u32 = 10;
pub const ADDED_EXERCISE_REPS_IN_RESERVE: u8 = 2;
pub const ADDED_EXERCISE_REST_SECONDS: u32 = 120;

#[derive(Debug, Clone, PartialEq)]
pub struct LoggedSet {
    pub id: Uuid,
    pub set: Set,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExerciseProgress {
    pub plan: SessionExercise,
    pub planned: bool,
    pub sets: Vec<LoggedSet>,
    pub warm_ups_done: u32,
    pub working_done: u32,
}

impl ExerciseProgress {
    pub fn is_open(&self) -> bool {
        !self.plan.skipped
            && (self.warm_ups_done < self.plan.warm_up_sets
                || self.working_done < self.plan.target_sets)
    }

    pub fn next_kind(&self) -> SetKind {
        if self.warm_ups_done < self.plan.warm_up_sets {
            SetKind::WarmUp
        } else {
            SetKind::Working
        }
    }

    pub fn prefill(&self, last_time: Option<&LastSessionSets>) -> SetPrefill {
        let kind = self.next_kind();
        if let Some(latest) = self
            .sets
            .iter()
            .rev()
            .find(|logged| logged.set.kind == kind)
        {
            return SetPrefill {
                weight_kg: latest.set.weight_kg,
                reps: latest.set.reps,
                reps_in_reserve: latest.set.reps_in_reserve,
            };
        }
        match kind {
            SetKind::WarmUp => SetPrefill {
                weight_kg: None,
                reps: Some(self.plan.target_reps).filter(|reps| *reps > 0),
                reps_in_reserve: None,
            },
            SetKind::Working => {
                let previous = last_time.and_then(|last| last.sets.first());
                SetPrefill {
                    weight_kg: previous.and_then(|set| set.weight_kg),
                    reps: previous
                        .and_then(|set| set.reps)
                        .or(Some(self.plan.target_reps))
                        .filter(|reps| *reps > 0),
                    reps_in_reserve: previous
                        .and_then(|set| set.reps_in_reserve)
                        .or(self.plan.target_reps_in_reserve),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SetPrefill {
    pub weight_kg: Option<f64>,
    pub reps: Option<u32>,
    pub reps_in_reserve: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestTimer {
    pub exercise_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionProgress {
    pub exercises: Vec<ExerciseProgress>,
    pub display_order: Vec<usize>,
    pub current: Option<usize>,
    pub rest: Option<RestTimer>,
    pub next_order: u32,
}

impl SessionProgress {
    pub fn new(session: &Session, session_id: Uuid, sets: &[LoggedSet]) -> Self {
        let mut sets: Vec<LoggedSet> = sets
            .iter()
            .filter(|logged| logged.set.session_id == session_id)
            .cloned()
            .collect();
        sets.sort_by(|a, b| {
            (a.set.order, a.set.completed_at, a.id).cmp(&(b.set.order, b.set.completed_at, b.id))
        });
        let next_order = sets
            .iter()
            .map(|logged| logged.set.order)
            .max()
            .map_or(1, |order| order.saturating_add(1));

        let mut exercises: Vec<ExerciseProgress> = session
            .exercises
            .iter()
            .map(|plan| ExerciseProgress {
                plan: *plan,
                planned: true,
                sets: Vec::new(),
                warm_ups_done: 0,
                working_done: 0,
            })
            .collect();
        for logged in &sets {
            let position = match exercises
                .iter()
                .position(|progress| progress.plan.exercise_id == logged.set.exercise_id)
            {
                Some(position) => position,
                None => {
                    exercises.push(ExerciseProgress {
                        plan: unplanned(logged.set.exercise_id),
                        planned: false,
                        sets: Vec::new(),
                        warm_ups_done: 0,
                        working_done: 0,
                    });
                    exercises.len() - 1
                }
            };
            let progress = &mut exercises[position];
            match logged.set.kind {
                SetKind::WarmUp => progress.warm_ups_done += 1,
                SetKind::Working => progress.working_done += 1,
            }
            progress.sets.push(logged.clone());
        }

        let blocks = superset_blocks(&exercises);
        if session.ended_at.is_some() {
            let display_order = display_order(&exercises, None);
            return Self {
                exercises,
                display_order,
                current: None,
                rest: None,
                next_order,
            };
        }
        let current = current_exercise(&exercises, &blocks, session.current_exercise_id);
        let rest = current.and(sets.last()).and_then(|last| {
            let position = exercises
                .iter()
                .position(|progress| progress.plan.exercise_id == last.set.exercise_id)?;
            let superset_hop = current
                .is_some_and(|current| current > position && blocks[current] == blocks[position]);
            let plan = &exercises[position].plan;
            let rest_seconds = match last.set.kind {
                SetKind::WarmUp => plan.warm_up_rest_seconds,
                SetKind::Working => plan.rest_seconds,
            };
            (!superset_hop && rest_seconds > 0).then(|| RestTimer {
                exercise_id: last.set.exercise_id,
                started_at: last.set.completed_at,
                ends_at: last.set.completed_at + TimeDelta::seconds(i64::from(rest_seconds)),
            })
        });
        let display_order = display_order(&exercises, current);
        Self {
            exercises,
            display_order,
            current,
            rest,
            next_order,
        }
    }

    pub fn current_exercise(&self) -> Option<&ExerciseProgress> {
        self.current.map(|index| &self.exercises[index])
    }
}

fn unplanned(exercise_id: Uuid) -> SessionExercise {
    SessionExercise {
        exercise_id,
        warm_up_sets: 0,
        warm_up_rest_seconds: DEFAULT_WARM_UP_REST_SECONDS,
        target_sets: 0,
        target_reps: 0,
        target_reps_in_reserve: None,
        rest_seconds: ADDED_EXERCISE_REST_SECONDS,
        superset_with_previous: false,
        skipped: false,
    }
}

fn superset_blocks(exercises: &[ExerciseProgress]) -> Vec<usize> {
    let mut blocks = Vec::with_capacity(exercises.len());
    let mut block = 0;
    for (index, progress) in exercises.iter().enumerate() {
        let joins_previous = index > 0
            && progress.planned
            && progress.plan.superset_with_previous
            && exercises[index - 1].planned;
        if index > 0 && !joins_previous {
            block += 1;
        }
        blocks.push(block);
    }
    blocks
}

fn display_order(exercises: &[ExerciseProgress], current: Option<usize>) -> Vec<usize> {
    let mut started: Vec<(u32, usize)> = exercises
        .iter()
        .enumerate()
        .filter_map(|(index, progress)| {
            progress
                .sets
                .iter()
                .map(|logged| logged.set.order)
                .min()
                .map(|first| (first, index))
        })
        .collect();
    started.sort_unstable();
    let mut order: Vec<usize> = started.into_iter().map(|(_, index)| index).collect();
    if let Some(current) = current
        && !order.contains(&current)
    {
        order.push(current);
    }
    for index in 0..exercises.len() {
        if !order.contains(&index) {
            order.push(index);
        }
    }
    order
}

fn open_in_block(exercises: &[ExerciseProgress], blocks: &[usize], block: usize) -> Option<usize> {
    (0..exercises.len())
        .filter(|index| blocks[*index] == block && exercises[*index].is_open())
        .min_by_key(|index| (exercises[*index].working_done, *index))
}

fn current_exercise(
    exercises: &[ExerciseProgress],
    blocks: &[usize],
    chosen: Option<Uuid>,
) -> Option<usize> {
    if let Some(next) = chosen
        .and_then(|id| {
            exercises
                .iter()
                .position(|progress| progress.planned && progress.plan.exercise_id == id)
        })
        .and_then(|chosen| open_in_block(exercises, blocks, blocks[chosen]))
    {
        return Some(next);
    }
    let mut start = 0;
    while start < exercises.len() {
        let end = (start..exercises.len())
            .find(|index| blocks[*index] != blocks[start])
            .unwrap_or(exercises.len());
        let next = (start..end)
            .filter(|index| exercises[*index].is_open())
            .min_by_key(|index| (exercises[*index].working_done, *index));
        if next.is_some() {
            return next;
        }
        start = end;
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionChangeError {
    #[error("exercise {0} is not part of this session")]
    ExerciseNotInSession(Uuid),
    #[error("this session is finished")]
    Finished,
}

impl Session {
    pub fn blank(started_at: DateTime<Utc>) -> Self {
        Self {
            started_at,
            ended_at: None,
            template_id: None,
            notes: String::new(),
            exercises: Vec::new(),
            current_exercise_id: None,
        }
    }

    pub fn from_template(
        started_at: DateTime<Utc>,
        template_id: Uuid,
        template: &WorkoutTemplate,
    ) -> Self {
        Self {
            started_at,
            ended_at: None,
            template_id: Some(template_id),
            notes: String::new(),
            exercises: template
                .exercises
                .iter()
                .copied()
                .map(SessionExercise::from)
                .collect(),
            current_exercise_id: None,
        }
    }

    pub fn add_exercise(&mut self, exercise_id: Uuid) -> Result<(), SessionChangeError> {
        self.ensure_open()?;
        if let Ok(position) = self.position(exercise_id) {
            self.exercises[position].skipped = false;
            return Ok(());
        }
        self.exercises.push(SessionExercise {
            exercise_id,
            warm_up_sets: 0,
            warm_up_rest_seconds: DEFAULT_WARM_UP_REST_SECONDS,
            target_sets: ADDED_EXERCISE_SETS,
            target_reps: ADDED_EXERCISE_REPS,
            target_reps_in_reserve: Some(ADDED_EXERCISE_REPS_IN_RESERVE),
            rest_seconds: ADDED_EXERCISE_REST_SECONDS,
            superset_with_previous: false,
            skipped: false,
        });
        Ok(())
    }

    pub fn move_exercise(
        &mut self,
        exercise_id: Uuid,
        to: usize,
    ) -> Result<(), SessionChangeError> {
        self.ensure_open()?;
        let from = self.position(exercise_id)?;
        move_keeping_supersets(
            &mut self.exercises,
            from,
            to,
            |planned| planned.superset_with_previous,
            |planned, joins| planned.superset_with_previous = joins,
        );
        Ok(())
    }

    pub fn switch_to(&mut self, exercise_id: Uuid) -> Result<(), SessionChangeError> {
        self.ensure_open()?;
        let position = self.position(exercise_id)?;
        self.exercises[position].skipped = false;
        self.current_exercise_id = Some(exercise_id);
        Ok(())
    }

    pub fn set_skipped(
        &mut self,
        exercise_id: Uuid,
        skipped: bool,
    ) -> Result<(), SessionChangeError> {
        self.ensure_open()?;
        let position = self.position(exercise_id)?;
        self.exercises[position].skipped = skipped;
        Ok(())
    }

    pub fn add_working_set(&mut self, exercise_id: Uuid) -> Result<(), SessionChangeError> {
        self.ensure_open()?;
        let position = self.position(exercise_id)?;
        let planned = &mut self.exercises[position];
        planned.target_sets = planned.target_sets.saturating_add(1);
        planned.skipped = false;
        Ok(())
    }

    pub fn add_warm_up_set(&mut self, exercise_id: Uuid) -> Result<(), SessionChangeError> {
        self.ensure_open()?;
        let position = self.position(exercise_id)?;
        let planned = &mut self.exercises[position];
        planned.warm_up_sets = planned.warm_up_sets.saturating_add(1);
        planned.skipped = false;
        Ok(())
    }

    pub fn finish(&mut self, at: DateTime<Utc>) -> Result<(), SessionChangeError> {
        self.ensure_open()?;
        self.ended_at = Some(at.max(self.started_at));
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), SessionChangeError> {
        if self.ended_at.is_some() {
            return Err(SessionChangeError::Finished);
        }
        Ok(())
    }

    fn position(&self, exercise_id: Uuid) -> Result<usize, SessionChangeError> {
        self.exercises
            .iter()
            .position(|planned| planned.exercise_id == exercise_id)
            .ok_or(SessionChangeError::ExerciseNotInSession(exercise_id))
    }
}

impl WorkoutTemplate {
    pub fn move_exercise(&mut self, from: usize, to: usize) {
        move_keeping_supersets(
            &mut self.exercises,
            from,
            to,
            |planned: &WorkoutExercise| planned.superset_with_previous,
            |planned, joins| planned.superset_with_previous = joins,
        );
    }
}

fn move_keeping_supersets<T: Copy>(
    items: &mut Vec<T>,
    from: usize,
    to: usize,
    joins_previous: impl Fn(&T) -> bool,
    set_joins_previous: impl Fn(&mut T, bool),
) {
    if from >= items.len() || items.is_empty() {
        return;
    }
    let to = to.min(items.len() - 1);
    let mut groups: Vec<Vec<T>> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match groups.last_mut() {
            Some(group) if index > 0 && joins_previous(item) => group.push(*item),
            _ => groups.push(vec![*item]),
        }
    }
    let locate = |position: usize| {
        let mut start = 0;
        for (group, members) in groups.iter().enumerate() {
            if position < start + members.len() {
                return (group, position - start);
            }
            start += members.len();
        }
        (groups.len() - 1, 0)
    };
    let (from_group, from_offset) = locate(from);
    let (to_group, to_offset) = locate(to);
    if from_group == to_group {
        let members = &mut groups[from_group];
        let item = members.remove(from_offset);
        members.insert(to_offset, item);
    } else {
        let members = groups.remove(from_group);
        groups.insert(to_group, members);
    }
    *items = groups
        .into_iter()
        .flat_map(|members| {
            members.into_iter().enumerate().map(|(offset, mut item)| {
                set_joins_previous(&mut item, offset > 0);
                item
            })
        })
        .collect();
}
