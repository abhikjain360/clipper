use std::{
    net::SocketAddr,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use clipper_gym::{Muscle, Recovery};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use uuid::Uuid;

use super::{
    schedule_integration_tests::{start_server, wait_for},
    *,
};
use crate::app_data::AppDataKeys;

const TABLES: [&str; 6] = [
    "gym.exercises",
    "gym.workouts",
    "gym.sessions",
    "gym.sets",
    "gym.body_weight",
    "gym.recovery",
];

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn gym_writes_finish_while_upload_is_held() {
    crate::ensure_crypto_provider();
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let directory = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(directory.path()).await;
    let proxy = TestProxy::start(address).await;
    let engine = signed_in(&proxy.url, directory.path(), "lifter", true).await;
    let mut held = proxy.hold_sends();
    let started = std::time::Instant::now();
    let exercise = engine
        .gym_save_exercise(
            None,
            clipper_app_types::GymExerciseInput {
                name: "Bench".into(),
                muscles: vec![clipper_app_types::GymMuscleShare {
                    muscle: Muscle::Chest,
                    share: 1.0,
                }],
                archived: false,
            },
        )
        .await
        .unwrap();
    tracing::info!(
        elapsed_us = started.elapsed().as_micros(),
        action = "save_exercise"
    );
    let upload = tokio::time::timeout(Duration::from_secs(5), held.recv())
        .await
        .unwrap()
        .unwrap();
    let started = std::time::Instant::now();
    let session = engine.gym_start_session(None).await.unwrap();
    tracing::info!(
        elapsed_us = started.elapsed().as_micros(),
        action = "start_session"
    );
    let started = std::time::Instant::now();
    engine.gym_add_exercise(&session, &exercise).await.unwrap();
    tracing::info!(
        elapsed_us = started.elapsed().as_micros(),
        action = "add_exercise"
    );
    let values = clipper_app_types::GymSetValues {
        weight_kg: Some(62.5),
        reps: Some(8),
        reps_in_reserve: Some(2),
    };
    let started = std::time::Instant::now();
    let view = tokio::time::timeout(
        Duration::from_secs(1),
        engine.gym_complete_set(
            &session,
            &exercise,
            clipper_gym::SetKind::Working,
            1,
            values,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    tracing::info!(
        elapsed_us = started.elapsed().as_micros(),
        action = "complete_set"
    );
    assert_eq!(view.exercises[0].sets.len(), 1);
    assert!(
        engine
            .gym_complete_set(
                &session,
                &exercise,
                clipper_gym::SetKind::Working,
                1,
                values
            )
            .await
            .is_err()
    );
    let started = std::time::Instant::now();
    tokio::time::timeout(
        Duration::from_secs(1),
        engine.gym_add_set(&session, &exercise, clipper_gym::SetKind::Working, values),
    )
    .await
    .unwrap()
    .unwrap();
    tracing::info!(
        elapsed_us = started.elapsed().as_micros(),
        action = "add_set"
    );
    assert_eq!(
        engine.gym_session(&session).await.unwrap().exercises[0]
            .sets
            .len(),
        2
    );
    assert!(pending(&engine).await >= 4);
    proxy.stop_holding_sends();
    upload.send(()).unwrap();
    eventually("gym writes upload after release", async || {
        pending(&engine).await == 0
    })
    .await;
    engine.stop_session_work().await;
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn schedule_done_marks_sync_and_undo_wins_an_offline_conflict() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let url = format!("http://{address}");
    let proxy = TestProxy::start(address).await;
    let phone = signed_in(&url, data, "phone", true).await;
    let desktop = signed_in(&proxy.url, data, "desktop", false).await;
    let item_id = Uuid::new_v4();
    let mark = json!({"item_id": item_id, "occurrence_key": "date:2026-10-08", "done": true});
    let id = write(&phone, "schedule.done", None, mark.clone()).await;
    let expected: clipper_schedule::DoneMark = serde_json::from_value(mark.clone()).unwrap();
    assert_eq!(id, expected.row_id().to_string());
    eventually("the desktop receives the phone's done mark", async || {
        field(&desktop, "schedule.done", &id, "done").await == json!(1)
    })
    .await;
    let next = write(
        &phone,
        "schedule.done",
        None,
        json!({"item_id": item_id, "occurrence_key": "date:2026-10-09", "done": true}),
    )
    .await;
    assert_ne!(id, next);
    proxy.go_offline(&desktop).await;
    write(&phone, "schedule.done", None, mark.clone()).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let undo = json!({"item_id": item_id, "occurrence_key": "date:2026-10-08", "done": false});
    assert_eq!(write(&desktop, "schedule.done", None, undo).await, id);
    proxy.go_online();
    eventually(
        "undo reaches both devices and wins the older mark",
        async || {
            field(&phone, "schedule.done", &id, "done").await == json!(0)
                && field(&desktop, "schedule.done", &id, "done").await == json!(0)
                && pending(&desktop).await == 0
        },
    )
    .await;
    assert_eq!(
        field(&phone, "schedule.done", &next, "done").await,
        json!(1)
    );
    phone.stop_session_work().await;
    desktop.stop_session_work().await;
}

type HeldSends = tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>;

struct TestProxy {
    url: String,
    online: Arc<AtomicBool>,
    block_websocket: Arc<AtomicBool>,
    held_sends: Arc<std::sync::Mutex<Option<HeldSends>>>,
    requests: Arc<std::sync::Mutex<Vec<&'static str>>>,
    connections: Arc<std::sync::Mutex<Vec<JoinHandle<()>>>>,
    listener: JoinHandle<()>,
}

impl TestProxy {
    async fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let online = Arc::new(AtomicBool::new(true));
        let block_websocket = Arc::new(AtomicBool::new(false));
        let held_sends = Arc::new(std::sync::Mutex::new(None::<HeldSends>));
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let connections = Arc::new(std::sync::Mutex::new(Vec::new()));
        let accepting = Arc::clone(&online);
        let holding = Arc::clone(&held_sends);
        let logging = Arc::clone(&requests);
        let tracked = Arc::clone(&connections);
        let blocking = Arc::clone(&block_websocket);
        let listener = tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    return;
                };
                if !accepting.load(Ordering::SeqCst) {
                    continue;
                }
                let holding = Arc::clone(&holding);
                let logging = Arc::clone(&logging);
                let blocking = Arc::clone(&blocking);
                let connection = tokio::spawn(async move {
                    let Ok(server) = TcpStream::connect(upstream).await else {
                        return;
                    };
                    let (mut client_read, mut client_write) = client.into_split();
                    let (mut server_read, mut server_write) = server.into_split();
                    let download = tokio::spawn(async move {
                        let _ = tokio::io::copy(&mut server_read, &mut client_write).await;
                    });
                    let mut buffer = vec![0_u8; 65536];
                    loop {
                        let read = match client_read.read(&mut buffer).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => read,
                        };
                        let chunk = &buffer[..read];
                        if blocking.load(Ordering::SeqCst) && chunk.starts_with(b"GET /api/ws ") {
                            break;
                        }
                        if chunk.starts_with(b"POST /api/app-data/changes") {
                            logging.lock().unwrap().push("send");
                            let held = holding.lock().unwrap().clone();
                            if let Some(held) = held {
                                let (release, released) = tokio::sync::oneshot::channel();
                                if held.send(release).is_ok() {
                                    let _ = released.await;
                                }
                            }
                        } else if chunk.starts_with(b"GET /api/app-data/changes") {
                            logging.lock().unwrap().push("fetch");
                        }
                        if server_write.write_all(chunk).await.is_err() {
                            break;
                        }
                    }
                    download.abort();
                });
                tracked.lock().unwrap().push(connection);
            }
        });
        Self {
            url,
            online,
            block_websocket,
            held_sends,
            requests,
            connections,
            listener,
        }
    }

    async fn go_offline(&self, engine: &SyncEngine) {
        self.online.store(false, Ordering::SeqCst);
        for connection in self.connections.lock().unwrap().drain(..) {
            connection.abort();
        }
        wait_for(engine, |state| {
            !matches!(state.connection_status, ConnectionStatus::Connected)
        })
        .await;
    }

    fn go_online(&self) {
        self.online.store(true, Ordering::SeqCst);
    }

    fn hold_sends(&self) -> tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<()>> {
        let (held, receiver) = tokio::sync::mpsc::unbounded_channel();
        *self.held_sends.lock().unwrap() = Some(held);
        self.requests.lock().unwrap().clear();
        receiver
    }

    fn stop_holding_sends(&self) {
        *self.held_sends.lock().unwrap() = None;
    }

    fn requests(&self) -> Vec<&'static str> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for TestProxy {
    fn drop(&mut self) {
        self.listener.abort();
        for connection in self.connections.lock().unwrap().drain(..) {
            connection.abort();
        }
    }
}

