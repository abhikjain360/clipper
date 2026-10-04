use chrono::{DateTime, TimeDelta, Utc};
use clipper_kitchen::{CookingSession, Recipe, TimerState, decode, format_quantity, scale_factor};
use serde_json::json;
use uuid::Uuid;

fn at(minutes: i64) -> DateTime<Utc> {
    "2026-10-07T18:00:00Z".parse::<DateTime<Utc>>().unwrap() + TimeDelta::minutes(minutes)
}

fn recipe() -> Recipe {
    decode(&json!({
        "title": "Onion soup",
        "summary": "Onions softened in butter",
        "servings": 2,
        "active_minutes": 20,
        "total_minutes": 45,
        "ingredients": [
            {"id": "onion", "name": "Onion", "amount": 2, "unit": "pieces"},
            {"id": "stock", "name": "Stock", "amount": 600, "unit": "g"},
            {"id": "thyme", "name": "Thyme", "amount": 0.5, "unit": "tbsp"},
            {"id": "salt", "name": "Salt", "amount": 1, "unit": "pinch", "scales": false}
        ],
        "steps": [
            {"text": "Slice the {onion} and add the {salt}.", "timers": [{"label": "Slice", "minutes": 5}]},
            {"text": "Simmer in the {stock} with {thyme}.", "timers": [{"label": "Simmer", "minutes": 10}]}
        ],
        "created_on": "2026-10-07"
    }))
    .unwrap()
}

#[test]
fn scaling_changes_scaled_quantities_and_the_step_text() {
    let recipe = recipe();
    let quantities = |servings| {
        let factor = scale_factor(recipe.servings, servings);
        recipe
            .ingredients
            .iter()
            .map(|ingredient| ingredient.quantity(factor))
            .collect::<Vec<_>>()
    };
    assert_eq!(quantities(2), ["2 pieces", "600 g", "½ tbsp", "1 pinch"]);
    assert_eq!(quantities(3), ["3 pieces", "900 g", "¾ tbsp", "1 pinch"]);
    assert_eq!(quantities(4), ["4 pieces", "1.2 kg", "1 tbsp", "1 pinch"]);
    assert_eq!(format_quantity(Some(0.04), Some("tsp")), "0.04 tsp");
    assert_eq!(format_quantity(Some(0.013), Some("g")), "0.013 g");
    assert_eq!(
        recipe.step_text(0, scale_factor(2, 3)),
        "Slice the Onion (3 pieces) and add the Salt (1 pinch)."
    );
}

#[test]
fn timers_are_derived_from_their_stored_instants() {
    let device = Uuid::now_v7();
    let mut session = CookingSession::new(Uuid::now_v7(), 1, 2, at(0));
    assert_eq!(session.timer_state(1, 0, at(0)), TimerState::Idle);

    session.start_timer(1, 0, 10.0, device, at(0));
    assert_eq!(
        session.timer_state(1, 0, at(4)),
        TimerState::Running { ends_at: at(10) }
    );
    session.pause_timer(1, 0, at(4));
    assert_eq!(
        session.timer_state(1, 0, at(30)),
        TimerState::Paused {
            remaining_ms: 6 * 60_000
        }
    );
    session.resume_timer(1, 0, at(20));
    assert_eq!(session.running_timers_on(device), [(1, 0, at(26))]);
    assert_eq!(
        session.timer_state(1, 0, at(26)),
        TimerState::Done { ended_at: at(26) }
    );
    session.add_minute(1, 0, at(30));
    assert_eq!(
        session.timer_state(1, 0, at(30)),
        TimerState::Running { ends_at: at(31) }
    );
    assert!(session.running_timers_on(Uuid::now_v7()).is_empty());
}

#[test]
fn a_session_read_back_from_storage_keeps_its_checklist_and_timers() {
    let device = Uuid::now_v7();
    let mut session = CookingSession::new(Uuid::now_v7(), 3, 4, at(0));
    session.set_gathered("onion", true);
    session.start_timer(0, 0, 5.0, device, at(1));
    session.start_timer(1, 0, 10.0, device, at(2));
    session.pause_timer(1, 0, at(5));
    session.set_step_done(0, true, at(6));

    let stored = serde_json::to_value(&session).unwrap();
    let restored: CookingSession = decode(&stored).unwrap();

    assert_eq!(restored, session);
    assert!(restored.is_gathered("onion"));
    assert_eq!(restored.step_done_at(0), Some(at(6)));
    assert_eq!(restored.timer_state(0, 0, at(7)), TimerState::Idle);
    assert_eq!(
        restored.timer_state(1, 0, at(60)),
        TimerState::Paused {
            remaining_ms: 7 * 60_000
        }
    );
}

#[test]
fn each_session_starts_with_an_empty_checklist() {
    let recipe_id = Uuid::now_v7();
    let mut first = CookingSession::new(recipe_id, 1, 2, at(0));
    first.set_gathered("onion", true);
    first.set_step_done(0, true, at(5));
    first.start_timer(1, 0, 10.0, Uuid::now_v7(), at(6));
    first.finish(Some("Too salty".into()), at(30));

    let second = CookingSession::new(recipe_id, 2, 2, at(60));

    assert!(first.is_gathered("onion") && first.timers.is_empty() && !first.is_open());
    assert!(!second.is_gathered("onion"));
    assert_eq!(second.step_done_at(0), None);
    assert!(second.is_open() && second.timers.is_empty());
}

#[test]
fn a_step_done_in_either_session_stops_its_timers() {
    let recipe_id = Uuid::now_v7();
    let phone = Uuid::now_v7();
    let mut earlier = CookingSession::new(recipe_id, 1, 2, at(0));
    earlier.start_timer(0, 0, 10.0, phone, at(1));
    earlier.start_timer(1, 0, 10.0, phone, at(1));
    let mut later = CookingSession::new(recipe_id, 1, 4, at(5));
    later.set_step_done(1, true, at(6));

    let merged = earlier.merge(later);

    assert_eq!((merged.started_at, merged.servings), (at(0), 2));
    assert_eq!(merged.step_done_at(1), Some(at(6)));
    assert_eq!(merged.running_timers_on(phone), [(0, 0, at(11))]);
    assert_eq!(merged.timer_state(1, 0, at(7)), TimerState::Idle);

    let stored: CookingSession = decode(&json!({
        "recipe": recipe_id,
        "recipe_revision": 1,
        "servings": 2,
        "started_at": "2026-10-07T18:00:00Z",
        "steps_done": [{"step": 1, "done_at": "2026-10-07T18:06:00Z"}],
        "timers": [{"step": 1, "timer": 0, "device_id": phone, "ends_at": "2026-10-07T18:11:00Z"}]
    }))
    .unwrap();
    assert!(stored.running_timers_on(phone).is_empty());
}
