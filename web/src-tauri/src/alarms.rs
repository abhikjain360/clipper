use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use clipper_app_types::AlarmView;
use clipper_daemon_types::{DaemonCommand, ExpandScheduleParams};

use crate::{daemon_client::DaemonClient, notifications};

struct Alarms {
    after: DateTime<Utc>,
}

impl Alarms {
    fn from(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        self.after.max(now - chrono::Duration::minutes(1))
    }

    fn due<'a>(&mut self, plan: Option<&'a [AlarmView]>, now: DateTime<Utc>) -> Vec<&'a AlarmView> {
        let Some(plan) = plan else {
            return Vec::new();
        };
        let from = self.from(now).timestamp_millis();
        self.after = now;
        plan.iter()
            .filter(|alarm| {
                alarm.fire_at_millis > from && alarm.fire_at_millis <= now.timestamp_millis()
            })
            .collect()
    }
}

pub async fn run(daemon: Arc<DaemonClient>) {
    let mut alarms = Alarms { after: Utc::now() };
    let mut device = None;
    let mut authorized = false;
    loop {
        let version = daemon.state_version();
        let state = daemon.get_state().await;
        let now = Utc::now();
        let current_device = state.device_id().map(str::to_owned);
        if current_device != device {
            device = current_device;
            alarms.after = now;
        }
        let mut delay = Duration::from_secs(60);
        if device.is_some() {
            let from = alarms.from(now);
            let zone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                daemon.send_result::<Vec<AlarmView>>(DaemonCommand::DesktopAlarms(
                    ExpandScheduleParams {
                        from: from.to_rfc3339(),
                        to: (now + chrono::Duration::days(1)).to_rfc3339(),
                        observer_zone: zone,
                    },
                )),
            )
            .await;
            let plan = result.ok().and_then(Result::ok);
            if plan.as_ref().is_some_and(|plan| !plan.is_empty()) && !authorized {
                if !notifications::request_permission().await {
                    return;
                }
                authorized = true;
                continue;
            }
            if daemon.state_version() != version {
                continue;
            }
            let observed = Utc::now();
            let fresh = daemon.get_state().await;
            if fresh.device_id() == device.as_deref() {
                for alarm in alarms.due(plan.as_deref(), observed) {
                    notifications::show_alarm(alarm);
                }
                if let Some(next) = plan
                    .iter()
                    .flatten()
                    .find(|alarm| alarm.fire_at_millis > observed.timestamp_millis())
                {
                    delay = Duration::from_millis(
                        (next.fire_at_millis - observed.timestamp_millis()) as u64,
                    )
                    .min(delay);
                }
            }
            if plan.is_none() {
                delay = Duration::from_secs(5);
            }
        }
        tokio::select! {
            _ = daemon.wait_for_state_change_after(version) => {}
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_queries_leave_due_alarms_available_for_the_next_plan() {
        let started: DateTime<Utc> = "2026-10-07T09:00:00Z".parse().unwrap();
        let mut alarms = Alarms { after: started };
        let fire_at = started + chrono::Duration::seconds(5);
        let plan = vec![AlarmView {
            fire_at_millis: fire_at.timestamp_millis(),
            ..Default::default()
        }];
        assert!(
            alarms
                .due(None, started + chrono::Duration::seconds(10))
                .is_empty()
        );
        assert!(
            alarms
                .due(None, started + chrono::Duration::seconds(15))
                .is_empty()
        );
        let due = alarms.due(Some(&plan), started + chrono::Duration::seconds(20));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].fire_at_millis, fire_at.timestamp_millis());
        assert!(
            alarms
                .due(Some(&plan), started + chrono::Duration::seconds(25))
                .is_empty()
        );
    }
}