pub(super) async fn signed_in(
    url: &str,
    data: &Path,
    name: &str,
    register: bool,
) -> Arc<SyncEngine> {
    let engine = SyncEngine::new_with_data_dir(url, data.join(name));
    if register {
        engine
            .register_with_platform(
                "test-invite",
                "app-data-test",
                "local-test-passphrase",
                name,
                "test",
            )
            .await
            .expect("register");
    } else {
        engine
            .login_with_platform("local-test-passphrase", "app-data-test", name, "test")
            .await
            .expect("login");
    }
    wait_for(&engine, |state| {
        matches!(state.connection_status, ConnectionStatus::Connected)
    })
    .await;
    engine
}

pub(super) async fn eventually(description: &str, mut check: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{description} within thirty seconds"));
}

pub(super) async fn query(engine: &SyncEngine, sql: &str) -> Value {
    Value::Array(
        engine
            .query_app_data(sql)
            .await
            .expect("query")
            .into_iter()
            .map(Value::Object)
            .collect(),
    )
}

async fn snapshot(engine: &SyncEngine) -> Vec<Value> {
    let mut tables = Vec::new();
    for table in TABLES {
        tables.push(
            query(
                engine,
                &format!("SELECT id, revision, written_at, value FROM {table} ORDER BY id"),
            )
            .await,
        );
    }
    tables
}

