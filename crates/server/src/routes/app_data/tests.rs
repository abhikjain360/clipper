use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    middleware,
};
use clipper_core::{
    crypto::{self, app_data::*},
    models::POSTCARD_CONTENT_TYPE,
};
use sea_orm::Database;
use tempfile::TempDir;
use tokio::sync::broadcast::error::TryRecvError;
use tower::ServiceExt;

use super::*;
use crate::{
    config::ServerConfig,
    entity::{access_keys, event_log, users},
    secret::ServerSecrets,
    secret_storage,
};

struct TestDevice {
    auth: AuthInfo,
    secret: [u8; 32],
}

async fn test_state(rows: u64, bytes: usize) -> (AppState, TempDir) {
    let mut options = sea_orm::ConnectOptions::new("sqlite::memory:");
    options.max_connections(1);
    state_with(options, tempfile::tempdir().expect("tempdir"), rows, bytes).await
}

async fn file_backed_state(rows: u64, bytes: usize) -> (AppState, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut options = sea_orm::ConnectOptions::new(format!(
        "sqlite:{}?mode=rwc",
        dir.path().join("test.db").display()
    ));
    options.max_connections(4);
    state_with(options, dir, rows, bytes).await
}

async fn state_with(
    options: sea_orm::ConnectOptions,
    dir: TempDir,
    rows: u64,
    bytes: usize,
) -> (AppState, TempDir) {
    let db = Database::connect(options).await.expect("database");
    let mut config = ServerConfig::default();
    config.server.data_dir = dir.path().to_path_buf();
    config.limits.max_user_app_data_rows = rows;
    config.limits.max_app_data_ciphertext_bytes = bytes;
    config.rate_limit.api_per_user_per_minute = 1;
    let state = AppState::open_with_db_and_config(db, config, ServerSecrets::test_fixture())
        .await
        .expect("state");
    (state, dir)
}

async fn add_user(state: &AppState) -> TestDevice {
    let user_id = Uuid::now_v7();
    let now = Utc::now().to_rfc3339();
    let access_key_hash = user_id.to_string();
    access_keys::ActiveModel {
        key_hash: Set(access_key_hash.clone()),
        created_at: Set(now.clone()),
        expires_at: Set(None),
        used_at: Set(Some(now.clone())),
        used_by_user_id: Set(Some(user_id)),
    }
    .insert(state.db())
    .await
    .expect("access key");
    users::ActiveModel {
        id: Set(user_id),
        username: Set(user_id.as_simple().to_string()),
        opaque_password_file: Set(secret_storage::wrap_opaque_password_file(
            state.secrets(),
            user_id,
            &[2],
        )
        .expect("wrap password")),
        encryption_salt: Set(
            secret_storage::wrap_encryption_salt(state.secrets(), user_id, &[3])
                .expect("wrap salt"),
        ),
        access_key_hash: Set(access_key_hash),
        created_at: Set(now.clone()),
        updated_at: Set(now),
        storage_bytes: Set(0),
        object_count: Set(0),
    }
    .insert(state.db())
    .await
    .expect("user");
    add_device(state, user_id).await
}

async fn add_device(state: &AppState, user_id: Uuid) -> TestDevice {
    let device_id = Uuid::now_v7();
    let secret = crypto::generate_device_signing_secret_key();
    let now = Utc::now().to_rfc3339();
    devices::ActiveModel {
        id: Set(device_id),
        user_id: Set(user_id),
        name: Set("test device".into()),
        platform: Set("test".into()),
        signing_public_key: Set(crypto::device_signing_public_key(&secret).to_vec()),
        created_at: Set(now.clone()),
        updated_at: Set(now.clone()),
        last_seen_at: Set(now),
    }
    .insert(state.db())
    .await
    .expect("device");
    TestDevice {
        auth: AuthInfo {
            user_id,
            device_id,
            session_id: Uuid::now_v7(),
        },
        secret,
    }
}

