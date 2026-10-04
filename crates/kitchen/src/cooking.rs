use chrono::{DateTime, TimeDelta, Utc};
use uuid::Uuid;

use crate::{CookingSession, SessionTimer, StepDone};

const ONE_MINUTE_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerState {
    Idle,
    Running { ends_at: DateTime<Utc> },
    Paused { remaining_ms: u64 },
    Done { ended_at: DateTime<Utc> },
}

impl CookingSession {
    pub fn new(recipe: Uuid, recipe_revision: u64, servings: u32, now: DateTime<Utc>) -> Self {
        Self {
            recipe,
            recipe_revision,
            servings,
            started_at: now,
            finished_at: None,
            steps_done: Vec::new(),
            gathered: Vec::new(),
            timers: Vec::new(),
            notes: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.finished_at.is_none()
    }

    pub fn has_progress(&self) -> bool {
        !self.gathered.is_empty() || !self.steps_done.is_empty() || !self.timers.is_empty()
    }

    pub fn merge(mut self, newer: CookingSession) -> CookingSession {
        if !self.has_progress() {
            self.recipe_revision = newer.recipe_revision;
            self.servings = newer.servings;
        }
        self.started_at = self.started_at.min(newer.started_at);
        for ingredient in newer.gathered {
            if !self.is_gathered(&ingredient) {
                self.gathered.push(ingredient);
            }
        }
        for done in newer.steps_done {
            match self
                .steps_done
                .iter_mut()
                .find(|entry| entry.step == done.step)
            {
                Some(entry) => entry.done_at = entry.done_at.min(done.done_at),
                None => self.steps_done.push(done),
            }
        }
        for timer in newer.timers {
            self.timers
                .retain(|entry| (entry.step, entry.timer) != (timer.step, timer.timer));
            self.timers.push(timer);
        }
        self.notes = self.notes.or(newer.notes);
        self
    }

    pub fn is_gathered(&self, ingredient: &str) -> bool {
        self.gathered.iter().any(|id| id == ingredient)
    }

    pub fn set_gathered(&mut self, ingredient: &str, gathered: bool) {
        self.gathered.retain(|id| id != ingredient);
        if gathered {
            self.gathered.push(ingredient.to_string());
        }
    }

    pub fn step_done_at(&self, step: u32) -> Option<DateTime<Utc>> {
        self.steps_done
            .iter()
            .find(|done| done.step == step)
            .map(|done| done.done_at)
    }

    pub fn set_step_done(&mut self, step: u32, done: bool, now: DateTime<Utc>) {
        self.steps_done.retain(|entry| entry.step != step);
        if done {
            self.steps_done.push(StepDone { step, done_at: now });
            self.timers.retain(|timer| timer.step != step);
        }
    }

    pub fn timer_state(&self, step: u32, timer: u32, now: DateTime<Utc>) -> TimerState {
        match self.timer(step, timer) {
            None => TimerState::Idle,
            Some(SessionTimer {
                remaining_ms: Some(remaining_ms),
                ..
            }) => TimerState::Paused {
                remaining_ms: *remaining_ms,
            },
            Some(SessionTimer {
                ends_at: Some(ends_at),
                ..
            }) if *ends_at <= now => TimerState::Done { ended_at: *ends_at },
            Some(SessionTimer {
                ends_at: Some(ends_at),
                ..
            }) => TimerState::Running { ends_at: *ends_at },
            Some(_) => TimerState::Idle,
        }
    }

    pub fn timer_device(&self, step: u32, timer: u32) -> Option<Uuid> {
        self.timer(step, timer).map(|timer| timer.device_id)
    }

    pub fn start_timer(
        &mut self,
        step: u32,
        timer: u32,
        minutes: f64,
        device: Uuid,
        now: DateTime<Utc>,
    ) {
        self.clear_timer(step, timer);
        if let Some(ends_at) = later(now, minutes_to_ms(minutes)) {
            self.timers.push(SessionTimer {
                step,
                timer,
                device_id: device,
                ends_at: Some(ends_at),
                remaining_ms: None,
            });
        }
    }

    pub fn pause_timer(&mut self, step: u32, timer: u32, now: DateTime<Utc>) {
        if let Some(entry) = self.timer_mut(step, timer)
            && let Some(ends_at) = entry.ends_at
        {
            let remaining = (ends_at - now).num_milliseconds().max(0);
            entry.ends_at = None;
            entry.remaining_ms = Some(u64::try_from(remaining).unwrap_or_default());
        }
    }

    pub fn resume_timer(&mut self, step: u32, timer: u32, now: DateTime<Utc>) {
        if let Some(entry) = self.timer_mut(step, timer)
            && let Some(ends_at) = entry
                .remaining_ms
                .and_then(|remaining| later(now, remaining))
        {
            entry.ends_at = Some(ends_at);
            entry.remaining_ms = None;
        }
    }

    pub fn add_minute(&mut self, step: u32, timer: u32, now: DateTime<Utc>) {
        if let Some(entry) = self.timer_mut(step, timer) {
            if let Some(remaining) = entry.remaining_ms {
                entry.remaining_ms = Some(remaining.saturating_add(ONE_MINUTE_MS));
            } else if let Some(ends_at) = entry.ends_at
                && let Some(extended) = later(ends_at.max(now), ONE_MINUTE_MS)
            {
                entry.ends_at = Some(extended);
            }
        }
    }

    pub fn clear_timer(&mut self, step: u32, timer: u32) {
        self.timers
            .retain(|entry| entry.step != step || entry.timer != timer);
    }

    pub fn finish(&mut self, notes: Option<String>, now: DateTime<Utc>) {
        self.finished_at = Some(now.max(self.started_at));
        self.notes = notes.filter(|notes| !notes.trim().is_empty());
        self.timers.clear();
    }

    pub fn running_timers_on(&self, device: Uuid) -> Vec<(u32, u32, DateTime<Utc>)> {
        if !self.is_open() {
            return Vec::new();
        }
        self.timers
            .iter()
            .filter(|timer| timer.device_id == device)
            .filter_map(|timer| Some((timer.step, timer.timer, timer.ends_at?)))
            .collect()
    }

    fn timer(&self, step: u32, timer: u32) -> Option<&SessionTimer> {
        self.timers
            .iter()
            .find(|entry| entry.step == step && entry.timer == timer)
    }

    fn timer_mut(&mut self, step: u32, timer: u32) -> Option<&mut SessionTimer> {
        self.timers
            .iter_mut()
            .find(|entry| entry.step == step && entry.timer == timer)
    }
}

fn minutes_to_ms(minutes: f64) -> u64 {
    let milliseconds = (minutes * 60_000.0).round();
    if milliseconds.is_finite() && milliseconds > 0.0 {
        milliseconds as u64
    } else {
        0
    }
}

fn later(from: DateTime<Utc>, milliseconds: u64) -> Option<DateTime<Utc>> {
    let delta = TimeDelta::try_milliseconds(i64::try_from(milliseconds).ok()?)?;
    from.checked_add_signed(delta)
}
