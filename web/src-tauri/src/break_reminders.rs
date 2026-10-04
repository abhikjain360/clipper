use chrono::{DateTime, Duration, Utc};
use clipper_app_types::{ActualView, OccurrenceView};
use clipper_schedule::{BreakReminder, next_break_reminder};

struct Timer {
    id: String,
    started: DateTime<Utc>,
    next: BreakReminder,
}

#[derive(Default)]
struct BreakReminders {
    timer: Option<Timer>,
}

impl BreakReminders {
    fn update(&mut self, actual: Option<&ActualView>, now: DateTime<Utc>) -> Option<BreakReminder> {
        let Some(actual) = actual.filter(|actual| {
            actual.running
                && actual.end.is_empty()
                && !actual.item_id.is_empty()
                && actual.break_reminders
        }) else {
            self.timer = None;
            return None;
        };
        let Ok(started) = actual.start.parse::<DateTime<Utc>>() else {
            self.timer = None;
            return None;
        };
        if self
            .timer
            .as_ref()
            .is_none_or(|timer| timer.id != actual.id || timer.started != started)
        {
            self.timer = Some(Timer {
                id: actual.id.clone(),
                started,
                next: next_break_reminder(started, now),
            });
        }
        let timer = self.timer.as_mut()?;
        if now < timer.next.at {
            return None;
        }
        let reminder = timer.next;
        timer.next = next_break_reminder(started, now);
        (now - reminder.at < Duration::minutes(1)).then_some(reminder)
    }
}

fn outside_meetings(reminder: BreakReminder, meetings: &[OccurrenceView]) -> bool {
    !meetings.iter().any(|meeting| {
        meeting
            .start
            .parse::<DateTime<Utc>>()
            .is_ok_and(|start| start <= reminder.at)
            && meeting
                .end
                .parse::<DateTime<Utc>>()
                .is_ok_and(|end| reminder.at < end)
    })
}

