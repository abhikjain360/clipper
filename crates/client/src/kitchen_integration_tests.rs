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
async fn live_two_open_sessions_of_one_recipe_settle_on_the_newest() {
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
    let device = second.current_device_id().await.unwrap();
    let now = Utc::now();
    let session = |started: i64, ends_in: i64| {
        AppDataWrite::Value(json!({
            "recipe": id,
            "recipe_revision": 1,
            "servings": 2,
            "started_at": (now - TimeDelta::minutes(started)).to_rfc3339(),
            "timers": [{
                "step": 0,
                "timer": 0,
                "device_id": device,
                "ends_at": (now + TimeDelta::minutes(ends_in)).to_rfc3339()
            }]
        }))
    };
    second
        .write_app_data("kitchen.sessions", None, session(30, 5))
        .await
        .unwrap();
    let newer = first
        .write_app_data("kitchen.sessions", None, session(20, -2))
        .await
        .unwrap();
    eventually("the second device holds both sessions", async || {
        second
            .kitchen_recipe(&id, None, "UTC")
            .await
            .is_ok_and(|recipe| recipe.session.is_some_and(|session| session.id == newer))
    })
    .await;
    let alarms = second.next_alarms(24, "UTC").await.unwrap();
    assert_eq!(
        alarms
            .iter()
            .map(|alarm| alarm.item_id.as_str())
            .collect::<Vec<_>>(),
        [newer.as_str()]
    );

    second
        .kitchen_change_session(&id, 1, 2, timer(KitchenTimerAction::Clear))
        .await
        .unwrap();
    let open = second
        .query_app_data(
            "SELECT id FROM kitchen.sessions WHERE json_extract(value, '$.finished_at') IS NULL",
        )
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0]["id"], newer.as_str());
    assert!(second.next_alarms(24, "UTC").await.unwrap().is_empty());
    first.stop_session_work().await;
    second.stop_session_work().await;
}
