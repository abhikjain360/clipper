use serde_json::{Value, json};

use super::{
    app_data_integration_tests::{eventually, query, signed_in},
    schedule_integration_tests::start_server,
    *,
};

fn recipe(title: &str) -> Value {
    json!({
        "title": title,
        "summary": "Onions softened slowly in butter",
        "cuisine": null,
        "servings": 2,
        "active_minutes": 10,
        "total_minutes": 25,
        "ingredients": [
            {"id": "onion", "name": "Onion", "amount": 2, "unit": "pieces"},
            {"id": "butter", "name": "Butter", "amount": 20, "unit": "g"}
        ],
        "steps": [
            {"text": "Slice the {onion}.", "timers": [{"label": "Slice", "minutes": 5}]},
            {"text": "Soften it in the {butter}.", "timers": [{"label": "Soften", "minutes": 20}]}
        ],
        "created_on": "2026-10-07"
    })
}

async fn write_recipe(
    engine: &SyncEngine,
    id: Option<&str>,
    read_revision: Option<u64>,
    value: Value,
) -> Result<String, ClientError> {
    engine
        .write_app_document(
            "kitchen.recipes",
            id,
            read_revision,
            AppDataWrite::Value(value),
        )
        .await
}

async fn held_recipe(engine: &SyncEngine) -> Value {
    query(
        engine,
        "SELECT revision, json_extract(value, '$.title') AS title FROM kitchen.recipes",
    )
    .await
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_documents_sync_beside_rows_and_keep_their_history() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let url = format!("http://{address}");
    let first = signed_in(&url, data, "first", true).await;
    let second = signed_in(&url, data, "second", false).await;
    let first_device = first.get_state().await.session.unwrap().device_id;

    let mut invalid = recipe("Soft onions");
    invalid["steps"][1]["text"] = json!("Add the {garlic}.");
    let refused = write_recipe(&first, None, None, invalid).await.unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("steps[1].text: {garlic} does not match any ingredient id"),
        "{refused}"
    );

    let id = write_recipe(&first, None, None, recipe("Soft onions"))
        .await
        .unwrap();
    write_recipe(&first, Some(&id), Some(1), recipe("Golden onions"))
        .await
        .unwrap();
    second
        .write_app_data(
            "kitchen.pantry",
            None,
            AppDataWrite::Value(json!({"name": "Onion", "category": "Vegetables"})),
        )
        .await
        .unwrap();
    let recipes_on_hand = "SELECT r.id, r.revision, json_extract(r.value, '$.title') AS title,
                json_type(r.value, '$.cuisine') AS cuisine,
                json_extract(r.value, '$.ingredients[0].scales') AS scales,
                json_extract(p.value, '$.name') AS on_hand
         FROM kitchen.recipes r, json_each(r.value, '$.ingredients') i
         JOIN kitchen.pantry p ON json_extract(p.value, '$.name') = json_extract(i.value, '$.name')";
    let expected = json!([{
        "id": id,
        "revision": 2,
        "title": "Golden onions",
        "cuisine": null,
        "scales": 1,
        "on_hand": "Onion"
    }]);
    eventually(
        "both devices join the revised recipe with the pantry",
        async || {
            query(&first, recipes_on_hand).await == expected
                && query(&second, recipes_on_hand).await == expected
        },
    )
    .await;

    let history = second
        .app_document_history("kitchen.recipes", &id)
        .await
        .unwrap();
    assert_eq!(
        history
            .iter()
            .map(|revision| (
                revision.revision,
                revision.device_id.as_str(),
                revision.deleted
            ))
            .collect::<Vec<_>>(),
        [
            (1, first_device.as_str(), false),
            (2, first_device.as_str(), false)
        ]
    );
    assert_eq!(
        second
            .app_document_revision("kitchen.recipes", &id, 1)
            .await
            .unwrap()["title"],
        "Soft onions"
    );

    first
        .write_app_document("kitchen.recipes", Some(&id), Some(2), AppDataWrite::Delete)
        .await
        .unwrap();
    eventually("the second device drops the deleted recipe", async || {
        query(&second, "SELECT id FROM kitchen.recipes").await == json!([])
    })
    .await;
    let history = second
        .app_document_history("kitchen.recipes", &id)
        .await
        .unwrap();
    assert_eq!(
        history
            .iter()
            .map(|revision| (revision.revision, revision.deleted))
            .collect::<Vec<_>>(),
        [(1, false), (2, false), (3, true)]
    );
    assert_eq!(
        second
            .app_document_revision("kitchen.recipes", &id, 2)
            .await
            .unwrap()["title"],
        "Golden onions"
    );
    first.stop_session_work().await;
    second.stop_session_work().await;
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_document_writes_fail_offline_while_the_local_copy_stays_readable() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let phone = signed_in(&format!("http://{address}"), data, "phone", true).await;
    let id = write_recipe(&phone, None, None, recipe("Soft onions"))
        .await
        .unwrap();
    let material = phone.session_resume_material().await.unwrap();
    phone.clear_local_session().await;

    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unreachable = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let offline = SyncEngine::new_with_data_dir(&unreachable, data.join("phone"));
    offline
        .resume_saved_session(material, "app-data-test", "phone", true)
        .await
        .unwrap();
    assert!(offline.get_state().await.offline);
    assert_eq!(
        query(
            &offline,
            "SELECT id, json_extract(value, '$.title') AS title FROM kitchen.recipes"
        )
        .await,
        json!([{"id": id, "title": "Soft onions"}])
    );
    assert!(matches!(
        write_recipe(&offline, Some(&id), Some(1), recipe("Golden onions")).await,
        Err(ClientError::Offline)
    ));
    assert!(matches!(
        write_recipe(&offline, None, None, recipe("Raw onions")).await,
        Err(ClientError::Offline)
    ));
    offline.stop_session_work().await;
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_a_document_saved_from_an_older_revision_is_refused() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let url = format!("http://{address}");
    let first = signed_in(&url, data, "first", true).await;
    let second = signed_in(&url, data, "second", false).await;

    let id = write_recipe(&first, None, None, recipe("Soft onions"))
        .await
        .unwrap();
    eventually("the second device holds revision 1", async || {
        held_recipe(&second).await == json!([{"revision": 1, "title": "Soft onions"}])
    })
    .await;
    write_recipe(&first, Some(&id), Some(1), recipe("Golden onions"))
        .await
        .unwrap();
    eventually("the second device holds revision 2", async || {
        held_recipe(&second).await == json!([{"revision": 2, "title": "Golden onions"}])
    })
    .await;

    let stale = write_recipe(&second, Some(&id), Some(1), recipe("Raw onions"))
        .await
        .unwrap_err();
    assert!(
        stale
            .to_string()
            .contains("read it again and reapply the change"),
        "{stale}"
    );
    let unread = write_recipe(&second, Some(&id), None, recipe("Raw onions"))
        .await
        .unwrap_err();
    assert!(
        unread
            .to_string()
            .contains("needs the revision it was read at"),
        "{unread}"
    );
    assert_eq!(
        held_recipe(&second).await,
        json!([{"revision": 2, "title": "Golden onions"}])
    );

    write_recipe(&second, Some(&id), Some(2), recipe("Caramelised onions"))
        .await
        .unwrap();
    eventually("the first device receives revision 3", async || {
        held_recipe(&first).await == json!([{"revision": 3, "title": "Caramelised onions"}])
    })
    .await;
    first.stop_session_work().await;
    second.stop_session_work().await;
}