#[cfg(target_os = "macos")]
pub async fn run(daemon: std::sync::Arc<crate::daemon_client::DaemonClient>) {
    use std::time::Duration as Wait;

    use clipper_daemon_types::{ActualsBetweenParams, DaemonCommand, ExpandScheduleParams};

    use crate::notifications;

    let mut reminders = BreakReminders::default();
    let mut authorized = false;
    let mut resolved_timer = None;
    loop {
        let version = daemon.state_version();
        let state = daemon.get_state().await;
        if let Some(actual) = state
            .running_actual
            .as_ref()
            .filter(|actual| actual.running && !actual.item_id.is_empty())
            && resolved_timer.as_ref() != Some(&actual.id)
        {
            resolved_timer = Some(actual.id.clone());
            let _ = tokio::time::timeout(
                Wait::from_secs(10),
                daemon.send_result::<Vec<ActualView>>(DaemonCommand::ActualsBetween(
                    ActualsBetweenParams {
                        from: actual.start.clone(),
                        to: Utc::now().to_rfc3339(),
                    },
                )),
            )
            .await;
            continue;
        }
        let due = reminders.update(state.running_actual.as_ref(), Utc::now());
        if reminders.timer.is_some() && !authorized {
            if !notifications::request_permission().await {
                return;
            }
            authorized = true;
            continue;
        }
        if let Some(reminder) = due
            && let Ok(Ok(meetings)) = tokio::time::timeout(
                Wait::from_secs(2),
                daemon.send_result::<Vec<OccurrenceView>>(DaemonCommand::MeetingsBetween(
                    ExpandScheduleParams {
                        from: reminder.at.to_rfc3339(),
                        to: (reminder.at + Duration::seconds(1)).to_rfc3339(),
                        observer_zone: iana_time_zone::get_timezone()
                            .unwrap_or_else(|_| "UTC".into()),
                    },
                )),
            )
            .await
            && outside_meetings(reminder, &meetings)
            && let Ok(Ok(fresh)) = tokio::time::timeout(
                Wait::from_secs(2),
                daemon.send_result::<clipper_app_types::AppState>(DaemonCommand::GetState),
            )
            .await
            && let Some(actual) = fresh.running_actual
            && actual.running
            && actual.break_reminders
            && actual.end.is_empty()
            && reminders.timer.as_ref().is_some_and(|timer| {
                timer.id == actual.id && actual.start.parse::<DateTime<Utc>>() == Ok(timer.started)
            })
        {
            notifications::show(reminder.kind.message());
        }
        let delay = reminders
            .timer
            .as_ref()
            .map(|timer| {
                (timer.next.at - Utc::now())
                    .to_std()
                    .unwrap_or_default()
                    .min(Wait::from_secs(60))
            })
            .unwrap_or(Wait::from_secs(60));
        tokio::select! {
            _ = daemon.wait_for_state_change_after(version) => {}
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use clipper_schedule::BreakReminderKind;

    use super::*;

    fn actual() -> ActualView {
        ActualView {
            id: "timer".into(),
            item_id: "block".into(),
            start: "2026-10-07T09:13:27Z".into(),
            running: true,
            break_reminders: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_running_timer_delivers_each_reminder_once_and_repeats() {
        let actual = actual();
        let started = actual.start.parse::<DateTime<Utc>>().unwrap();
        let mut reminders = BreakReminders::default();
        assert_eq!(reminders.update(Some(&actual), started), None);
        for (minute, kind) in [
            (20, BreakReminderKind::Eyes),
            (40, BreakReminderKind::Eyes),
            (50, BreakReminderKind::Move),
            (60, BreakReminderKind::Work),
            (80, BreakReminderKind::Eyes),
            (100, BreakReminderKind::Eyes),
            (110, BreakReminderKind::Move),
            (120, BreakReminderKind::Work),
        ] {
            let at = started + Duration::minutes(minute);
            assert_eq!(
                reminders.update(Some(&actual), at - Duration::seconds(1)),
                None
            );
            assert_eq!(
                reminders.update(Some(&actual), at),
                Some(BreakReminder { kind, at })
            );
            assert_eq!(reminders.update(Some(&actual), at), None);
        }
    }

    #[test]
    fn stopping_a_timer_cancels_its_reminders() {
        let mut actual = actual();
        let started = actual.start.parse::<DateTime<Utc>>().unwrap();
        let mut reminders = BreakReminders::default();
        reminders.update(Some(&actual), started);
        actual.running = false;
        actual.end = (started + Duration::minutes(10)).to_rfc3339();
        assert_eq!(
            reminders.update(Some(&actual), started + Duration::minutes(20)),
            None
        );
        assert_eq!(
            reminders.update(None, started + Duration::minutes(50)),
            None
        );
        assert_eq!(
            reminders.update(None, started + Duration::minutes(80)),
            None
        );
    }

    #[test]
    fn timers_without_an_enabled_block_stay_silent() {
        for actual in [
            ActualView {
                break_reminders: false,
                ..actual()
            },
            ActualView {
                item_id: String::new(),
                ..actual()
            },
        ] {
            let started = actual.start.parse::<DateTime<Utc>>().unwrap();
            let mut reminders = BreakReminders::default();
            assert_eq!(reminders.update(Some(&actual), started), None);
            assert_eq!(
                reminders.update(Some(&actual), started + Duration::minutes(20)),
                None
            );
        }
    }

    #[test]
    fn a_meeting_skips_one_reminder_without_changing_the_rhythm() {
        let actual = actual();
        let started = actual.start.parse::<DateTime<Utc>>().unwrap();
        let meetings = vec![OccurrenceView {
            start: (started + Duration::minutes(20)).to_rfc3339(),
            end: (started + Duration::minutes(40)).to_rfc3339(),
            source: Some("Meetings".into()),
            ..Default::default()
        }];
        let mut reminders = BreakReminders::default();
        reminders.update(Some(&actual), started);
        let deliver = |reminder| outside_meetings(reminder, &meetings);
        assert_eq!(
            reminders
                .update(Some(&actual), started + Duration::minutes(20))
                .filter(|reminder| deliver(*reminder)),
            None,
        );
        assert_eq!(
            reminders.timer.as_ref().unwrap().next.at,
            started + Duration::minutes(40)
        );
        let at = started + Duration::minutes(40);
        assert_eq!(
            reminders
                .update(Some(&actual), at)
                .filter(|reminder| deliver(*reminder)),
            Some(BreakReminder {
                at,
                kind: BreakReminderKind::Eyes
            }),
        );
    }
}
