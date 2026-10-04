use clipper_kitchen::{Recipe, decode};
use serde_json::json;

#[test]
fn an_invalid_recipe_is_refused_with_its_field_path() {
    let recipe = json!({
        "title": "Onion and carrot soup",
        "summary": "A simple vegetable soup",
        "servings": 2,
        "active_minutes": 10,
        "total_minutes": 30,
        "ingredients": [
            {"id": "onion", "name": "Onion", "amount": 1, "unit": "piece"},
            {"id": "carrot", "name": "Carrot", "amount": 2, "unit": "pieces"}
        ],
        "steps": [
            {"text": "Chop {onion}.", "timers": [{"label": "Chop", "minutes": 5}]},
            {"text": "Simmer the vegetables.", "timers": [{"label": "Simmer", "minutes": 20}]}
        ],
        "created_on": "2026-10-07"
    });
    assert!(decode::<Recipe>(&recipe).is_ok());

    let mut changed = recipe.clone();
    changed["steps"][1]["text"] = json!("Add {garlic}.");
    assert_eq!(
        decode::<Recipe>(&changed).unwrap_err().to_string(),
        "steps[1].text: {garlic} does not match any ingredient id"
    );

    let mut changed = recipe.clone();
    changed["ingredients"][0]
        .as_object_mut()
        .unwrap()
        .remove("amount");
    assert_eq!(
        decode::<Recipe>(&changed).unwrap_err().to_string(),
        "ingredients[0].unit: a unit needs an amount"
    );

    let mut changed = recipe.clone();
    changed["active_minutes"] = json!(31);
    assert_eq!(
        decode::<Recipe>(&changed).unwrap_err().to_string(),
        "active_minutes: active time cannot exceed total time"
    );

    let mut changed = recipe.clone();
    changed["ingredients"][1]["id"] = json!("onion");
    assert_eq!(
        decode::<Recipe>(&changed).unwrap_err().to_string(),
        "ingredients[1].id: duplicate ingredient id \"onion\""
    );

    let mut changed = recipe.clone();
    changed["steps"][0]["text"] = json!("");
    assert_eq!(
        decode::<Recipe>(&changed).unwrap_err().to_string(),
        "steps[0].text: must not be empty"
    );

    let mut changed = recipe.clone();
    changed["created_on"] = json!("2026-2-18");
    assert_eq!(
        decode::<Recipe>(&changed).unwrap_err().to_string(),
        "created_on: must be a date written YYYY-MM-DD"
    );

    let mut changed = recipe;
    changed["steps"][0]["timers"][0]["minutes"] = json!("five");
    assert!(
        decode::<Recipe>(&changed)
            .unwrap_err()
            .to_string()
            .starts_with("steps[0].timers[0].minutes: ")
    );
}