fn signed_change(
    device: &TestDevice,
    row_key: [u8; 32],
    replaces_revision: u64,
    deleted: bool,
    plaintext: &[u8],
) -> AppDataChange {
    let revision = replaces_revision + 1;
    let (nonce, ciphertext) = encrypt_app_data_value(
        &derive_app_data_value_key(&[7; 32]),
        &row_key,
        revision,
        plaintext,
    )
    .expect("encrypt");
    let mut change = AppDataChange {
        row_key: row_key.to_vec(),
        revision,
        replaces_revision,
        deleted,
        nonce,
        ciphertext,
        device_id: device.auth.device_id.into(),
        signature: Vec::new(),
    };
    change.signature = sign_app_data_change(&device.secret, &change).expect("sign");
    change
}

async fn write(
    state: &AppState,
    device: &TestDevice,
    changes: Vec<AppDataChange>,
) -> Vec<AppDataChangeResult> {
    let bytes = postcard::to_allocvec(&AppDataChangesRequest { changes }).expect("encode request");
    let request = postcard::from_bytes(&bytes).expect("decode request");
    let response = post_changes(
        State(state.clone()),
        Extension(device.auth.clone()),
        Postcard::validated(request).expect("valid batch"),
    )
    .await
    .expect("write")
    .0;
    let bytes = postcard::to_allocvec(&response).expect("encode results");
    postcard::from_bytes::<AppDataChangesResponse>(&bytes)
        .expect("decode results")
        .results
}

async fn page(state: &AppState, device: &TestDevice, after: i64, limit: u64) -> AppDataChangesPage {
    let response = get_changes(
        State(state.clone()),
        Extension(device.auth.clone()),
        Query(AppDataChangesQuery { after, limit }),
    )
    .await
    .expect("page")
    .0;
    postcard::from_bytes(&postcard::to_allocvec(&response).expect("encode page"))
        .expect("decode page")
}

fn accepted(result: &AppDataChangeResult) -> i64 {
    let AppDataChangeResult::Accepted { sequence } = result else {
        panic!("expected acceptance, got {result:?}")
    };
    *sequence
}

fn refused(reason: AppDataChangeRefusal) -> AppDataChangeResult {
    AppDataChangeResult::Refused { reason }
}

#[tokio::test]
async fn accepts_new_rows_and_edits_and_returns_current_state_on_conflict() {
    let (state, _dir) = test_state(10, 65552).await;
    let device = add_user(&state).await;
    let other = add_device(&state, device.auth.user_id).await;
    let envelope = AppDataValueEnvelope {
        collection: "gym.sets".into(),
        row_id: Uuid::now_v7().into(),
        schema_version: 1,
        written_at: Utc::now().to_rfc3339(),
        deleted: false,
        value: Some(serde_json::json!({"reps": 8})),
    };
    let row_key = app_data_row_key(
        &derive_app_data_row_key_key(&[7; 32]),
        &envelope.collection,
        envelope.row_id,
    );
    let first = signed_change(
        &device,
        row_key,
        0,
        false,
        &serde_json::to_vec(&envelope).expect("envelope"),
    );
    let first_sequence = accepted(&write(&state, &device, vec![first.clone()]).await[0]);
    let stored = page(&state, &device, 0, 500).await.rows.remove(0);
    assert_eq!(stored.sequence, first_sequence);
    assert_eq!(stored.signature, first.signature);
    assert_eq!(
        stored.device_signing_public_key,
        Some(crypto::device_signing_public_key(&device.secret).to_vec())
    );
    let plaintext = decrypt_app_data_value(
        &derive_app_data_value_key(&[7; 32]),
        &row_key,
        1,
        &stored.nonce,
        &stored.ciphertext,
    )
    .expect("decrypt");
    assert_eq!(
        serde_json::from_slice::<AppDataValueEnvelope>(&plaintext).expect("envelope"),
        envelope
    );
    assert!(
        decrypt_app_data_value(
            &derive_app_data_value_key(&[7; 32]),
            &row_key,
            2,
            &stored.nonce,
            &stored.ciphertext
        )
        .is_err()
    );
    assert!(
        decrypt_app_data_value(
            &derive_app_data_value_key(&[7; 32]),
            &[0; 32],
            1,
            &stored.nonce,
            &stored.ciphertext
        )
        .is_err()
    );
    assert_eq!(
        write(
            &state,
            &other,
            vec![signed_change(&other, row_key, 0, false, b"other")]
        )
        .await,
        vec![AppDataChangeResult::Conflict {
            current: Some(stored)
        }]
    );
    let next = signed_change(&other, row_key, 1, false, b"updated");
    let next_sequence = accepted(&write(&state, &other, vec![next.clone()]).await[0]);
    assert!(next_sequence > first_sequence);
    let current = page(&state, &device, 0, 500).await.rows.remove(0);
    assert_eq!(current.revision, 2);
    assert_eq!(current.device_id, Some(other.auth.device_id.into()));
    assert_eq!(
        current.device_signing_public_key,
        Some(crypto::device_signing_public_key(&other.secret).to_vec())
    );
    assert_eq!(current.ciphertext, next.ciphertext);
    assert_eq!(
        write(&state, &device, vec![first]).await,
        vec![AppDataChangeResult::Conflict {
            current: Some(current)
        }]
    );
    assert_eq!(
        write(
            &state,
            &device,
            vec![signed_change(&device, [99; 32], 2, false, b"missing")]
        )
        .await,
        vec![AppDataChangeResult::Conflict { current: None }]
    );
}

