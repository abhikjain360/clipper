use clipper_app_types::{KitchenSessionChange, KitchenTimerAction, KitchenTimerState};
use serde_json::json;

use super::{
    app_data_integration_tests::{eventually, signed_in},
    schedule_integration_tests::start_server,
    *,
};

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
            AppDataWrite::Value(json!({
                "title": "Onion soup",
                "summary": "Onions in stock",
                "servings": 2,
                "active_minutes": 10,
                "total_minutes": 30,
                "ingredients": [{"id": "onion", "name": "Onion", "amount": 2}],
                "steps": [{"text": "Simmer the {onion}.", "timers": [{"label": "Simmer", "minutes": 20}]}],
                "created_on": "2026-10-07"
            })),
        )
        .await
        .unwrap();
    eventually("the second device holds the recipe", async || {
        second.kitchen_recipe(&id, None, "UTC").await.is_ok()
    })
    .await;

    second
        .kitchen_change_session(&id, timer(KitchenTimerAction::Start))
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
        .kitchen_change_session(&id, timer(KitchenTimerAction::Pause))
        .await
        .unwrap();
    assert!(second.next_alarms(24, "UTC").await.unwrap().is_empty());
    first.stop_session_work().await;
    second.stop_session_work().await;
}
