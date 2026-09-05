//! Live protocol regression coverage. Run after building the server:
//! `cargo build -p clipper-server && cargo test -p clipper-client live_schedule -- --ignored`
//! Each run owns its server, database, users and device caches.

use std::{
    path::Path,
    process::{Child, Command, Stdio},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
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
        .start_actual(Some(&berlin[0].plan_context))
        .await
        .expect("start floating timer");
    wait_for(&second, |state| {
        state
            .running_actual
            .as_ref()
            .is_some_and(|actual| actual.id == timer)
    })
    .await;
    assert!(first.start_actual(Some("invalid")).await.is_err());
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

    exercise_revision_aware_plans(&first).await;

    let stale_head = second.local_head(&object_id).await.expect("old head");
    let mut edited = item.clone();
    edited.title = "Edited workout".into();
    assert_eq!(
        first
            .update_schedule_item(&object_id, edited.clone(), 1)
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
        .update_schedule_item(&large_id, large.clone(), 1)
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
        first.update_schedule_item(&large_id, oversized, 2).await,
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
    let first_source = first
        .local_store
        .schedule_records_with_ids()
        .await
        .into_iter()
        .find_map(|(id, record)| {
            (id == source)
                .then(|| record.as_source().cloned())
                .flatten()
        })
        .expect("stored source");
    let first_import = first_source.active_import.clone().expect("active import");
    assert_eq!(
        first
            .download_file_bytes(&first_import.object_id.to_string())
            .await
            .expect("raw calendar download"),
        feed.read().await.as_bytes()
    );
    let mut interrupted_source = first_source.clone();
    interrupted_source.pending_import = interrupted_source.active_import.take();
    let interrupted_head = first.local_head(&source).await.expect("source head");
    first
        .write_schedule_record(
            &source,
            ScheduleRecord::Source(Box::new(interrupted_source)),
            EnvelopePlacement::Revise(interrupted_head),
        )
        .await
        .expect("simulate interrupted activation");
    let original_feed = feed.read().await.clone();
    *feed.write().await = "not the pending calendar".into();
    first
        .sync_calendar_source(&source)
        .await
        .expect("resume stored pending import");
    *feed.write().await = original_feed;
    let resumed_source = first
        .local_store
        .schedule_records_with_ids()
        .await
        .into_iter()
        .find_map(|(id, record)| {
            (id == source)
                .then(|| record.as_source().cloned())
                .flatten()
        })
        .expect("resumed source");
    assert!(resumed_source.pending_import.is_none());
    assert_eq!(
        resumed_source
            .active_import
            .as_ref()
            .map(|batch| batch.object_id),
        Some(first_import.object_id),
        "resume promotes the exact stored snapshot without fetching a new raw file"
    );
    assert_eq!(
        first
            .sync_calendar_source(&source)
            .await
            .expect("replacement sync")
            .added,
        1
    );
    let replacement_source = first
        .local_store
        .schedule_records_with_ids()
        .await
        .into_iter()
        .find_map(|(id, record)| {
            (id == source)
                .then(|| record.as_source().cloned())
                .flatten()
        })
        .expect("replacement source");
    let replacement_import = replacement_source
        .active_import
        .expect("replacement import");
    assert_ne!(replacement_import.object_id, first_import.object_id);
    assert_eq!(replacement_import.events.len(), 1);
    let imported = first
        .local_store
        .schedule_records_with_ids()
        .await
        .into_iter()
        .find(|(_, record)| record.as_ingested().is_some())
        .expect("imported object");
    let imported_event = imported.1.as_ingested().expect("ingested event");
    assert_eq!(imported_event.import, Some(replacement_import.object_id));
    let event_domain_id = imported_event.id;
    let basic_feed = feed.read().await.clone();
    *feed.write().await = basic_feed.replace(
        "DTEND:20260908T100000Z",
        "DTEND:20260908T100000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEXDATE:20260909T090000Z\r\nRDATE:20260909T120000Z",
    );
    first
        .sync_calendar_source(&source)
        .await
        .expect("provider overrides");
    let recurring = first
        .local_store
        .schedule_records_with_ids()
        .await
        .into_iter()
        .find_map(|(_, record)| record.as_ingested().cloned())
        .expect("recurring import");
    assert_eq!(recurring.id, event_domain_id);
    assert!(matches!(recurring.recurrence, Recurrence::Every(_)));
    let imported_occurrences = first
        .expand_schedule(from, to, "UTC")
        .await
        .expect("overrides expand");
    let starts: Vec<_> = imported_occurrences
        .iter()
        .filter(|event| event.source.as_deref() == Some("Work"))
        .map(|event| event.start.as_str())
        .collect();
    assert_eq!(starts, ["2026-09-08T09:00:00Z", "2026-09-09T12:00:00Z"]);
    let provider_added = imported_occurrences
        .iter()
        .find(|event| {
            event.start == "2026-09-09T12:00:00Z" && event.source.as_deref() == Some("Work")
        })
        .unwrap();
    let provider_actual = first
        .start_actual(Some(&provider_added.plan_context))
        .await
        .unwrap();
    first.stop_actual(&provider_actual).await.unwrap();
    *feed.write().await = basic_feed;
    let updated_feed = feed
        .read()
        .await
        .replace("SUMMARY:Planning", "SUMMARY:Updated planning");
    *feed.write().await = updated_feed;
    let updated = first
        .sync_calendar_source(&source)
        .await
        .expect("updated feed");
    assert_eq!(updated.added, 1);
    assert_eq!(updated.tombstoned, 1);
    first.schedule_history.lock().await.clear();
    assert!(
        first.recorded_plan(&provider_actual).await.is_err(),
        "the imported plan revision was permanently purged"
    );
    assert!(
        first
            .local_store
            .schedule_records_with_ids()
            .await
            .into_iter()
            .any(
                |(id, record)| id == provider_actual && matches!(record, ScheduleRecord::Actual(_))
            ),
        "recordings survive replacement of their imported plan"
    );
    let (mut poisoned_source, poisoned_head) = first
        .local_store
        .schedule_records_with_heads()
        .await
        .expect("schedule records")
        .into_iter()
        .find_map(|(id, record, head)| {
            (id == source).then(|| record.as_source().cloned().map(|source| (source, head)))?
        })
        .expect("source to test cleanup ownership");
    poisoned_source
        .retired_imports
        .push(clipper_schedule::ingest::CalendarImport {
            object_id: uuid::Uuid::new_v4().into(),
            fetched_at: Utc::now(),
            events: vec![provider_actual.parse().expect("actual object id")],
        });
    first
        .write_schedule_record(
            &source,
            ScheduleRecord::Source(Box::new(poisoned_source.clone())),
            EnvelopePlacement::Revise(poisoned_head),
        )
        .await
        .expect("store malformed retired manifest");
    assert!(
        first.sync_calendar_source(&source).await.is_err(),
        "cleanup must reject an Actual named by a malformed import manifest"
    );
    assert!(
        first
            .local_store
            .schedule_records_with_ids()
            .await
            .into_iter()
            .any(
                |(id, record)| id == provider_actual && matches!(record, ScheduleRecord::Actual(_))
            ),
        "rejected cleanup leaves the Actual untouched"
    );
    poisoned_source.retired_imports.clear();
    let poisoned_head = first
        .local_head(&source)
        .await
        .expect("poisoned source head");
    first
        .write_schedule_record(
            &source,
            ScheduleRecord::Source(Box::new(poisoned_source)),
            EnvelopePlacement::Revise(poisoned_head),
        )
        .await
        .expect("restore valid source manifest");

    let valid_feed = feed.read().await.clone();
    let active_before_failure = first
        .local_store
        .schedule_records_with_ids()
        .await
        .into_iter()
        .find_map(|(id, record)| {
            (id == source)
                .then(|| record.as_source()?.active_import.clone())
                .flatten()
        })
        .expect("active import before failed refresh");
    *feed.write().await = valid_feed.replace("DTSTART:20260908T090000Z", "DTSTART:invalid");
    assert!(first.sync_calendar_source(&source).await.is_err());
    let active_after_failure = first
        .local_store
        .schedule_records_with_ids()
        .await
        .into_iter()
        .find_map(|(id, record)| {
            (id == source)
                .then(|| record.as_source()?.active_import.clone())
                .flatten()
        })
        .expect("active import after failed refresh");
    assert_eq!(
        active_after_failure.object_id,
        active_before_failure.object_id
    );
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
    *feed.write().await = valid_feed;
    first
        .delete_file(&active_before_failure.object_id.to_string())
        .await
        .expect("delete raw import only");
    assert!(
        first
            .expand_schedule(from, to, "UTC")
            .await
            .expect("calendar survives raw deletion")
            .iter()
            .any(|event| event.title == "Updated planning")
    );
    let after_raw_delete = first
        .sync_calendar_source(&source)
        .await
        .expect("replacement after raw-only deletion");
    assert_eq!(after_raw_delete.added, 1);
    assert_eq!(after_raw_delete.tombstoned, 1);
    let active_after_raw_delete = first
        .local_store
        .schedule_records_with_ids()
        .await
        .into_iter()
        .find_map(|(id, record)| {
            (id == source)
                .then(|| record.as_source()?.active_import.clone())
                .flatten()
        })
        .expect("replacement import after raw deletion");
    assert_ne!(
        active_after_raw_delete.object_id,
        active_before_failure.object_id
    );
    assert!(
        first
            .expand_schedule(from, to, "UTC")
            .await
            .expect("replacement calendar after raw deletion")
            .iter()
            .any(|event| event.title == "Updated planning")
    );
    let personal_source = first
        .add_calendar_source("Personal", &feed_url)
        .await
        .expect("independent source");
    first
        .sync_calendar_source(&personal_source)
        .await
        .expect("independent source sync");
    assert!(
        first
            .expand_schedule(from, to, "UTC")
            .await
            .expect("both sources visible")
            .iter()
            .any(|event| event.source.as_deref() == Some("Personal"))
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
            .any(|event| event.source.as_deref() == Some("Work"))
    );
    assert!(
        first
            .expand_schedule(from, to, "UTC")
            .await
            .expect("independent source retained")
            .iter()
            .any(|event| event.source.as_deref() == Some("Personal"))
    );
    assert!(
        first
            .local_store
            .schedule_records_with_ids()
            .await
            .into_iter()
            .filter_map(|(_, record)| record.as_ingested().cloned())
            .all(|event| event.source != first_source.id),
        "removed source event objects are purged"
    );
    assert!(
        first
            .local_store
            .schedule_records_with_ids()
            .await
            .into_iter()
            .any(
                |(id, record)| id == provider_actual && matches!(record, ScheduleRecord::Actual(_))
            ),
        "recordings survive source removal"
    );
    first
        .delete_schedule_object(&personal_source)
        .await
        .expect("remove independent source");
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

/// One connected workflow checks the relationships rather than mirroring each
/// field assignment: edits, stale selections, overridden plans and deletion.
#[test]
fn imported_source_readiness_requires_a_complete_active_batch() {
    let source_id = SourceId::new();
    let raw_id: ObjectId = uuid::Uuid::new_v4().into();
    let event_id: ObjectId = uuid::Uuid::new_v4().into();
    let batch = clipper_schedule::ingest::CalendarImport {
        object_id: raw_id,
        fetched_at: Utc::now(),
        events: vec![event_id],
    };
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:ready\r\nSUMMARY:Ready\r\nDTSTART:20260908T090000Z\r\nDTEND:20260908T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let mut event = clipper_schedule::parse_ics(feed, source_id)
        .expect("valid feed")
        .events
        .pop()
        .expect("event");
    event.import = Some(raw_id);
    let head = LocalHead {
        revision: 1,
        parent_hash: [0; crypto::SHA256_BYTES],
    };
    let source_record = |active_import, pending_import| {
        ScheduleRecord::Source(Box::new(CalendarSource {
            id: source_id,
            name: "Ready".into(),
            kind: SourceKind::Ics {
                url: "https://example.test/feed.ics".into(),
            },
            enabled: true,
            active_import,
            pending_import,
            retired_imports: Vec::new(),
        }))
    };
    let complete = vec![
        (
            "source".into(),
            source_record(Some(batch.clone()), None),
            head,
        ),
        (
            event_id.to_string(),
            ScheduleRecord::Ingested(Box::new(event.clone())),
            head,
        ),
    ];
    assert!(calendar_import::ready_sources(&complete).contains(&source_id));

    let incomplete = vec![(
        "source".into(),
        source_record(Some(batch.clone()), None),
        head,
    )];
    assert!(!calendar_import::ready_sources(&incomplete).contains(&source_id));

    let staged = vec![
        ("source".into(), source_record(None, Some(batch)), head),
        (
            event_id.to_string(),
            ScheduleRecord::Ingested(Box::new(event)),
            head,
        ),
    ];
    assert!(!calendar_import::ready_sources(&staged).contains(&source_id));
}

async fn exercise_revision_aware_plans(engine: &SyncEngine) {
    use clipper_schedule::{
        ActualSpan, ObjectRevisionRef, OccurrenceOverride, OccurrenceOverrideData, OverrideChange,
        OverrideId, PlannedRef, RecurrenceId,
    };
    let mut item = ScheduleItem {
        id: ScheduleItemId::new(),
        title: "Original historical title".into(),
        span: ScheduleSpan::Timed {
            start: TimedStart::Floating("2027-01-05T07:00:00".parse().unwrap()),
            duration: BlockDuration::from_minutes(30).unwrap(),
        },
        recurrence: Recurrence::Once,
        reference: None,
        alarm: None,
    };
    let id = engine.create_schedule_item(item.clone()).await.unwrap();
    let from = "2027-01-04T00:00:00Z";
    let to = "2027-01-08T00:00:00Z";
    let calendar = engine
        .expand_schedule(from, to, "Europe/Berlin")
        .await
        .unwrap();
    let original = calendar
        .iter()
        .find(|entry| entry.item_id == item.id.to_string())
        .unwrap();
    let original_context: PlannedRef = serde_json::from_str(&original.plan_context).unwrap();
    let actual_id = engine
        .start_actual(Some(&original.plan_context))
        .await
        .unwrap();
    item.title = "Renamed while recording".into();
    engine
        .update_schedule_item(&id, item.clone(), 1)
        .await
        .unwrap();
    assert_eq!(
        engine.get_state().await.running_actual.unwrap().title,
        "Original historical title"
    );
    engine.stop_actual(&actual_id).await.unwrap();

    // Force a historical HTTP read rather than accepting a cache-only success.
    engine.schedule_history.lock().await.clear();
    let historical = engine.recorded_plan(&actual_id).await.unwrap().unwrap();
    assert_eq!(historical.item.title, "Original historical title");
    assert_eq!(historical.context, original_context);
    assert_eq!(historical.context.observer, chrono_tz::Europe::Berlin);
    assert_eq!(
        historical.context.span.start.to_rfc3339(),
        "2027-01-05T06:00:00+00:00"
    );
    assert_eq!(engine.local_head(&id).await.unwrap().revision, 2);

    let unplanned = engine.start_actual(None).await.unwrap();
    assert!(
        engine
            .start_actual(Some(&original.plan_context))
            .await
            .is_err()
    );
    assert_eq!(
        engine.get_state().await.running_actual.unwrap().id,
        unplanned
    );
    assert!(
        engine
            .update_schedule_item(&id, item.clone(), 1)
            .await
            .is_err()
    );
    let current = engine
        .expand_schedule(from, to, "Europe/Berlin")
        .await
        .unwrap();
    let current = current
        .iter()
        .find(|entry| entry.item_id == item.id.to_string())
        .unwrap();
    let mut forged: PlannedRef = serde_json::from_str(&current.plan_context).unwrap();
    forged.span.start += chrono::TimeDelta::minutes(1);
    assert!(
        engine
            .start_actual(Some(&serde_json::to_string(&forged).unwrap()))
            .await
            .is_err()
    );
    assert_eq!(
        engine.get_state().await.running_actual.unwrap().id,
        unplanned
    );

    let base = revision_ref(&id, engine.local_head(&id).await.unwrap()).unwrap();
    let mut override_data = OccurrenceOverride {
        base,
        override_data: OccurrenceOverrideData {
            id: OverrideId::new(),
            item: item.id,
            recurrence_id: original_context.recurrence_id,
            change: OverrideChange::Rescheduled(ScheduleSpan::Timed {
                start: TimedStart::Floating("2027-01-06T09:00:00".parse().unwrap()),
                duration: BlockDuration::from_minutes(45).unwrap(),
            }),
        },
    };
    let override_id = engine
        .create_schedule_record(ScheduleRecord::Override(Box::new(override_data.clone())))
        .await
        .unwrap();
    let mut structural = item.clone();
    structural.recurrence = Recurrence::Every(clipper_schedule::Cadence::each(
        clipper_schedule::Frequency::Daily,
    ));
    assert!(
        engine
            .update_schedule_item(&id, structural, 2)
            .await
            .is_err()
    );
    item.title = "Cosmetic edit keeps override".into();
    engine
        .update_schedule_item(&id, item.clone(), 2)
        .await
        .unwrap();

    let moved = engine
        .expand_schedule(from, to, "Europe/Berlin")
        .await
        .unwrap();
    let moved = moved
        .iter()
        .find(|entry| entry.item_id == item.id.to_string())
        .unwrap();
    assert_eq!(moved.start, "2027-01-06T08:00:00Z");
    let moved_context: PlannedRef = serde_json::from_str(&moved.plan_context).unwrap();
    assert_eq!(moved_context.schedule.revision, 3);
    assert_eq!(moved_context.override_revision.unwrap().revision, 1);
    assert_eq!(moved_context.recurrence_id, original_context.recurrence_id);
    let moved_actual = engine
        .start_actual(Some(&moved.plan_context))
        .await
        .unwrap();
    let override_head = engine.local_head(&override_id).await.unwrap();
    override_data.override_data.change = OverrideChange::Cancelled;
    engine
        .write_schedule_record(
            &override_id,
            ScheduleRecord::Override(Box::new(override_data)),
            EnvelopePlacement::Revise(override_head),
        )
        .await
        .unwrap();
    assert!(
        engine
            .start_actual(Some(&moved.plan_context))
            .await
            .is_err()
    );
    assert_eq!(
        engine.get_state().await.running_actual.unwrap().id,
        moved_actual
    );
    engine.stop_actual(&moved_actual).await.unwrap();
    // A structural edit received from an older/other writer is surfaced, never
    // interpreted as an override to a different rule or allowed to hide peers.
    let mut incompatible = item.clone();
    incompatible.recurrence = Recurrence::Every(clipper_schedule::Cadence::each(
        clipper_schedule::Frequency::Daily,
    ));
    engine
        .write_schedule_record(
            &id,
            ScheduleRecord::Item(Box::new(incompatible)),
            EnvelopePlacement::Revise(engine.local_head(&id).await.unwrap()),
        )
        .await
        .unwrap();
    let mut unaffected = item.clone();
    unaffected.id = ScheduleItemId::new();
    engine
        .create_schedule_item(unaffected.clone())
        .await
        .unwrap();
    let visible = engine
        .expand_schedule(from, to, "Europe/Berlin")
        .await
        .unwrap();
    assert!(
        !visible
            .iter()
            .any(|entry| entry.item_id == item.id.to_string())
    );
    assert!(
        visible
            .iter()
            .any(|entry| entry.item_id == unaffected.id.to_string())
    );
    assert!(!engine.get_state().await.schedule_warnings.is_empty());
    engine.delete_schedule_object(&id).await.unwrap();
    engine.delete_schedule_object(&override_id).await.unwrap();
    engine.schedule_history.lock().await.clear();
    let historical = engine.recorded_plan(&moved_actual).await.unwrap().unwrap();
    assert!(matches!(
        historical.override_data.unwrap().change,
        OverrideChange::Rescheduled(_)
    ));
    assert_eq!(historical.context, moved_context);
    assert_eq!(engine.local_head(&id).await.unwrap().revision, 5);
    assert_eq!(engine.local_head(&override_id).await.unwrap().revision, 3);

    // A wrong accepted hash must not return some other authentic revision.
    let mut wrong_pin: ObjectRevisionRef = historical.context.schedule;
    wrong_pin.body_hash[0] ^= 1;
    assert!(engine.schedule_revision(wrong_pin).await.is_err());

    // The timing and pins survive the stop revision.
    let stopped = engine.local_store.schedule_records_with_ids().await;
    let actual = stopped
        .iter()
        .find_map(|(id, record)| match record {
            ScheduleRecord::Actual(actual) if id == &moved_actual => Some(actual),
            _ => None,
        })
        .unwrap();
    assert!(matches!(actual.span, ActualSpan::Complete(_)));
    assert_eq!(actual.planned, Some(moved_context));
    assert!(matches!(
        actual.planned.unwrap().recurrence_id,
        RecurrenceId::Floating(_)
    ));

    engine.api.delete_object(&id).await.unwrap();
    engine.schedule_history.lock().await.clear();
    assert!(engine.recorded_plan(&moved_actual).await.is_err());
}