#[tokio::test]
async fn refuses_bad_signatures_and_other_devices_without_losing_valid_changes() {
    let (state, _dir) = test_state(10, 65552).await;
    let device = add_user(&state).await;
    let other = add_device(&state, device.auth.user_id).await;
    let valid = signed_change(&device, [1; 32], 0, false, b"value");
    let mut tampered = Vec::new();
    let mut row = valid.clone();
    row.row_key[0] ^= 1;
    tampered.push(row);
    let mut row = valid.clone();
    row.nonce[0] ^= 1;
    tampered.push(row);
    let mut row = valid.clone();
    row.ciphertext[0] ^= 1;
    tampered.push(row);
    let mut row = valid.clone();
    row.revision = 2;
    row.replaces_revision = 1;
    tampered.push(row);
    let mut row = valid.clone();
    row.deleted = true;
    tampered.push(row);
    let mut row = valid.clone();
    row.signature[0] ^= 1;
    tampered.push(row);
    let mut row = signed_change(&other, [2; 32], 0, false, b"other");
    row.device_id = device.auth.device_id.into();
    tampered.push(row);
    assert!(
        write(&state, &device, tampered)
            .await
            .iter()
            .all(|result| *result == refused(AppDataChangeRefusal::BadSignature))
    );
    let results = write(
        &state,
        &device,
        vec![signed_change(&other, [2; 32], 0, false, b"other"), valid],
    )
    .await;
    assert_eq!(results[0], refused(AppDataChangeRefusal::Malformed));
    accepted(&results[1]);
    assert_eq!(page(&state, &device, 0, 500).await.rows.len(), 1);
}

#[tokio::test]
async fn refuses_malformed_changes_individually() {
    let (state, _dir) = test_state(10, 65552).await;
    let device = add_user(&state).await;
    let valid = signed_change(&device, [1; 32], 0, false, b"value");
    let mut changes = Vec::new();
    let mut row = valid.clone();
    row.row_key.pop();
    changes.push(row);
    let mut row = valid.clone();
    row.signature.pop();
    changes.push(row);
    let mut row = valid.clone();
    row.nonce.pop();
    changes.push(row);
    let mut row = valid.clone();
    row.ciphertext = vec![0; 15];
    changes.push(row);
    let mut row = valid.clone();
    row.revision = 3;
    changes.push(row);
    let mut row = valid.clone();
    row.revision = 0;
    changes.push(row);
    let mut row = valid.clone();
    row.revision = u64::MAX;
    row.replaces_revision = u64::MAX;
    changes.push(row);
    let mut row = valid.clone();
    row.revision = i64::MAX as u64 + 1;
    row.replaces_revision = i64::MAX as u64;
    changes.push(row);
    assert!(
        write(&state, &device, changes)
            .await
            .iter()
            .all(|result| *result == refused(AppDataChangeRefusal::Malformed))
    );
    assert!(page(&state, &device, 0, 500).await.rows.is_empty());
}

