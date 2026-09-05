//! Live protocol regression coverage. Run after building the server:
//! `cargo build -p clipper-server && cargo test -p clipper-client live_schedule -- --ignored`
//! Each run owns its server, database, users and device caches.

use std::{
    path::Path,
    process::{Child, Command, Stdio},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clipper_schedule::{AlarmPolicy, BlockDuration, Recurrence, ScheduleItemId, TimedStart};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

struct TestServer(Child);

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_for(engine: &SyncEngine, predicate: impl Fn(&AppState) -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if predicate(&engine.get_state().await) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("state converges within ten seconds");
}

fn server_command(binary: &Path, data: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .env("CLIPPER_SERVER_SECRET", STANDARD.encode([42_u8; 32]))
        .env_remove("CLIPPER_SERVER_SECRET_FILE")
        .env("RUST_LOG", "warn")
        .arg("--config")
        .arg(data.join("config.toml"))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    command
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_schedule_revisions_timers_feeds_and_two_devices() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().expect("tempdir");
    let binary = std::env::var_os("CLIPPER_TEST_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/clipper-server")
        });
    assert!(binary.exists(), "cargo build -p clipper-server first");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
    let address = listener.local_addr().expect("address");
    drop(listener);
    let data = temp.path();
    std::fs::write(data.join("config.toml"), format!(
        "[server]\ndata_dir = {:?}\naddr = {:?}\n[rate_limit]\nauth_per_client_per_minute = 200\nauth_per_username_per_minute = 200\n",
        data.join("server").to_str().expect("path"), address.to_string()
    )).expect("config");
    assert!(
        server_command(&binary, data)
            .arg("init")
            .status()
            .expect("init")
            .success()
    );
    assert!(
        server_command(&binary, data)
            .args(["add-access-key", "--access-key", "test-invite"])
            .status()
            .expect("invite")
            .success()
    );
    let _server = TestServer(
        server_command(&binary, data)
            .arg("serve")
            .spawn()
            .expect("server"),
    );
    let url = format!("http://{address}");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if reqwest::get(format!("{url}/api/health")).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("server starts");

    let first = SyncEngine::new_with_data_dir(&url, data.join("first"));
    first
        .register_with_platform(
            "test-invite",
            "scheduler-test",
            "local-test-passphrase",
            "First",
            "test",
        )
        .await
        .expect("register");
    let second = SyncEngine::new_with_data_dir(&url, data.join("second"));
    second
        .login_with_platform("local-test-passphrase", "scheduler-test", "Second", "test")
        .await
        .expect("login");
    wait_for(&first, |state| {
        matches!(state.connection_status, ConnectionStatus::Connected)
    })
    .await;
    wait_for(&second, |state| {
        matches!(state.connection_status, ConnectionStatus::Connected)
    })
    .await;

    let item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "Floating workout".into(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Floating("2026-09-08T07:00:00".parse().expect("time")),
            duration: BlockDuration::from_minutes(45).expect("duration"),
        },
        recurrence: Recurrence::Once,
        reference: None,
        alarm: None,
    };
    let object_id = first
        .create_schedule_item(item.clone())
        .await
        .expect("create");
    wait_for(&second, |state| {
        state
            .schedule_items
            .iter()
            .any(|entry| entry.id == object_id)
    })
    .await;
    let from = "2026-09-07T00:00:00Z";
    let to = "2026-09-10T00:00:00Z";
    let berlin = first
        .expand_schedule(from, to, "Europe/Berlin")
        .await
        .expect("Berlin");
    let tokyo = second
        .expand_schedule(from, to, "Asia/Tokyo")
        .await
        .expect("Tokyo");
    assert_eq!(berlin[0].start, "2026-09-08T05:00:00Z");
    assert_eq!(tokyo[0].start, "2026-09-07T22:00:00Z");
    assert_eq!(berlin[0].occurrence_key, tokyo[0].occurrence_key);

    let timer = first
        .start_actual(Some((&berlin[0].item_id, &berlin[0].occurrence_key)))
        .await
        .expect("start floating timer");
    wait_for(&second, |state| {
        state
            .running_actual
            .as_ref()
            .is_some_and(|actual| actual.id == timer)
    })
    .await;
    assert!(
        first
            .start_actual(Some(("invalid", "invalid")))
            .await
            .is_err()
    );
    assert_eq!(
        first
            .get_state()
            .await
            .running_actual
            .expect("invalid start keeps timer running")
            .id,
        timer
    );
    assert_eq!(first.stop_actual(&timer).await.expect("stop"), timer);
    assert_eq!(
        first.local_head(&timer).await.expect("timer head").revision,
        2
    );
    wait_for(&second, |state| state.running_actual.is_none()).await;
    assert_eq!(
        first
            .local_store
            .schedule_records_with_ids()
            .await
            .iter()
            .filter(|(_, record)| matches!(record, ScheduleRecord::Actual(_)))
            .count(),
        1
    );

    let stale_head = second.local_head(&object_id).await.expect("old head");
    let mut edited = item.clone();
    edited.title = "Edited workout".into();
    assert_eq!(
        first
            .update_schedule_item(&object_id, edited.clone())
            .await
            .expect("edit"),
        object_id
    );
    wait_for(&second, |state| {
        state
            .schedule_items
            .iter()
            .any(|entry| entry.title == "Edited workout")
    })
    .await;
    assert_eq!(
        second
            .local_head(&object_id)
            .await
            .expect("remote head")
            .revision,
        2
    );
    assert!(matches!(
        second
            .write_schedule_record(
                &object_id,
                ScheduleRecord::Item(Box::new(item)),
                EnvelopePlacement::Revise(stale_head)
            )
            .await,
        Err(ClientError::Api { status: 409, .. })
    ));

    // A lead time can put an alarm inside the requested horizon even when the
    // event itself starts beyond that horizon.
    let mut future = edited;
    future.id = ScheduleItemId::new();
    future.span = ScheduleSpan::Timed {
        start: TimedStart::Zoned {
            local: (chrono::Utc::now() + chrono::TimeDelta::hours(25)).naive_utc(),
            zone: chrono_tz::UTC,
        },
        duration: BlockDuration::from_minutes(30).expect("duration"),
    };
    future.alarm = Some(AlarmPolicy::minutes_before(120));
    first
        .create_schedule_item(future.clone())
        .await
        .expect("future alarm");
    assert!(
        first
            .next_alarms(24, "UTC")
            .await
            .expect("alarms")
            .iter()
            .any(|alarm| alarm.item_id == future.id.to_string())
    );

    // Exercise the streamed revision path as well as the inline path: payload
    // completion must count only this revision, not all historical payloads.
    let mut large = future.clone();
    large.id = ScheduleItemId::new();
    large.alarm = None;
    large.title = "x".repeat(70_000);
    let large_id = first
        .create_schedule_item(large.clone())
        .await
        .expect("streamed create");
    large.title = "y".repeat(70_000);
    first
        .update_schedule_item(&large_id, large.clone())
        .await
        .expect("streamed revision");
    wait_for(&second, |state| {
        state
            .schedule_items
            .iter()
            .any(|entry| entry.id == large_id && entry.title.starts_with('y'))
    })
    .await;
    assert_eq!(
        second
            .local_head(&large_id)
            .await
            .expect("streamed remote head")
            .revision,
        2
    );
    let mut oversized = large;
    oversized.title = "z".repeat(256 * 1024);
    assert!(matches!(
        first.update_schedule_item(&large_id, oversized).await,
        Err(ClientError::InvalidArgument(message)) if message.contains("256 KiB")
    ));
    assert_eq!(
        first
            .local_head(&large_id)
            .await
            .expect("unchanged large head")
            .revision,
        2
    );
    first
        .delete_schedule_object(&large_id)
        .await
        .expect("delete large fixture");

    let feed = Arc::new(RwLock::new(String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:meeting-1\r\nSUMMARY:Planning\r\nDTSTART:20260908T090000Z\r\nDTEND:20260908T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("feed listener");
    let feed_url = format!(
        "http://{}/feed.ics",
        listener.local_addr().expect("feed address")
    );
    let current_feed = Arc::clone(&feed);
    let feed_task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.expect("feed request");
            let mut request = [0; 4096];
            if socket.read(&mut request).await.is_err() {
                continue;
            }
            let body = current_feed.read().await.clone();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/calendar\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    let source = first
        .add_calendar_source("Work", &feed_url)
        .await
        .expect("source");
    assert_eq!(
        first
            .sync_calendar_source(&source)
            .await
            .expect("first sync")
            .added,
        1
    );
    assert_eq!(
        first
            .sync_calendar_source(&source)
            .await
            .expect("idempotent sync")
            .unchanged,
        1
    );
    let imported = first
        .local_store
        .schedule_records_with_ids()
        .await
        .into_iter()
        .find(|(_, record)| record.as_ingested().is_some())
        .expect("imported object");
    let basic_feed = feed.read().await.clone();
    *feed.write().await = basic_feed.replace(
        "DTEND:20260908T100000Z",
        "DTEND:20260908T100000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEXDATE:20260909T090000Z\r\nRDATE:20260909T120000Z",
    );
    first
        .sync_calendar_source(&source)
        .await
        .expect("provider exceptions");
    let imported_occurrences = first
        .expand_schedule(from, to, "UTC")
        .await
        .expect("exceptions expand");
    let starts: Vec<_> = imported_occurrences
        .iter()
        .filter(|event| event.source.as_deref() == Some("Work"))
        .map(|event| event.start.as_str())
        .collect();
    assert_eq!(starts, ["2026-09-08T09:00:00Z", "2026-09-09T12:00:00Z"]);
    *feed.write().await = basic_feed;
    let updated_feed = feed
        .read()
        .await
        .replace("SUMMARY:Planning", "SUMMARY:Updated planning");
    *feed.write().await = updated_feed;
    assert_eq!(
        first
            .sync_calendar_source(&source)
            .await
            .expect("updated feed")
            .updated,
        1
    );
    assert_eq!(
        first
            .local_head(&imported.0)
            .await
            .expect("import head")
            .revision,
        3
    );
    let valid_feed = feed.read().await.clone();
    *feed.write().await = valid_feed.replace("DTSTART:20260908T090000Z", "DTSTART:invalid");
    let partial = first
        .sync_calendar_source(&source)
        .await
        .expect("partial parse reported");
    assert!(!partial.skipped.is_empty());
    assert_eq!(partial.tombstoned, 0);
    assert!(
        first
            .expand_schedule(from, to, "UTC")
            .await
            .expect("retained meeting")
            .iter()
            .any(|event| event.title == "Updated planning" && !event.cancelled)
    );
    *feed.write().await = "not a calendar".into();
    assert!(first.sync_calendar_source(&source).await.is_err());
    *feed.write().await = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n".into();
    assert_eq!(
        first
            .sync_calendar_source(&source)
            .await
            .expect("removed upstream")
            .tombstoned,
        1
    );
    assert_eq!(
        first
            .local_head(&imported.0)
            .await
            .expect("cancel head")
            .revision,
        4
    );
    first
        .delete_schedule_object(&source)
        .await
        .expect("remove source");
    assert!(
        !first
            .expand_schedule(from, to, "UTC")
            .await
            .expect("hidden source")
            .iter()
            .any(|event| event.title == "Updated planning")
    );
    feed_task.abort();

    let file = first
        .upload_file_bytes("qa.txt", Some("text/plain"), b"QA file")
        .await
        .expect("file");
    wait_for(&second, |state| {
        state.files.iter().any(|entry| entry.id == file)
    })
    .await;
    first.delete_file(&file).await.expect("file tombstone");
    wait_for(&second, |state| {
        state.files.iter().all(|entry| entry.id != file)
    })
    .await;
    let restore_record = first
        .local_store
        .schedule_records_with_ids()
        .await
        .into_iter()
        .find(|(id, _)| id == &object_id)
        .expect("record to restore")
        .1;
    first
        .delete_schedule_object(&object_id)
        .await
        .expect("schedule tombstone");
    wait_for(&second, |state| {
        state
            .schedule_items
            .iter()
            .all(|entry| entry.id != object_id)
    })
    .await;
    let deleted_head = first
        .local_head(&object_id)
        .await
        .expect("tombstone anchor survives");
    assert_eq!(deleted_head.revision, 3);
    first
        .write_schedule_record(
            &object_id,
            restore_record,
            EnvelopePlacement::Revise(deleted_head),
        )
        .await
        .expect("restore signed tombstone");
    wait_for(&second, |state| {
        state
            .schedule_items
            .iter()
            .any(|entry| entry.id == object_id)
    })
    .await;
    assert_eq!(
        second
            .local_head(&object_id)
            .await
            .expect("restored head")
            .revision,
        4
    );
    first
        .delete_schedule_object(&object_id)
        .await
        .expect("delete restored fixture");
    let third = SyncEngine::new_with_data_dir(&url, data.join("third"));
    third
        .login_with_platform("local-test-passphrase", "scheduler-test", "Third", "test")
        .await
        .expect("cold device login");
    wait_for(&third, |state| {
        state
            .schedule_items
            .iter()
            .any(|entry| entry.id != object_id)
    })
    .await;
    assert!(
        third
            .get_state()
            .await
            .schedule_items
            .iter()
            .all(|entry| entry.id != object_id && entry.id != large_id)
    );

    let resume = first
        .session_resume_material()
        .await
        .expect("derived resume material");
    let resumed = SyncEngine::new_with_data_dir(&url, data.join("first"));
    resumed
        .resume_with_platform(
            resume.token.clone(),
            resume.data_key.clone(),
            resume.device_identity_wrapping_key.clone(),
            "scheduler-test",
            "First",
        )
        .await
        .expect("resume without passphrase");
    assert_eq!(
        resumed
            .get_state()
            .await
            .session
            .expect("resumed session")
            .device_id,
        first
            .get_state()
            .await
            .session
            .expect("original session")
            .device_id
    );
    first.logout().await.expect("logout first");
    assert!(
        resumed
            .resume_with_platform(
                resume.token,
                resume.data_key,
                resume.device_identity_wrapping_key,
                "scheduler-test",
                "First"
            )
            .await
            .is_err(),
        "revoked token cannot resume"
    );
    second.logout().await.expect("logout second");
    third.logout().await.expect("logout third");
}
