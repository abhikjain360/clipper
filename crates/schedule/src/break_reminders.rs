use chrono::{DateTime, Duration, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakReminderKind {
    Eyes,
    Move,
    Work,
}

impl BreakReminderKind {
    pub fn message(self) -> &'static str {
        match self {
            Self::Eyes => "Look at something about 6 m away for 20 seconds.",
            Self::Move => "Break: stand up and move for 10 minutes. Walk, stretch, drink water.",
            Self::Work => "Break over. Back to work.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakReminder {
    pub kind: BreakReminderKind,
    pub at: DateTime<Utc>,
}

pub fn next_break_reminder(started: DateTime<Utc>, now: DateTime<Utc>) -> BreakReminder {
    let elapsed = now.signed_duration_since(started).num_seconds().max(0);
    let cycle = elapsed / 3600;
    for (minutes, kind) in [
        (20, BreakReminderKind::Eyes),
        (40, BreakReminderKind::Eyes),
        (50, BreakReminderKind::Move),
        (60, BreakReminderKind::Work),
    ] {
        let at = started + Duration::seconds(cycle * 3600 + minutes * 60);
        if at > now {
            return BreakReminder { kind, at };
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_repeat_from_the_timer_start() {
        let started = "2026-10-07T09:13:27Z".parse::<DateTime<Utc>>().unwrap();
        for (elapsed, next, kind) in [
            (0, 20, BreakReminderKind::Eyes),
            (19, 20, BreakReminderKind::Eyes),
            (20, 40, BreakReminderKind::Eyes),
            (40, 50, BreakReminderKind::Move),
            (50, 60, BreakReminderKind::Work),
            (60, 80, BreakReminderKind::Eyes),
            (80, 100, BreakReminderKind::Eyes),
            (100, 110, BreakReminderKind::Move),
            (110, 120, BreakReminderKind::Work),
            (120, 140, BreakReminderKind::Eyes),
        ] {
            assert_eq!(
                next_break_reminder(started, started + Duration::minutes(elapsed)),
                BreakReminder {
                    kind,
                    at: started + Duration::minutes(next),
                }
            );
        }
    }
}
