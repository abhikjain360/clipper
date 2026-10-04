use chrono::{TimeDelta, Utc};
use clipper_app_types::{KitchenSessionChange, KitchenTimerAction, KitchenTimerState};
use serde_json::{Value, json};

use super::{
    app_data_integration_tests::{eventually, signed_in},
    schedule_integration_tests::start_server,
    *,
};

fn onion_soup(steps: Value) -> AppDataWrite {
    AppDataWrite::Value(json!({
        "title": "Onion soup",
        "summary": "Onions in stock",
        "servings": 2,
        "active_minutes": 10,
        "total_minutes": 30,
        "ingredients": [{"id": "onion", "name": "Onion", "amount": 2}],
        "steps": steps,
        "created_on": "2026-10-07"
    }))
}

fn simmer(minutes: u32) -> Value {
    json!({"text": "Simmer the {onion}.", "timers": [{"label": "Simmer", "minutes": minutes}]})
}

fn timer(action: KitchenTimerAction) -> KitchenSessionChange {
    KitchenSessionChange::Timer {
        step: 0,
        timer: 0,
        action,
    }
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_a_step_timer_rings_only_on_the_device_that_started_it() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let url = format!("http://{address}");
    let first = signed_in(&url, data, "first", true).await;
    let second = signed_in(&url, data, "second", false).await;
    let id = first
        .write_app_document(
            "kitchen.recipes",
            None,
            None,
            onion_soup(json!([simmer(20)])),
        )
        .await
        .unwrap();
    eventually("the second device holds the recipe", async || {
        second.kitchen_recipe(&id, None, "UTC").await.is_ok()
    })
    .await;

    second
        .kitchen_change_session(&id, 1, 2, timer(KitchenTimerAction::Start))
        .await
        .unwrap();
    let alarms = second.next_alarms(24, "UTC").await.unwrap();
    assert_eq!(
        alarms
            .iter()
            .map(|alarm| (alarm.label.as_str(), alarm.can_snooze))
            .collect::<Vec<_>>(),
        [("Simmer · Onion soup", false)]
    );
    eventually("the first device sees the running timer", async || {
        first
            .kitchen_recipe(&id, None, "UTC")
            .await
            .is_ok_and(|recipe| {
                recipe.steps[0].timers[0].state == KitchenTimerState::Running
                    && !recipe.steps[0].timers[0].rings_here
            })
    })
    .await;
    assert!(first.next_alarms(24, "UTC").await.unwrap().is_empty());

    second
        .kitchen_change_session(&id, 1, 2, timer(KitchenTimerAction::Pause))
        .await
        .unwrap();
    assert!(second.next_alarms(24, "UTC").await.unwrap().is_empty());
    first.stop_session_work().await;
    second.stop_session_work().await;
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_an_open_session_cooks_from_the_revision_it_started_from() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let engine = signed_in(&format!("http://{address}"), data, "cook", true).await;
    let id = engine
        .write_app_document(
            "kitchen.recipes",
            None,
            None,
            onion_soup(json!([simmer(20)])),
        )
        .await
        .unwrap();

    engine
        .kitchen_change_session(
            &id,
            1,
            3,
            KitchenSessionChange::Gathered {
                ingredient: "onion".into(),
                gathered: true,
            },
        )
        .await
        .unwrap();
    engine
        .write_app_document(
            "kitchen.recipes",
            Some(&id),
            Some(1),
            onion_soup(json!([
                {"text": "Peel the {onion}.", "timers": [{"label": "Peel", "minutes": 5}]},
                simmer(40)
            ])),
        )
        .await
        .unwrap();
    let recipe = engine.kitchen_recipe(&id, None, "UTC").await.unwrap();
    assert_eq!(
        (recipe.revision, recipe.newer_revision, recipe.servings),
        (1, Some(2), 3)
    );
    assert_eq!(
        recipe
            .steps
            .iter()
            .map(|step| step.text.as_str())
            .collect::<Vec<_>>(),
        ["Simmer the Onion (3)."]
    );
    assert!(recipe.groups[0].ingredients[0].gathered);

    engine
        .kitchen_change_session(&id, 1, 3, timer(KitchenTimerAction::Start))
        .await
        .unwrap();
    let alarms = engine.next_alarms(24, "UTC").await.unwrap();
    let rings_in = alarms[0].fire_at_millis - Utc::now().timestamp_millis();
    assert_eq!(alarms[0].label, "Simmer · Onion soup");
    assert!((19 * 60_000..=20 * 60_000).contains(&rings_in));

    engine
        .write_app_document("kitchen.recipes", Some(&id), Some(2), AppDataWrite::Delete)
        .await
        .unwrap();
    let recipe = engine.kitchen_recipe(&id, None, "UTC").await.unwrap();
    assert!(recipe.deleted && recipe.session.is_some());
    assert!(engine.next_alarms(24, "UTC").await.unwrap().is_empty());
    engine
        .kitchen_change_session(&id, 1, 3, KitchenSessionChange::Finish { notes: None })
        .await
        .unwrap();
    assert!(matches!(
        engine.kitchen_recipe(&id, None, "UTC").await,
        Err(ClientError::ItemNotFound { .. })
    ));
    engine.stop_session_work().await;
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_open_sessions_from_two_devices_merge_and_keep_their_timers() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let url = format!("http://{address}");
    let first = signed_in(&url, data, "first", true).await;
    let second = signed_in(&url, data, "second", false).await;
    let id = first
        .write_app_document(
            "kitchen.recipes",
            None,
            None,
            onion_soup(json!([
                simmer(20),
                {"text": "Taste the {onion}.", "timers": [{"label": "Taste", "minutes": 2}]},
                {"text": "Serve the {onion}.", "timers": [{"label": "Rest", "minutes": 1}]}
            ])),
        )
        .await
        .unwrap();
    let first_device = first.current_device_id().await.unwrap();
    let second_device = second.current_device_id().await.unwrap();
    let now = Utc::now();
    let at = |minutes: i64| (now + TimeDelta::minutes(minutes)).to_rfc3339();
    let older = second
        .write_app_data(
            "kitchen.sessions",
            None,
            AppDataWrite::Value(json!({
                "recipe": id,
                "recipe_revision": 1,
                "servings": 3,
                "started_at": at(-40),
                "gathered": ["onion"],
                "timers": [
                    {"step": 0, "timer": 0, "device_id": second_device, "ends_at": at(-30)},
                    {"step": 2, "timer": 0, "device_id": second_device, "ends_at": at(20)}
                ]
            })),
        )
        .await
        .unwrap();
    first
        .write_app_data(
            "kitchen.sessions",
            None,
            AppDataWrite::Value(json!({
                "recipe": id,
                "recipe_revision": 1,
                "servings": 5,
                "started_at": at(-10),
                "steps_done": [{"step": 2, "done_at": at(-1)}],
                "timers": [{"step": 1, "timer": 0, "device_id": first_device, "ends_at": at(15)}]
            })),
        )
        .await
        .unwrap();
    eventually("the second device holds both sessions", async || {
        second
            .kitchen_recipe(&id, None, "UTC")
            .await
            .is_ok_and(|recipe| recipe.steps[2].done)
    })
    .await;
    let recipe = second.kitchen_recipe(&id, None, "UTC").await.unwrap();
    let session = recipe.session.unwrap();
    assert_eq!((session.id.as_str(), session.servings), (older.as_str(), 3));
    assert_eq!(
        session.started_at_millis,
        (now - TimeDelta::minutes(40)).timestamp_millis()
    );
    assert!(recipe.groups[0].ingredients[0].gathered);
    assert_eq!(
        (
            recipe.steps[0].timers[0].state,
            recipe.steps[0].timers[0].rings_here
        ),
        (KitchenTimerState::Done, true)
    );
    assert_eq!(
        (
            recipe.steps[1].timers[0].state,
            recipe.steps[1].timers[0].rings_here
        ),
        (KitchenTimerState::Running, false)
    );
    let planned = |alarms: Vec<AlarmView>| {
        alarms
            .into_iter()
            .map(|alarm| (alarm.item_id, alarm.occurrence_key, alarm.fire_at_millis))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        planned(second.next_alarms(24, "UTC").await.unwrap()),
        [(
            id.clone(),
            "timer:0:0".to_string(),
            (now - TimeDelta::minutes(30)).timestamp_millis()
        )]
    );
    let first_timer = [(
        id.clone(),
        "timer:1:0".to_string(),
        (now + TimeDelta::minutes(15)).timestamp_millis(),
    )];
    assert_eq!(
        planned(first.next_alarms(24, "UTC").await.unwrap()),
        first_timer
    );

    second
        .kitchen_change_session(&id, 1, 3, timer(KitchenTimerAction::Clear))
        .await
        .unwrap();
    assert!(second.next_alarms(24, "UTC").await.unwrap().is_empty());
    let open_sql =
        "SELECT id FROM kitchen.sessions WHERE json_extract(value, '$.finished_at') IS NULL";
    eventually("the first device holds the merged session", async || {
        first
            .query_app_data(open_sql)
            .await
            .is_ok_and(|open| open.len() == 1 && open[0]["id"] == older.as_str())
    })
    .await;
    assert_eq!(
        planned(first.next_alarms(24, "UTC").await.unwrap()),
        first_timer
    );
    first.stop_session_work().await;
    second.stop_session_work().await;
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_a_kitchen_read_failure_still_delivers_schedule_alarm_changes() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let engine = signed_in(&format!("http://{address}"), data, "cook", true).await;
    let id = engine
        .write_app_document(
            "kitchen.recipes",
            None,
            None,
            onion_soup(json!([simmer(20)])),
        )
        .await
        .unwrap();
    engine
        .kitchen_change_session(&id, 1, 2, timer(KitchenTimerAction::Start))
        .await
        .unwrap();
    let timers = |alarms: &[AlarmView]| {
        alarms
            .iter()
            .filter(|alarm| !alarm.can_snooze)
            .map(|alarm| alarm.label.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        timers(&engine.next_alarms(24, "UTC").await.unwrap()),
        ["Simmer · Onion soup"]
    );

    engine.app_data.close().await;
    let item: ScheduleItem = serde_json::from_value(json!({
        "id": uuid::Uuid::new_v4(),
        "title": "Wake up",
        "span": ScheduleSpan::Timed {
            start: clipper_schedule::TimedStart::Floating((Utc::now() + TimeDelta::hours(2)).naive_utc()),
            duration: clipper_schedule::BlockDuration::from_minutes(30).unwrap(),
        },
        "recurrence": {"kind": "once"},
        "alarm": {"minutes_before": 5},
    }))
    .unwrap();
    engine.create_schedule_item(item.clone()).await.unwrap();

    let alarms = engine.next_alarms(24, "UTC").await.unwrap();
    assert!(
        alarms
            .iter()
            .any(|alarm| alarm.can_snooze && alarm.item_id == item.id.to_string())
    );
    assert_eq!(timers(&alarms), ["Simmer · Onion soup"]);
    engine.stop_session_work().await;
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_sessions_from_different_recipe_revisions_stay_separate() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let url = format!("http://{address}");
    let first = signed_in(&url, data, "first", true).await;
    let second = signed_in(&url, data, "second", false).await;
    let taste = json!({"text": "Taste the {onion}.", "timers": [{"label": "Taste", "minutes": 2}]});
    let id = first
        .write_app_document(
            "kitchen.recipes",
            None,
            None,
            onion_soup(json!([simmer(20), taste])),
        )
        .await
        .unwrap();
    let first_device = first.current_device_id().await.unwrap();
    let second_device = second.current_device_id().await.unwrap();
    let now = Utc::now();
    let at = |minutes: i64| (now + TimeDelta::minutes(minutes)).to_rfc3339();
    let older = second
        .write_app_data(
            "kitchen.sessions",
            None,
            AppDataWrite::Value(json!({
                "recipe": id,
                "recipe_revision": 1,
                "servings": 2,
                "started_at": at(-30),
                "gathered": ["onion"],
                "timers": [{"step": 0, "timer": 0, "device_id": second_device, "ends_at": at(10)}]
            })),
        )
        .await
        .unwrap();
    first
        .write_app_document(
            "kitchen.recipes",
            Some(&id),
            Some(1),
            onion_soup(json!([taste, simmer(25)])),
        )
        .await
        .unwrap();
    let newer = first
        .write_app_data(
            "kitchen.sessions",
            None,
            AppDataWrite::Value(json!({
                "recipe": id,
                "recipe_revision": 2,
                "servings": 4,
                "started_at": at(-10),
                "timers": [{"step": 1, "timer": 0, "device_id": first_device, "ends_at": at(5)}]
            })),
        )
        .await
        .unwrap();
    eventually("the second device holds both sessions", async || {
        second
            .kitchen_recipes("", "UTC")
            .await
            .is_ok_and(|list| list.open_sessions.len() == 2)
    })
    .await;
    let list = second.kitchen_recipes("", "UTC").await.unwrap();
    assert_eq!(
        list.open_sessions
            .iter()
            .map(|session| (session.session_id.as_str(), session.recipe_revision))
            .collect::<Vec<_>>(),
        [(older.as_str(), 1), (newer.as_str(), 2)]
    );
    let recipe = second.kitchen_recipe(&id, None, "UTC").await.unwrap();
    assert_eq!(recipe.session.unwrap().id, older);
    assert_eq!(
        recipe
            .other_sessions
            .iter()
            .map(|session| (session.id.as_str(), session.recipe_revision))
            .collect::<Vec<_>>(),
        [(newer.as_str(), 2)]
    );
    assert_eq!(recipe.steps[0].text, "Simmer the Onion (2).");
    let planned = |alarms: Vec<AlarmView>| {
        alarms
            .into_iter()
            .map(|alarm| alarm.occurrence_key)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        planned(second.next_alarms(24, "UTC").await.unwrap()),
        ["timer:0:0"]
    );

    second
        .kitchen_change_session(
            &id,
            1,
            2,
            KitchenSessionChange::Gathered {
                ingredient: "onion".into(),
                gathered: false,
            },
        )
        .await
        .unwrap();
    let open = second
        .query_app_data(
            "SELECT id FROM kitchen.sessions WHERE json_extract(value, '$.finished_at') IS NULL",
        )
        .await
        .unwrap();
    assert_eq!(open.len(), 2);
    eventually("the first device still plans its own timer", async || {
        first
            .next_alarms(24, "UTC")
            .await
            .is_ok_and(|alarms| planned(alarms) == ["timer:1:0"])
    })
    .await;
    first.stop_session_work().await;
    second.stop_session_work().await;
}
