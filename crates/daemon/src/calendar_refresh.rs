use std::{future::Future, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use clipper_client::engine::{CalendarSourceView, SyncEngine};
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tracing::warn;

const REFRESH_PERIOD: Duration = Duration::from_secs(3600);

fn due_sources(sources: &[CalendarSourceView], now: DateTime<Utc>) -> Vec<String> {
    sources
        .iter()
        .filter(|source| {
            source.enabled
                && source
                    .checked_at
                    .as_deref()
                    .and_then(|time| DateTime::parse_from_rfc3339(time).ok())
                    .is_none_or(|time| {
                        now.signed_duration_since(time) > chrono::TimeDelta::hours(1)
                            || time.signed_duration_since(now) > chrono::TimeDelta::minutes(5)
                    })
        })
        .map(|source| source.id.clone())
        .collect()
}

async fn refresh_sources<E: std::fmt::Display, F: Future<Output = Result<(), E>>>(
    ids: Vec<String>,
    mut refresh: impl FnMut(String) -> F,
) {
    for id in ids {
        if let Err(error) = refresh(id.clone()).await {
            warn!(source_id = %id, %error, "Calendar refresh failed");
        }
    }
}

pub async fn run(engine: Arc<SyncEngine>) {
    let mut state_changed = engine.subscribe();
    let mut timer = interval_at(Instant::now() + Duration::from_secs(2), REFRESH_PERIOD);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = timer.tick() => {
                let state = engine.get_state().await;
                if state.session.is_none() {
                    return;
                }
                let ids = due_sources(&state.calendar_sources, Utc::now());
                let refresh = refresh_sources(ids, |id| {
                    let engine = engine.clone();
                    async move {
                        let state = engine.get_state().await;
                        if !due_sources(&state.calendar_sources, Utc::now()).contains(&id) {
                            return Ok(());
                        }
                        engine.sync_calendar_source(&id).await.map(|_| ())
                    }
                });
                tokio::pin!(refresh);
                loop {
                    tokio::select! {
                        _ = &mut refresh => break,
                        changed = state_changed.changed() => {
                            if changed.is_err() || engine.get_state().await.session.is_none() {
                                return;
                            }
                        }
                    }
                }
            }
            changed = state_changed.changed() => {
                if changed.is_err() || engine.get_state().await.session.is_none() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn source(id: &str, enabled: bool, checked_at: Option<DateTime<Utc>>) -> CalendarSourceView {
        CalendarSourceView {
            id: id.into(),
            name: id.into(),
            protocol: "ics".into(),
            location: "https://example.com/…".into(),
            enabled,
            alarms_on: true,
            target_device: None,
            fetched_at: checked_at.map(|time| time.to_rfc3339()),
            checked_at: checked_at.map(|time| time.to_rfc3339()),
            event_count: 0,
            raw_import_file_id: None,
            raw_import_available: false,
        }
    }

    #[test]
    fn refreshes_only_enabled_sources_missing_or_older_than_an_hour() {
        let now = Utc::now();
        let mut silent = source("silent", true, None);
        silent.alarms_on = false;
        let mut checked = source("checked", true, Some(now));
        checked.fetched_at = Some((now - chrono::TimeDelta::days(2)).to_rfc3339());
        let sources = vec![
            source("never", true, None),
            source("old", true, Some(now - chrono::TimeDelta::seconds(3601))),
            source(
                "just-old",
                true,
                Some(now - chrono::TimeDelta::milliseconds(3600001)),
            ),
            source("boundary", true, Some(now - chrono::TimeDelta::hours(1))),
            source("recent", true, Some(now - chrono::TimeDelta::minutes(10))),
            source("future", true, Some(now + chrono::TimeDelta::hours(1))),
            source(
                "future-boundary",
                true,
                Some(now + chrono::TimeDelta::minutes(5)),
            ),
            source("too-far", true, Some(now + chrono::TimeDelta::seconds(301))),
            source("disabled", false, None),
            checked,
            silent,
        ];
        assert_eq!(
            due_sources(&sources, now),
            ["never", "old", "just-old", "future", "too-far", "silent"]
        );
    }

    #[tokio::test]
    async fn waits_for_each_source_and_continues_after_failure() {
        let calls = Mutex::new(Vec::new());
        refresh_sources(vec!["first".into(), "second".into()], |id| {
            let calls = &calls;
            async move {
                calls.lock().unwrap().push(format!("start {id}"));
                tokio::task::yield_now().await;
                calls.lock().unwrap().push(format!("end {id}"));
                if id == "first" { Err("failed") } else { Ok(()) }
            }
        })
        .await;
        assert_eq!(
            *calls.lock().unwrap(),
            ["start first", "end first", "start second", "end second"]
        );
    }
}