async fn field(engine: &SyncEngine, table: &str, id: &str, path: &str) -> Value {
    let rows = query(
        engine,
        &format!("SELECT json_extract(value, '$.{path}') AS field FROM {table} WHERE id = '{id}'"),
    )
    .await;
    rows.get(0).map_or(Value::Null, |row| row["field"].clone())
}

async fn pending(engine: &SyncEngine) -> u32 {
    engine
        .app_data_status()
        .await
        .expect("status")
        .pending_changes
}

async fn write(engine: &SyncEngine, collection: &str, id: Option<&str>, value: Value) -> String {
    engine
        .write_app_data(collection, id, AppDataWrite::Value(value))
        .await
        .expect("write")
}

async fn delete(engine: &SyncEngine, collection: &str, id: &str) {
    engine
        .write_app_data(collection, Some(id), AppDataWrite::Delete)
        .await
        .expect("delete");
}

fn set(session_id: Uuid, exercise_id: &str, order: u32, reps: u32) -> Value {
    json!({
        "session_id": session_id,
        "exercise_id": exercise_id,
        "order": order,
        "kind": "working",
        "weight_kg": 100.0,
        "reps": reps,
        "reps_in_reserve": 2,
        "completed_at": "2026-10-07T10:00:00Z",
    })
}