#[tokio::test]
async fn quotas_include_delete_markers_and_allow_existing_rows_to_change() {
    let (state, _dir) = test_state(2, 65552).await;
    let device = add_user(&state).await;
    let results = write(
        &state,
        &device,
        vec![
            signed_change(&device, [1; 32], 0, false, b"one"),
            signed_change(&device, [2; 32], 0, true, b""),
            signed_change(&device, [3; 32], 0, false, b"three"),
        ],
    )
    .await;
    accepted(&results[0]);
    accepted(&results[1]);
    assert_eq!(results[2], refused(AppDataChangeRefusal::OverQuota));
    accepted(
        &write(
            &state,
            &device,
            vec![signed_change(&device, [1; 32], 1, true, b"")],
        )
        .await[0],
    );
    assert_eq!(
        write(
            &state,
            &device,
            vec![signed_change(&device, [3; 32], 0, true, b"")]
        )
        .await[0],
        refused(AppDataChangeRefusal::OverQuota)
    );
    accepted(
        &write(
            &state,
            &device,
            vec![signed_change(&device, [2; 32], 1, false, b"restored")],
        )
        .await[0],
    );
    assert_eq!(page(&state, &device, 0, 500).await.rows.len(), 2);
}

#[tokio::test]
async fn enforces_ciphertext_size_and_keeps_other_changes_in_the_batch() {
    let (state, _dir) = test_state(10, 32).await;
    let device = add_user(&state).await;
    let results = write(
        &state,
        &device,
        vec![
            signed_change(&device, [1; 32], 0, false, &[0; 17]),
            signed_change(&device, [2; 32], 0, false, &[0; 16]),
            signed_change(&device, [3; 32], 0, true, b""),
        ],
    )
    .await;
    assert_eq!(results[0], refused(AppDataChangeRefusal::TooLarge));
    accepted(&results[1]);
    accepted(&results[2]);
    assert_eq!(page(&state, &device, 0, 500).await.rows.len(), 2);
}

#[tokio::test]
async fn pages_current_rows_in_sequence_order_and_isolates_users() {
    let (state, _dir) = test_state(3, 65552).await;
    let device = add_user(&state).await;
    let other = add_user(&state).await;
    let results = write(
        &state,
        &device,
        (1..=3)
            .map(|key| signed_change(&device, [key; 32], 0, false, b"value"))
            .collect(),
    )
    .await;
    let sequences: Vec<_> = results.iter().map(accepted).collect();
    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    let other_seq = accepted(
        &write(
            &state,
            &other,
            vec![signed_change(&other, [1; 32], 0, false, b"private")],
        )
        .await[0],
    );
    let first = page(&state, &device, 0, 2).await;
    assert_eq!(first.newest_sequence, sequences[2]);
    assert_eq!(
        first
            .rows
            .iter()
            .map(|row| row.sequence)
            .collect::<Vec<_>>(),
        sequences[..2]
    );
    let second = page(&state, &device, first.rows[1].sequence, 2).await;
    assert_eq!(second.rows.len(), 1);
    assert_eq!(second.rows[0].sequence, sequences[2]);
    let empty = page(&state, &device, sequences[2], 500).await;
    assert!(empty.rows.is_empty());
    assert_eq!(empty.newest_sequence, sequences[2]);
    let other_page = page(&state, &other, 0, 500).await;
    assert_eq!(other_page.rows.len(), 1);
    assert_eq!(other_page.newest_sequence, other_seq);
    assert_eq!(
        write(
            &state,
            &other,
            vec![signed_change(&other, [2; 32], 1, false, b"missing")]
        )
        .await,
        vec![AppDataChangeResult::Conflict { current: None }]
    );
    let updated = accepted(
        &write(
            &state,
            &device,
            vec![signed_change(&device, [1; 32], 1, true, b"")],
        )
        .await[0],
    );
    let current = page(&state, &device, 0, 500).await;
    assert_eq!(
        current
            .rows
            .iter()
            .map(|row| row.sequence)
            .collect::<Vec<_>>(),
        vec![sequences[1], sequences[2], updated]
    );
    assert!(current.rows[2].deleted);
    assert_eq!(current.newest_sequence, updated);
    for query in [
        AppDataChangesQuery {
            after: -1,
            limit: 1,
        },
        AppDataChangesQuery { after: 0, limit: 0 },
        AppDataChangesQuery {
            after: 0,
            limit: 501,
        },
    ] {
        assert_eq!(
            get_changes(
                State(state.clone()),
                Extension(device.auth.clone()),
                Query(query)
            )
            .await
            .expect_err("invalid query")
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn delete_markers_survive_device_removal_and_prevent_stale_writes() {
    let (state, _dir) = test_state(10, 65552).await;
    let device = add_user(&state).await;
    let other = add_device(&state, device.auth.user_id).await;
    let created = accepted(
        &write(
            &state,
            &device,
            vec![signed_change(&device, [1; 32], 0, false, b"value")],
        )
        .await[0],
    );
    let deleted = accepted(
        &write(
            &state,
            &device,
            vec![signed_change(&device, [1; 32], 1, true, b"")],
        )
        .await[0],
    );
    assert!(deleted > created);
    devices::Entity::delete_by_id(device.auth.device_id)
        .filter(devices::Column::UserId.eq(device.auth.user_id))
        .exec(state.db())
        .await
        .expect("delete device");
    let row = page(&state, &other, created, 500).await.rows.remove(0);
    assert!(row.deleted);
    assert_eq!(row.revision, 2);
    assert_eq!(row.device_id, None);
    assert_eq!(row.device_signing_public_key, None);
    assert_eq!(
        decrypt_app_data_value(
            &derive_app_data_value_key(&[7; 32]),
            &[1; 32],
            2,
            &row.nonce,
            &row.ciphertext
        )
        .expect("delete marker decrypts"),
        b""
    );
    assert_eq!(
        write(
            &state,
            &other,
            vec![signed_change(&other, [1; 32], 0, false, b"stale")]
        )
        .await,
        vec![AppDataChangeResult::Conflict { current: Some(row) }]
    );
}

#[tokio::test]
async fn row_sequences_share_the_event_counter_and_survive_restart() {
    let (state, _dir) = test_state(10, 65552).await;
    let device = add_user(&state).await;
    let first = accepted(
        &write(
            &state,
            &device,
            vec![signed_change(&device, [1; 32], 0, false, b"one")],
        )
        .await[0],
    );
    let object_sequence = with_txn(state.db(), "test object event", async |txn| {
        let sequence = state.next_event_seq();
        event_log::ActiveModel {
            seq: Set(sequence),
            user_id: Set(device.auth.user_id),
            event_type: Set("created".into()),
            object_kind: Set("file".into()),
            object_id: Set(Uuid::now_v7()),
            created_at: Set(Utc::now().to_rfc3339()),
        }
        .insert(txn)
        .await
        .map_err(database_error)?;
        Ok(sequence)
    })
    .await
    .expect("event");
    let second = accepted(
        &write(
            &state,
            &device,
            vec![signed_change(&device, [2; 32], 0, false, b"two")],
        )
        .await[0],
    );
    assert!(first < object_sequence && object_sequence < second);
    let future = second + 1_000_000_000_000;
    app_data_rows::Entity::update_many()
        .col_expr(
            app_data_rows::Column::Sequence,
            sea_orm::sea_query::Expr::value(future),
        )
        .filter(app_data_rows::Column::UserId.eq(device.auth.user_id))
        .filter(app_data_rows::Column::RowKey.eq(vec![2; 32]))
        .exec(state.db())
        .await
        .expect("future sequence");
    let restarted = AppState::open_with_db_and_config(
        state.db().clone(),
        state.config().clone(),
        ServerSecrets::test_fixture(),
    )
    .await
    .expect("restart");
    assert_eq!(
        crate::ws::get_latest_seq(&restarted, device.auth.user_id)
            .await
            .expect("watermark"),
        future
    );
    assert!(
        accepted(
            &write(
                &restarted,
                &device,
                vec![signed_change(&device, [2; 32], 1, true, b"")]
            )
            .await[0]
        ) > future
    );
}

#[tokio::test]
async fn broadcasts_changes_to_the_users_other_devices_after_commit() {
    let (state, _dir) = test_state(10, 65552).await;
    let device = add_user(&state).await;
    let other = add_device(&state, device.auth.user_id).await;
    let stranger = add_user(&state).await;
    let mut rx = state.subscribe_app_data_changes(device.auth.user_id);
    let mut stranger_rx = state.subscribe_app_data_changes(stranger.auth.user_id);
    let results = write(
        &state,
        &device,
        vec![
            signed_change(&device, [1; 32], 0, false, b"one"),
            signed_change(&device, [2; 32], 0, true, b""),
        ],
    )
    .await;
    let change = rx.try_recv().expect("broadcast");
    assert_eq!(change.sequence, accepted(&results[1]));
    assert_eq!(
        page(&state, &device, 0, 500).await.newest_sequence,
        change.sequence
    );
    assert!(!crate::ws::should_forward_app_data_change(
        &change,
        device.auth.device_id,
        0
    ));
    assert!(crate::ws::should_forward_app_data_change(
        &change,
        other.auth.device_id,
        0
    ));
    assert!(!crate::ws::should_forward_app_data_change(
        &change,
        other.auth.device_id,
        change.sequence
    ));
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    assert!(matches!(stranger_rx.try_recv(), Err(TryRecvError::Empty)));
    write(
        &state,
        &device,
        vec![signed_change(&device, [1; 32], 0, false, b"conflict")],
    )
    .await;
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
}

#[tokio::test]
async fn concurrent_writes_accept_one_revision_and_conflict_with_the_committed_row() {
    let (state, _dir) = file_backed_state(10, 65552).await;
    let device = add_user(&state).await;
    let other = add_device(&state, device.auth.user_id).await;
    let (one, two) = tokio::join!(
        write(
            &state,
            &device,
            vec![signed_change(&device, [1; 32], 0, false, b"one")]
        ),
        write(
            &state,
            &other,
            vec![signed_change(&other, [1; 32], 0, false, b"two")]
        )
    );
    let current = page(&state, &device, 0, 500).await.rows.remove(0);
    let results = [one[0].clone(), two[0].clone()];
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, AppDataChangeResult::Accepted { .. }))
            .count(),
        1
    );
    assert!(results.contains(&AppDataChangeResult::Conflict {
        current: Some(current)
    }));
}