fn session(exercise_id: &str, ended_at: Option<&str>, notes: &str) -> Value {
    json!({
        "started_at": "2026-10-07T10:00:00Z",
        "ended_at": ended_at,
        "template_id": null,
        "notes": notes,
        "exercises": [{
            "exercise_id": exercise_id,
            "warm_up_sets": 0,
            "warm_up_rest_seconds": 60,
            "target_sets": 3,
            "target_reps": 5,
            "target_reps_in_reserve": 2,
            "rest_seconds": 180,
            "superset_with_previous": false,
            "skipped": false,
        }],
    })
}

fn exercise(name: &str) -> Value {
    json!({"name": name, "muscles": [{"muscle": "quadriceps", "share": 1.0}], "archived": false})
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_offline_resume_syncs_pending_writes_and_erases_a_revoked_session() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let url = format!("http://{address}");
    let proxy = TestProxy::start(address).await;
    let first = signed_in(&url, data, "first", true).await;
    let phone = signed_in(&proxy.url, data, "phone", false).await;
    let now = chrono::Utc::now();
    let item = ScheduleItem {
        id: clipper_schedule::ScheduleItemId::new(),
        title: "Offline alarm".into(),
        span: ScheduleSpan::Timed {
            start: clipper_schedule::TimedStart::Floating(
                (now + chrono::Duration::hours(1)).naive_utc(),
            ),
            duration: clipper_schedule::BlockDuration::from_minutes(30).unwrap(),
        },
        recurrence: clipper_schedule::Recurrence::Once,
        reference: None,
        alarm: Some(clipper_schedule::AlarmPolicy {
            minutes_before: 0,
            target_device: None,
        }),
        break_reminders: false,
    };
    let schedule_id = phone.create_schedule_item(item.clone()).await.unwrap();
    let row = write(&phone, "gym.exercises", None, exercise("Offline squat")).await;
    eventually("the first device receives the row", async || {
        field(&first, "gym.exercises", &row, "name").await == json!("Offline squat")
    })
    .await;
    let material = phone.session_resume_material().await.unwrap();
    let confirmed_at = material.last_confirmed_at;
    proxy.go_offline(&phone).await;
    phone.clear_local_session().await;
    let offline = SyncEngine::new_with_data_dir(&proxy.url, data.join("phone"));
    offline
        .resume_saved_session(material, "app-data-test", "phone", true)
        .await
        .unwrap();
    assert_eq!(
        offline
            .session_resume_material()
            .await
            .unwrap()
            .last_confirmed_at,
        confirmed_at
    );
    assert_eq!(
        field(&offline, "gym.exercises", &row, "name").await,
        json!("Offline squat")
    );
    assert_eq!(offline.get_state().await.schedule_items[0].id, schedule_id);
    assert_eq!(
        offline.next_alarms(24, "UTC").await.unwrap()[0].label,
        "Offline alarm"
    );
    assert!(matches!(
        offline.update_schedule_item(&schedule_id, item, 1).await,
        Err(ClientError::Offline)
    ));
    let pending_id = write(&offline, "gym.exercises", None, exercise("Pending squat")).await;
    assert_eq!(pending(&offline).await, 1);
    proxy.block_websocket.store(true, Ordering::SeqCst);
    proxy.go_online();
    eventually("pending changes sync on reconnect", async || {
        pending(&offline).await == 0
            && field(&first, "gym.exercises", &pending_id, "name").await == json!("Pending squat")
    })
    .await;
    assert!(!offline.get_state().await.offline);
    assert_ne!(
        offline.get_state().await.connection_status,
        ConnectionStatus::Connected
    );
    assert!(
        offline
            .send_clipboard_payload(TEXT_CLIPBOARD_MIME_TYPE, b"HTTP works without a WebSocket")
            .await
            .is_ok()
    );
    let refreshed = offline.session_resume_material().await.unwrap();
    assert!(refreshed.last_confirmed_at > confirmed_at);
    let device_id = offline.get_state().await.session.unwrap().device_id;
    proxy.go_offline(&offline).await;
    offline.clear_local_session().await;
    first.remove_device(&device_id).await.unwrap();
    let revoked = SyncEngine::new_with_data_dir(&proxy.url, data.join("phone"));
    revoked
        .resume_saved_session(refreshed, "app-data-test", "phone", true)
        .await
        .unwrap();
    write(&revoked, "gym.exercises", None, exercise("Must be erased")).await;
    proxy.go_online();
    eventually("the revoked phone signs out", async || {
        revoked.get_state().await.session.is_none()
    })
    .await;
    assert!(revoked.session_resume_material().await.is_none());
    assert!(
        revoked
            .query_app_data("SELECT id FROM gym.exercises")
            .await
            .is_err()
    );
    assert!(
        revoked
            .local_store
            .app_data_rows()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        revoked
            .local_store
            .app_data_pending_counts()
            .await
            .unwrap()
            .pending,
        0
    );
    first.stop_session_work().await;
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_app_data_syncs_resolves_conflicts_and_rejects_tampered_rows() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let url = format!("http://{address}");
    let proxy = TestProxy::start(address).await;
    let first = signed_in(&url, data, "first", true).await;
    let second = signed_in(&proxy.url, data, "second", false).await;

    assert!(
        first
            .write_app_data("gym.notes", None, AppDataWrite::Value(json!({})))
            .await
            .is_err()
    );
    assert!(
        first
            .write_app_data(
                "gym.body_weight",
                None,
                AppDataWrite::Value(json!({"time": "2026-10-07T07:00:00Z", "kg": -1.0}))
            )
            .await
            .is_err()
    );
    assert!(first.query_app_data("DELETE FROM gym.sets").await.is_err());

    let squat = write(&first, "gym.exercises", None, exercise("Squat")).await;
    let session_id = Uuid::now_v7();
    let logged = write(&first, "gym.sets", None, set(session_id, &squat, 1, 5)).await;
    let chest = write(
        &first,
        "gym.recovery",
        None,
        json!({"muscle": "chest", "recovery_days": 3.0}),
    )
    .await;
    assert_eq!(chest, Recovery::row_id(Muscle::Chest).to_string());
    eventually("the second device receives the first rows", async || {
        field(&second, "gym.sets", &logged, "reps").await == json!(5)
            && field(&second, "gym.recovery", &chest, "recovery_days").await == json!(3.0)
    })
    .await;

    proxy.go_offline(&second).await;
    write(
        &first,
        "gym.recovery",
        None,
        json!({"muscle": "chest", "recovery_days": 4.0}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    write(
        &second,
        "gym.recovery",
        None,
        json!({"muscle": "chest", "recovery_days": 5.0}),
    )
    .await;
    let weighed = write(
        &second,
        "gym.body_weight",
        None,
        json!({"time": "2026-10-07T07:00:00Z", "kg": 80.5}),
    )
    .await;
    assert_eq!(pending(&second).await, 2);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        field(&first, "gym.body_weight", &weighed, "kg").await,
        Value::Null
    );
    proxy.go_online();
    eventually("the later offline recovery edit wins", async || {
        field(&first, "gym.recovery", &chest, "recovery_days").await == json!(5.0)
            && field(&first, "gym.body_weight", &weighed, "kg").await == json!(80.5)
            && pending(&second).await == 0
    })
    .await;
    assert_eq!(
        query(&second, "SELECT count(*) AS rows FROM gym.recovery").await,
        json!([{"rows": 1}])
    );

    proxy.go_offline(&second).await;
    write(
        &second,
        "gym.exercises",
        Some(&squat),
        exercise("Back squat"),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    write(
        &first,
        "gym.exercises",
        Some(&squat),
        exercise("Front squat"),
    )
    .await;
    proxy.go_online();
    eventually("the earlier offline exercise edit loses", async || {
        field(&second, "gym.exercises", &squat, "name").await == json!("Front squat")
            && pending(&second).await == 0
    })
    .await;
    assert_eq!(
        field(&first, "gym.exercises", &squat, "name").await,
        json!("Front squat")
    );

    proxy.go_offline(&second).await;
    delete(&second, "gym.sets", &logged).await;
    write(
        &first,
        "gym.sets",
        Some(&logged),
        set(session_id, &squat, 1, 6),
    )
    .await;
    proxy.go_online();
    eventually("an offline delete wins over an edit", async || {
        field(&first, "gym.sets", &logged, "reps").await == Value::Null
            && pending(&second).await == 0
    })
    .await;

    let second_set = write(&first, "gym.sets", None, set(session_id, &squat, 2, 8)).await;
    eventually("the second device receives the new set", async || {
        field(&second, "gym.sets", &second_set, "reps").await == json!(8)
    })
    .await;
    proxy.go_offline(&second).await;
    write(
        &second,
        "gym.sets",
        Some(&second_set),
        set(session_id, &squat, 2, 9),
    )
    .await;
    delete(&first, "gym.sets", &second_set).await;
    proxy.go_online();
    eventually(
        "a delete on the server wins over an offline edit",
        async || {
            field(&second, "gym.sets", &second_set, "reps").await == Value::Null
                && pending(&second).await == 0
        },
    )
    .await;

    eventually("both devices hold the same rows", async || {
        snapshot(&first).await == snapshot(&second).await
    })
    .await;
    let third = signed_in(&url, data, "third", false).await;
    eventually("a new device receives every row", async || {
        snapshot(&third).await == snapshot(&first).await
    })
    .await;

    let tampered_value = write(
        &first,
        "gym.body_weight",
        None,
        json!({"time": "2026-10-08T07:00:00Z", "kg": 81.0}),
    )
    .await;
    let tampered_signature = write(
        &first,
        "gym.body_weight",
        None,
        json!({"time": "2026-10-09T07:00:00Z", "kg": 81.5}),
    )
    .await;
    eventually("the server holds the rows to tamper with", async || {
        field(&second, "gym.body_weight", &tampered_signature, "kg").await == json!(81.5)
            && field(&second, "gym.body_weight", &tampered_value, "kg").await == json!(81.0)
    })
    .await;
    let keys = AppDataKeys::derive(&first.current_encryption_key().await.unwrap());
    let server = rusqlite::Connection::open(data.join("server/clipper.db")).unwrap();
    server.busy_timeout(Duration::from_secs(5)).unwrap();
    for (id, column) in [
        (&tampered_value, "ciphertext"),
        (&tampered_signature, "signature"),
    ] {
        let row_key = keys
            .row_key("gym.body_weight", id.parse().unwrap())
            .to_vec();
        let mut bytes: Vec<u8> = server
            .query_row(
                &format!("SELECT {column} FROM app_data_rows WHERE row_key = ?1"),
                [&row_key],
                |row| row.get(0),
            )
            .unwrap();
        bytes[0] ^= 1;
        server
            .execute(
                &format!("UPDATE app_data_rows SET {column} = ?1 WHERE row_key = ?2"),
                rusqlite::params![bytes, row_key],
            )
            .unwrap();
    }

    let mut expected = snapshot(&first).await;
    expected[4]
        .as_array_mut()
        .unwrap()
        .retain(|row| row["id"] != json!(tampered_value) && row["id"] != json!(tampered_signature));
    let fourth = signed_in(&url, data, "fourth", false).await;
    eventually("a new device receives every untampered row", async || {
        snapshot(&fourth).await == expected
    })
    .await;
    let status = fourth.app_data_status().await.expect("status");
    assert_eq!(
        status.last_sync_error.as_deref(),
        Some("Rejected 2 app-data changes from the server that failed verification")
    );
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_app_data_refuses_conflicts_that_do_not_move_a_row_forward() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let first = signed_in(&format!("http://{address}"), data, "first", true).await;
    let keys = AppDataKeys::derive(&first.current_encryption_key().await.unwrap());
    let server = rusqlite::Connection::open(data.join("server/clipper.db")).unwrap();
    server.busy_timeout(Duration::from_secs(5)).unwrap();
    let synced = async || pending(&first).await == 0;

    let squat = write(&first, "gym.exercises", None, exercise("Squat")).await;
    eventually("the first revision reaches the server", synced).await;
    let squat_key = keys
        .row_key("gym.exercises", squat.parse().unwrap())
        .to_vec();
    let backup: (i64, i64, Vec<u8>, Vec<u8>, Vec<u8>) = server
        .query_row(
            "SELECT revision, sequence, nonce, ciphertext, signature
             FROM app_data_rows WHERE row_key = ?1",
            [&squat_key],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    write(
        &first,
        "gym.exercises",
        Some(&squat),
        exercise("Front squat"),
    )
    .await;
    eventually("the second revision reaches the server", synced).await;
    server
        .execute(
            "UPDATE app_data_rows
             SET revision = ?1, sequence = ?2, nonce = ?3, ciphertext = ?4, signature = ?5
             WHERE row_key = ?6",
            rusqlite::params![backup.0, backup.1, backup.2, backup.3, backup.4, squat_key],
        )
        .unwrap();

    let weighed = write(
        &first,
        "gym.body_weight",
        None,
        json!({"time": "2026-10-07T07:00:00Z", "kg": 80.5}),
    )
    .await;
    write(
        &first,
        "gym.body_weight",
        Some(&weighed),
        json!({"time": "2026-10-07T07:00:00Z", "kg": 81.0}),
    )
    .await;
    eventually("the edited weighing reaches the server", synced).await;
    server
        .execute(
            "DELETE FROM app_data_rows WHERE row_key = ?1",
            [keys
                .row_key("gym.body_weight", weighed.parse().unwrap())
                .to_vec()],
        )
        .unwrap();

    write(
        &first,
        "gym.exercises",
        Some(&squat),
        exercise("Back squat"),
    )
    .await;
    write(
        &first,
        "gym.body_weight",
        Some(&weighed),
        json!({"time": "2026-10-07T07:00:00Z", "kg": 81.5}),
    )
    .await;
    eventually("both changes are refused", async || {
        let status = first.app_data_status().await.expect("status");
        status.refused_changes == 2 && status.pending_changes == 0
    })
    .await;
    let revision: i64 = server
        .query_row(
            "SELECT revision FROM app_data_rows WHERE row_key = ?1",
            [&squat_key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision, 1);
    assert_eq!(
        field(&first, "gym.exercises", &squat, "name").await,
        json!("Back squat")
    );
    assert!(
        first
            .app_data_status()
            .await
            .expect("status")
            .last_sync_error
            .is_some()
    );
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_app_data_pulls_between_bounded_push_passes() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let proxy = TestProxy::start(address).await;
    let first = signed_in(&proxy.url, data, "first", true).await;
    let second = signed_in(&format!("http://{address}"), data, "second", false).await;
    let squat = write(&first, "gym.exercises", None, exercise("Squat")).await;
    let session_id = Uuid::now_v7();
    let logged = write(&first, "gym.sets", None, set(session_id, &squat, 1, 5)).await;
    eventually("the second device receives the set", async || {
        field(&second, "gym.sets", &logged, "reps").await == json!(5)
    })
    .await;

    let mut held = proxy.hold_sends();
    delete(&first, "gym.sets", &logged).await;
    for reps in 6..12 {
        let release = tokio::time::timeout(Duration::from_secs(30), held.recv())
            .await
            .expect("the first device sends again")
            .expect("held send");
        write(
            &second,
            "gym.sets",
            Some(&logged),
            set(session_id, &squat, 1, reps),
        )
        .await;
        eventually("the competing edit reaches the server", async || {
            pending(&second).await == 0
        })
        .await;
        release.send(()).expect("release the held send");
    }
    proxy.stop_holding_sends();
    drop(held);

    eventually("the delete wins once the edits stop", async || {
        field(&first, "gym.sets", &logged, "reps").await == Value::Null
            && field(&second, "gym.sets", &logged, "reps").await == Value::Null
            && pending(&first).await == 0
    })
    .await;
    let requests = proxy.requests();
    assert!(
        requests
            .split(|request| *request == "fetch")
            .all(|sends| sends.len() <= 2),
        "a push pass sent more than one retry before pulling: {requests:?}"
    );
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_gym_conflicts_keep_a_session_end_and_settle_one_set_per_order() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let url = format!("http://{address}");
    let proxy = TestProxy::start(address).await;
    let first = signed_in(&url, data, "first", true).await;
    let second = signed_in(&proxy.url, data, "second", false).await;

    let squat = write(&first, "gym.exercises", None, exercise("Squat")).await;
    let session_id = write(&first, "gym.sessions", None, session(&squat, None, "")).await;
    eventually("the second device receives the open session", async || {
        field(&second, "gym.sessions", &session_id, "notes").await == json!("")
    })
    .await;

    proxy.go_offline(&second).await;
    let end = "2026-10-07T11:00:00Z";
    write(
        &first,
        "gym.sessions",
        Some(&session_id),
        session(&squat, Some(end), ""),
    )
    .await;
    let session_uuid: Uuid = session_id.parse().expect("session id");
    let first_set = write(&first, "gym.sets", None, set(session_uuid, &squat, 1, 5)).await;
    write(
        &second,
        "gym.sessions",
        Some(&session_id),
        session(&squat, None, "edited offline"),
    )
    .await;
    let second_set = write(&second, "gym.sets", None, set(session_uuid, &squat, 1, 7)).await;
    assert_eq!(first_set, second_set);
    proxy.go_online();

    eventually(
        "the later edit wins, the session stays finished and one set holds the order",
        async || {
            let mut settled = true;
            for engine in [&first, &second] {
                settled &= field(engine, "gym.sessions", &session_id, "ended_at").await
                    == json!(end)
                    && field(engine, "gym.sessions", &session_id, "notes").await
                        == json!("edited offline")
                    && field(engine, "gym.sets", &first_set, "reps").await == json!(7)
                    && pending(engine).await == 0;
            }
            settled
        },
    )
    .await;

    write(
        &first,
        "gym.sessions",
        Some(&session_id),
        session(&squat, None, "written without an end"),
    )
    .await;
    assert_eq!(
        field(&first, "gym.sessions", &session_id, "ended_at").await,
        json!(end)
    );
    first.stop_session_work().await;
    second.stop_session_work().await;
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_returning_to_the_foreground_reconnects_and_sends_pending_changes_at_once() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let url = format!("http://{address}");
    let proxy = TestProxy::start(address).await;
    let first = signed_in(&url, data, "first", true).await;
    let second = signed_in(&proxy.url, data, "second", false).await;

    proxy.go_offline(&second).await;
    let weighed = write(
        &second,
        "gym.body_weight",
        None,
        json!({"time": "2026-10-07T07:00:00Z", "kg": 80.5}),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert_eq!(pending(&second).await, 1);
    assert!(second.get_state().await.offline);

    proxy.go_online();
    second
        .reconnect_now()
        .await
        .expect("the session is confirmed");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = second.get_state().await;
            if pending(&second).await == 0
                && !state.offline
                && matches!(state.connection_status, ConnectionStatus::Connected)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the device reconnects and sends its change within seconds");
    eventually("the other device receives the change", async || {
        field(&first, "gym.body_weight", &weighed, "kg").await == json!(80.5)
    })
    .await;
    first.stop_session_work().await;
    second.stop_session_work().await;
}