#[tokio::test]
async fn accepts_a_full_size_batch_as_one_rate_limited_request() {
    let (state, _dir) = test_state(200, 65552).await;
    let device = add_user(&state).await;
    let request = AppDataChangesRequest {
        changes: (0..200)
            .map(|key| signed_change(&device, [key; 32], 0, false, &vec![0; 65536]))
            .collect(),
    };
    let app = Router::new()
        .route("/api/app-data/changes", change_routes(&state))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::rate_limit::user_rate_limit_middleware,
        ))
        .layer(Extension(device.auth.clone()))
        .with_state(state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/app-data/changes")
                .header("content-type", POSTCARD_CONTENT_TYPE)
                .body(Body::from(
                    postcard::to_allocvec(&request).expect("request"),
                ))
                .expect("http request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let response: AppDataChangesResponse = postcard::from_bytes(&body).expect("response");
    assert_eq!(response.results.len(), 200);
    assert!(
        response
            .results
            .iter()
            .all(|result| matches!(result, AppDataChangeResult::Accepted { .. }))
    );
    let response = app
        .oneshot(
            Request::get("/api/app-data/changes?after=0&limit=500")
                .body(Body::empty())
                .expect("http request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(page(&state, &device, 0, 500).await.rows.len(), 200);
    let mut too_many = request;
    too_many.changes.push(too_many.changes[0].clone());
    assert!(Postcard::validated(too_many).is_err());
}
