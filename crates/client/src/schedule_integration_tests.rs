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

pub(super) struct TestServer(Child);

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) async fn wait_for(engine: &SyncEngine, predicate: impl Fn(&AppState) -> bool) {
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
    let data = temp.path();
    let (_server, address) = start_server(data).await;
    let url = format!("http://{address}");

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

    check_schedule(first, second, &url, data).await;
}

pub(super) async fn start_server(data: &Path) -> (TestServer, std::net::SocketAddr) {
    let binary = std::env::var_os("CLIPPER_TEST_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/clipper-server")
        });
    assert!(binary.exists(), "cargo build -p clipper-server first");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
    let address = listener.local_addr().expect("address");
    drop(listener);
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
    let server = TestServer(
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

    (server, address)
}

async fn calendar_feed_server(
    initial: String,
) -> (
    String,
    Arc<RwLock<String>>,
    Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let feed = Arc::new(RwLock::new(initial));
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let feed_url = format!("http://{}/feed.ics", listener.local_addr().unwrap());
    let served = Arc::clone(&feed);
    let counted = Arc::clone(&requests);
    let feed_task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            if socket.read(&mut buffer).await.unwrap() == 0 {
                continue;
            }
            counted.fetch_add(1, Ordering::SeqCst);
            let body = served.read().await.clone();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (feed_url, feed, requests, feed_task)
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_unchanged_feeds_and_newer_fetches() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let url = format!("http://{address}");
    let first = register_proxy_engine(&url, &temp.path().join("first")).await;
    let second = SyncEngine::new_with_data_dir(&url, temp.path().join("second"));
    second
        .login_with_platform("local-test-passphrase", "recovery-test", "Second", "test")
        .await
        .unwrap();
    wait_for(&second, |state| {
        matches!(state.connection_status, ConnectionStatus::Connected)
    })
    .await;
    let start =
        chrono::DateTime::from_timestamp(Utc::now().timestamp() / 60 * 60 + 7200, 0).unwrap();
    let original = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:meeting\r\nSUMMARY:Meeting\r\nDTSTART:{}\r\nDTSTAMP:20260908T000000Z\r\nRRULE:FREQ=DAILY;COUNT=3;BYHOUR=9,17\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        start.format("%Y%m%dT%H%M%SZ")
    );
    let (feed_url, feed, requests, feed_task) = calendar_feed_server(original.clone()).await;
    let source_id = first.add_calendar_source("Work", &feed_url).await.unwrap();
    first.sync_calendar_source(&source_id).await.unwrap();
    wait_for(&second, |state| {
        state
            .calendar_sources
            .iter()
            .any(|source| source.id == source_id && source.event_count == 1)
    })
    .await;
    let before = serde_json::to_value(
        first
            .api
            .list_objects(None, Some(100), None, None)
            .await
            .unwrap(),
    )
    .unwrap();
    *feed.write().await = original.replace(
        "DTSTAMP:20260908T000000Z",
        "DTSTAMP:20261007T010203Z\r\nLAST-MODIFIED:20261007T010203Z",
    );
    let report = first.sync_calendar_source(&source_id).await.unwrap();
    assert!(report.feed_unchanged);
    assert_eq!(report.unchanged, 1);
    assert_eq!((report.added, report.tombstoned), (0, 0));
    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "manual Sync fetches a new response"
    );
    let after = serde_json::to_value(
        first
            .api
            .list_objects(None, Some(100), None, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        after, before,
        "unchanged refresh creates no objects or revisions"
    );
    let state = first.get_state().await;
    let source = state
        .calendar_sources
        .iter()
        .find(|source| source.id == source_id)
        .unwrap();
    assert!(
        chrono::DateTime::parse_from_rfc3339(source.checked_at.as_deref().unwrap()).unwrap()
            > chrono::DateTime::parse_from_rfc3339(source.fetched_at.as_deref().unwrap()).unwrap()
    );
    let held_key = first.current_encryption_key().await.unwrap();
    let restarted = copy_session(&first, &url, &temp.path().join("first")).await;
    let visible = restarted
        .local_store
        .hydrate_ciphertext_cache(&held_key, RECENT_CLIPBOARD_LIMIT)
        .await
        .unwrap();
    assert_eq!(
        visible
            .calendar_sources
            .iter()
            .find(|source| source.id == source_id)
            .unwrap()
            .checked_at,
        source.checked_at
    );
    *feed.write().await = original.replace("BYHOUR=9,17", "BYHOUR=9,18");
    assert!(
        !first
            .sync_calendar_source(&source_id)
            .await
            .unwrap()
            .feed_unchanged,
        "an imported rule change replaces the batch"
    );

    let alarms_feed = original.replace("RRULE:FREQ=DAILY;COUNT=3;BYHOUR=9,17\r\n", "");
    *feed.write().await = alarms_feed.clone();
    first.sync_calendar_source(&source_id).await.unwrap();
    let alarms = first.next_alarms(24, "UTC").await.unwrap();
    assert_eq!(alarms.len(), 1);
    assert_eq!(alarms[0].label, "Meeting");
    assert_eq!(
        alarms[0].fire_at_millis,
        (start - chrono::TimeDelta::minutes(5)).timestamp_millis()
    );
    first
        .set_calendar_source_alarms(&source_id, false)
        .await
        .unwrap();
    assert!(first.next_alarms(24, "UTC").await.unwrap().is_empty());
    first
        .set_calendar_source_alarms(&source_id, true)
        .await
        .unwrap();

    for host in ["calendar.google.com", "www.google.com"] {
        let google = first
            .add_calendar_source(
                "Google",
                &format!("https://{host}/calendar/ical/me%40gmail.com/private-secret/basic.ics"),
            )
            .await
            .unwrap();
        for (partstat, rings) in [("DECLINED", false), ("ACCEPTED", true)] {
            let text = alarms_feed.replace(
                "SUMMARY:Meeting\r\n",
                &format!("SUMMARY:Invite\r\nATTENDEE;PARTSTAT={partstat}:mailto:me@gmail.com\r\n"),
            );
            let batch = first
                .stage_calendar_import(&google, &text, Utc::now())
                .await
                .unwrap();
            first
                .finish_calendar_import(&google, &text, &batch)
                .await
                .unwrap();
            let alarms = first.next_alarms(24, "UTC").await.unwrap();
            assert_eq!(
                alarms
                    .iter()
                    .filter(|alarm| alarm.label == "Invite")
                    .count(),
                usize::from(rings)
            );
        }
        first.delete_schedule_object(&google).await.unwrap();
    }

    for newest_first in [true, false] {
        let older_text = alarms_feed.replace("SUMMARY:Meeting", "SUMMARY:Older");
        let newer_text = alarms_feed.replace("SUMMARY:Meeting", "SUMMARY:Newer");
        let older_time = Utc::now();
        let newer_time = older_time + chrono::TimeDelta::microseconds(1);
        let (older, newer) = tokio::join!(
            first.stage_calendar_import(&source_id, &older_text, older_time),
            second.stage_calendar_import(&source_id, &newer_text, newer_time),
        );
        let older = older.unwrap();
        let newer = newer.unwrap();
        wait_for(&first, |state| {
            state
                .files
                .iter()
                .any(|file| file.id == newer.object_id.to_string())
        })
        .await;
        wait_for(&second, |state| {
            state
                .files
                .iter()
                .any(|file| file.id == older.object_id.to_string())
        })
        .await;
        if newest_first {
            let report = second
                .finish_calendar_import(&source_id, &newer_text, &newer)
                .await
                .unwrap();
            assert!(
                report.skipped.is_empty(),
                "newer cleanup: {:?}",
                report.skipped
            );
            let report = first
                .finish_calendar_import(&source_id, &older_text, &older)
                .await
                .unwrap();
            assert!(report.superseded);
            assert!(
                report.skipped.is_empty(),
                "older cleanup: {:?}",
                report.skipped
            );
        } else {
            let (older_report, newer_report) = tokio::join!(
                first.finish_calendar_import(&source_id, &older_text, &older),
                second.finish_calendar_import(&source_id, &newer_text, &newer),
            );
            let older_report = older_report.unwrap();
            let newer_report = newer_report.unwrap();
            assert!(
                older_report.skipped.is_empty(),
                "older cleanup: {:?}",
                older_report.skipped
            );
            assert!(
                newer_report.skipped.is_empty(),
                "newer cleanup: {:?}",
                newer_report.skipped
            );
        }
        wait_for(&first, |state| {
            state.calendar_sources.iter().any(|source| {
                source.id == source_id
                    && source.fetched_at.as_deref() == Some(newer_time.to_rfc3339().as_str())
            })
        })
        .await;
        wait_for(&second, |state| {
            state.calendar_sources.iter().any(|source| {
                source.id == source_id
                    && source.fetched_at.as_deref() == Some(newer_time.to_rfc3339().as_str())
            })
        })
        .await;
        let (record, _) = load_schedule_object(&first, &source_id).await;
        let source = record.as_source().unwrap();
        assert_eq!(source.active_import.as_ref(), Some(&newer));
        assert!(source.pending_imports.is_empty());
        assert!(
            source.retired_imports.is_empty(),
            "both devices finish batch cleanup"
        );
        for id in older.events.iter().chain(std::iter::once(&older.object_id)) {
            assert!(
                matches!(
                    first.api.get_object_revision(&id.to_string(), 1).await,
                    Err(ClientError::Api { status: 404, .. })
                ),
                "older batch object {id} was purged"
            );
        }
    }

    let pending_time = Utc::now();
    let pending = second
        .stage_calendar_import(
            &source_id,
            &alarms_feed.replace("SUMMARY:Meeting", "SUMMARY:Pending"),
            pending_time,
        )
        .await
        .unwrap();
    wait_for(&first, |state| {
        state
            .files
            .iter()
            .any(|file| file.id == pending.object_id.to_string())
    })
    .await;
    *feed.write().await = alarms_feed.replace("SUMMARY:Meeting", "SUMMARY:Fresh");
    let before_requests = requests.load(Ordering::SeqCst);
    first.sync_calendar_source(&source_id).await.unwrap();
    assert_eq!(
        requests.load(Ordering::SeqCst),
        before_requests + 1,
        "recovering another device's pending batch still fetches fresh"
    );
    assert_eq!(
        first.next_alarms(24, "UTC").await.unwrap()[0].label,
        "Fresh"
    );
    *feed.write().await = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n".into();
    first.sync_calendar_source(&source_id).await.unwrap();
    let before = serde_json::to_value(
        first
            .api
            .list_objects(None, Some(100), None, None)
            .await
            .unwrap(),
    )
    .unwrap();
    let report = first.sync_calendar_source(&source_id).await.unwrap();
    assert!(report.feed_unchanged);
    assert_eq!(report.unchanged, 0);
    assert_eq!(
        before,
        serde_json::to_value(
            first
                .api
                .list_objects(None, Some(100), None, None)
                .await
                .unwrap()
        )
        .unwrap()
    );
    let (record, head) = load_schedule_object(&first, &source_id).await;
    let mut future_source = record.as_source().unwrap().clone();
    let previous = future_source.active_import.as_ref().unwrap().object_id;
    future_source.active_import.as_mut().unwrap().fetched_at =
        Utc::now() + chrono::TimeDelta::hours(1);
    first
        .write_schedule_record(
            &source_id,
            ScheduleRecord::Source(Box::new(future_source)),
            EnvelopePlacement::Revise(head),
        )
        .await
        .unwrap();
    assert!(
        first
            .sync_calendar_source(&source_id)
            .await
            .unwrap()
            .feed_unchanged
    );
    *feed.write().await =
        alarms_feed.replace("SUMMARY:Meeting", "SUMMARY:Fresh after clock change");
    let report = first.sync_calendar_source(&source_id).await.unwrap();
    assert!(!report.feed_unchanged);
    let (record, _) = load_schedule_object(&first, &source_id).await;
    let current = record.as_source().unwrap().active_import.as_ref().unwrap();
    assert_ne!(current.object_id, previous);
    assert!(current.fetched_at <= Utc::now());
    assert!(
        first
            .sync_calendar_source(&source_id)
            .await
            .unwrap()
            .feed_unchanged
    );
    feed_task.abort();
    first.logout(true).await.unwrap();
    let cleared = restarted
        .local_store
        .hydrate_ciphertext_cache(&held_key, RECENT_CLIPBOARD_LIMIT)
        .await
        .unwrap();
    let source = cleared
        .calendar_sources
        .iter()
        .find(|source| source.id == source_id)
        .unwrap();
    assert_eq!(
        source.checked_at, source.fetched_at,
        "logout clears persisted local check times"
    );
    second.logout(true).await.unwrap();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_unusable_pending_batches_are_retired_before_fresh_fetches() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let url = format!("http://{address}");
    let engine = register_proxy_engine(&url, &temp.path().join("client")).await;
    let text = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:meeting\r\nSUMMARY:Fresh\r\nDTSTART:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        (Utc::now() + chrono::TimeDelta::hours(2)).format("%Y%m%dT%H%M%SZ")
    );
    let (feed_url, _, requests, feed_task) = calendar_feed_server(text.clone()).await;
    let source_id = engine
        .add_calendar_source("Recovery", &feed_url)
        .await
        .unwrap();
    let (record, _) = load_schedule_object(&engine, &source_id).await;
    let source_domain_id = record.as_source().unwrap().id;
    for mismatch in ["hash", "event", "raw", "shape"] {
        let old_text = text.replace("SUMMARY:Fresh", "SUMMARY:Pending");
        let mut batch = engine
            .stage_calendar_import(&source_id, &old_text, Utc::now())
            .await
            .unwrap();
        if matches!(mismatch, "event" | "shape") {
            let mut event =
                clipper_schedule::parse_ics(&old_text, source_domain_id, batch.object_id)
                    .unwrap()
                    .events
                    .remove(0);
            event.title = "Previously normalized title".into();
            if mismatch == "shape" {
                let SessionCredentials {
                    api,
                    encryption_key,
                    device_id_typed,
                    signing_key,
                    ..
                } = engine
                    .credentials_for_session(engine.history_epoch.load(Ordering::SeqCst))
                    .await
                    .unwrap();
                let payload_id: ObjectPayloadId = uuid::Uuid::now_v7().into();
                let created_at = Utc::now().to_rfc3339();
                let aad_body = object_envelope_body_for_aad(
                    batch.events[0],
                    ObjectKind::Schedule,
                    EnvelopePlacement::Create,
                    device_id_typed,
                    created_at.clone(),
                    vec![payload_id],
                );
                let record = ScheduleRecord::Ingested(Box::new(event));
                let (meta_nonce, meta_ciphertext) =
                    encrypt_schedule_meta(&record.meta(), &encryption_key, &aad_body).unwrap();
                let mut data = serde_json::to_value(&record).unwrap();
                data.as_object_mut().unwrap().remove("attendance");
                let aad = crypto::object_payload_aad(&aad_body, payload_id).unwrap();
                let (nonce, ciphertext) =
                    crypto::encrypt(&encryption_key, &serde_json::to_vec(&data).unwrap(), &aad)
                        .unwrap();
                let payload = ObjectEnvelopePayload {
                    id: payload_id,
                    nonce: nonce.to_vec(),
                    ciphertext_size: ciphertext.len() as i64,
                    sha256_ciphertext: crypto::sha256(&ciphertext).to_vec(),
                };
                let body = object_envelope_body(
                    batch.events[0],
                    ObjectKind::Schedule,
                    EnvelopePlacement::Create,
                    device_id_typed,
                    created_at,
                    meta_nonce.clone(),
                    crypto::sha256(&meta_ciphertext).to_vec(),
                    vec![payload.clone()],
                );
                let envelope = ObjectEnvelope {
                    signature: crypto::sign_object_envelope_body(&signing_key, &body).unwrap(),
                    body,
                };
                api.object_init(&ObjectInitRequest {
                    id: batch.events[0],
                    kind: ObjectKind::Schedule,
                    meta_nonce,
                    meta_ciphertext,
                    payloads: vec![ObjectPayloadInit {
                        id: payload.id,
                        nonce: payload.nonce,
                        ciphertext_size: payload.ciphertext_size,
                        sha256_ciphertext: payload.sha256_ciphertext,
                        inline_ciphertext: Some(ciphertext),
                    }],
                    envelope,
                })
                .await
                .unwrap();
            } else {
                engine
                    .write_schedule_record(
                        &batch.events[0].to_string(),
                        ScheduleRecord::Ingested(Box::new(event)),
                        EnvelopePlacement::Create,
                    )
                    .await
                    .unwrap();
            }
        } else {
            if mismatch == "raw" {
                let raw = engine
                    .upload_file_bytes(
                        &format!("calendar-import-{}-invalid.ics", source_domain_id),
                        Some("text/calendar"),
                        &[255],
                    )
                    .await
                    .unwrap();
                batch.object_id = raw.parse().unwrap();
                batch.events.clear();
            }
            batch.content_hash = vec![0; 32];
            let (record, head) = load_schedule_object(&engine, &source_id).await;
            let mut source = record.as_source().unwrap().clone();
            if mismatch == "raw" {
                let staged = source.pending_imports.pop().unwrap();
                source.retired_imports.push(staged.into());
                source.pending_imports.push(batch.clone());
            } else {
                source.pending_imports[0] = batch.clone();
            }
            engine
                .write_schedule_record(
                    &source_id,
                    ScheduleRecord::Source(Box::new(source)),
                    EnvelopePlacement::Revise(head),
                )
                .await
                .unwrap();
        }
        let before = requests.load(Ordering::SeqCst);
        engine.sync_calendar_source(&source_id).await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), before + 1);
        let (record, _) = load_schedule_object(&engine, &source_id).await;
        let source = record.as_source().unwrap();
        assert!(source.pending_imports.is_empty());
        assert!(source.retired_imports.is_empty());
        assert_ne!(
            source.active_import.as_ref().unwrap().object_id,
            batch.object_id
        );
        assert_eq!(
            engine.next_alarms(24 * 366, "UTC").await.unwrap()[0].label,
            "Fresh"
        );
        for id in batch.events.iter().chain(std::iter::once(&batch.object_id)) {
            assert!(
                matches!(
                    engine.api.get_object_revision(&id.to_string(), 1).await,
                    Err(ClientError::Api { status: 404, .. })
                ),
                "unusable {mismatch} batch target {id} was purged"
            );
        }
    }
    feed_task.abort();
    engine.logout(true).await.unwrap();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_late_uploads_are_cleaned_after_retirement() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, upstream) = start_server(temp.path()).await;
    let direct_url = format!("http://{upstream}");
    let first = register_proxy_engine(&direct_url, &temp.path().join("first")).await;
    let second = SyncEngine::new_with_data_dir(&direct_url, temp.path().join("second"));
    second
        .login_with_platform("local-test-passphrase", "recovery-test", "Second", "test")
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let arrived = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Semaphore::new(0));
    let fail_revision = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let proxy = tokio::spawn(paused_import_proxy(
        listener,
        upstream,
        Arc::clone(&armed),
        Arc::clone(&arrived),
        Arc::clone(&resume),
        Arc::clone(&fail_revision),
    ));
    let slow = copy_session(&first, &proxy_url, &temp.path().join("slow")).await;
    let source_id = slow
        .add_calendar_source("Calendar", "http://127.0.0.1:9/feed.ics")
        .await
        .unwrap();
    let text = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:meeting\r\nSUMMARY:Older\r\nDTSTART:20261008T120000Z\r\nRRULE:FREQ=DAILY;COUNT=3;BYHOUR=9,17\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    for interrupt_tracking in [false, true] {
        let older = slow
            .stage_calendar_import(&source_id, text, Utc::now())
            .await
            .unwrap();
        wait_for(&second, |state| {
            state
                .files
                .iter()
                .any(|file| file.id == older.object_id.to_string())
        })
        .await;
        armed.store(true, Ordering::SeqCst);
        let uploading = {
            let slow = Arc::clone(&slow);
            let source_id = source_id.clone();
            let older = older.clone();
            tokio::spawn(async move { slow.finish_calendar_import(&source_id, text, &older).await })
        };
        tokio::time::timeout(Duration::from_secs(10), arrived.notified())
            .await
            .expect("event upload reaches the proxy");
        second
            .finish_calendar_import(&source_id, text, &older)
            .await
            .unwrap();
        let newer_text = text.replace("SUMMARY:Older", "SUMMARY:Newer");
        let newer = second
            .stage_calendar_import(&source_id, &newer_text, Utc::now())
            .await
            .unwrap();
        let report = second
            .finish_calendar_import(&source_id, &newer_text, &newer)
            .await
            .unwrap();
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let (record, _) = load_schedule_object(&second, &source_id).await;
        assert!(record.as_source().unwrap().retired_imports.is_empty());
        for id in older.events.iter().chain(std::iter::once(&older.object_id)) {
            assert!(matches!(
                second.api.get_object_revision(&id.to_string(), 1).await,
                Err(ClientError::Api { status: 404, .. })
            ));
        }
        fail_revision.store(interrupt_tracking, Ordering::SeqCst);
        resume.add_permits(1);
        let report = tokio::time::timeout(Duration::from_secs(10), uploading)
            .await
            .expect("late upload finishes")
            .unwrap();
        if interrupt_tracking {
            assert!(report.is_err(), "the retired-entry write was interrupted");
            assert!(!fail_revision.load(Ordering::SeqCst));
            assert!(
                slow.api
                    .get_object_revision(&older.events[0].to_string(), 1)
                    .await
                    .is_ok()
            );
            let restarted = SyncEngine::new_with_data_dir(&proxy_url, temp.path().join("slow"));
            restarted
                .login_with_platform(
                    "local-test-passphrase",
                    "recovery-test",
                    "Restarted",
                    "test",
                )
                .await
                .unwrap();
            wait_for(&restarted, |state| {
                matches!(state.connection_status, ConnectionStatus::Connected)
            })
            .await;
            assert!(
                restarted.sync_calendar_source(&source_id).await.is_err(),
                "the fresh feed is unavailable after recovery"
            );
            restarted.logout(true).await.unwrap();
        } else {
            let report = report.unwrap();
            assert!(report.superseded);
            assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        }
        let (record, _) = load_schedule_object(&second, &source_id).await;
        let source = record.as_source().unwrap();
        assert_eq!(source.active_import.as_ref(), Some(&newer));
        assert!(source.pending_imports.is_empty());
        assert!(source.retired_imports.is_empty());
        for id in older.events.iter().chain(std::iter::once(&older.object_id)) {
            assert!(
                matches!(
                    second.api.get_object_revision(&id.to_string(), 1).await,
                    Err(ClientError::Api { status: 404, .. })
                ),
                "late batch object {id} was purged"
            );
        }
    }
    first.logout(true).await.unwrap();
    second.logout(true).await.unwrap();
    proxy.abort();
}

async fn paused_import_proxy(
    listener: tokio::net::TcpListener,
    upstream: std::net::SocketAddr,
    armed: Arc<std::sync::atomic::AtomicBool>,
    arrived: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Semaphore>,
    fail_revision: Arc<std::sync::atomic::AtomicBool>,
) {
    loop {
        let Ok((client, _)) = listener.accept().await else {
            return;
        };
        let armed = Arc::clone(&armed);
        let arrived = Arc::clone(&arrived);
        let resume = Arc::clone(&resume);
        let fail_revision = Arc::clone(&fail_revision);
        tokio::spawn(async move {
            let mut server = tokio::net::TcpStream::connect(upstream).await.unwrap();
            let (mut client_read, mut client_write) = client.into_split();
            let (mut server_read, mut server_write) = server.split();
            let upload = async move {
                let mut buffer = [0; 65536];
                loop {
                    let read = match client_read.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => read,
                    };
                    if buffer[..read].starts_with(b"POST /api/objects/init ")
                        && armed.swap(false, Ordering::SeqCst)
                    {
                        arrived.notify_one();
                        resume.acquire().await.unwrap().forget();
                    }
                    if buffer[..read].starts_with(b"POST /api/objects/")
                        && buffer[..read]
                            .windows(b"/revisions HTTP/1.1".len())
                            .any(|part| part == b"/revisions HTTP/1.1")
                        && fail_revision.swap(false, Ordering::SeqCst)
                    {
                        break;
                    }
                    if server_write.write_all(&buffer[..read]).await.is_err() {
                        break;
                    }
                }
            };
            let download = tokio::io::copy(&mut server_read, &mut client_write);
            tokio::select! {
                () = upload => {},
                _ = download => {},
            }
        });
    }
}

async fn lossy_proxy(
    listener: tokio::net::TcpListener,
    upstream: std::net::SocketAddr,
    armed: Arc<std::sync::Mutex<Option<&'static [u8]>>>,
) {
    failing_proxy(listener, upstream, armed, None, false).await;
}

async fn failing_proxy(
    listener: tokio::net::TcpListener,
    upstream: std::net::SocketAddr,
    armed: Arc<std::sync::Mutex<Option<&'static [u8]>>>,
    status: Option<u16>,
    before_write: bool,
) {
    loop {
        let Ok((client, _)) = listener.accept().await else {
            return;
        };
        let armed = Arc::clone(&armed);
        tokio::spawn(async move {
            let Ok(server) = tokio::net::TcpStream::connect(upstream).await else {
                return;
            };
            let (mut client_read, mut client_write) = client.into_split();
            let (mut server_read, mut server_write) = server.into_split();
            let swallow = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let marks = Arc::clone(&swallow);
            let upward = tokio::spawn(async move {
                let mut buffer = vec![0_u8; 65536];
                loop {
                    let read = match client_read.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => read,
                    };
                    let chunk = &buffer[..read];
                    let hit = {
                        let mut armed = armed.lock().unwrap();
                        let hit = chunk.starts_with(b"POST ")
                            && armed.is_some_and(|needle| {
                                chunk.windows(needle.len()).any(|w| w == needle)
                            });
                        if hit {
                            *armed = None;
                        }
                        hit
                    };
                    if hit {
                        marks.store(true, Ordering::SeqCst);
                        if before_write {
                            server_write.shutdown().await.unwrap();
                            break;
                        }
                    }
                    if server_write.write_all(chunk).await.is_err() {
                        break;
                    }
                }
            });
            let mut buffer = vec![0_u8; 65536];
            loop {
                let read = match server_read.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                if swallow.load(Ordering::SeqCst) {
                    if let Some(status) = status {
                        let response = format!(
                            "HTTP/1.1 {status} Gateway Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        client_write.write_all(response.as_bytes()).await.unwrap();
                    }
                    break;
                }
                if client_write.write_all(&buffer[..read]).await.is_err() {
                    break;
                }
            }
            upward.abort();
        });
    }
}

async fn register_proxy_engine(url: &str, data: &Path) -> Arc<SyncEngine> {
    let engine = SyncEngine::new_with_data_dir(url, data);
    engine
        .register_with_platform(
            "test-invite",
            "recovery-test",
            "local-test-passphrase",
            "Test",
            "test",
        )
        .await
        .unwrap();
    wait_for(&engine, |state| {
        matches!(state.connection_status, ConnectionStatus::Connected)
    })
    .await;
    engine
}

async fn copy_session(engine: &SyncEngine, url: &str, data: &Path) -> Arc<SyncEngine> {
    let copy = SyncEngine::new_with_data_dir(url, data);
    copy.api.restore_token(engine.api.token().unwrap());
    *copy.encryption_key.write().await = engine.encryption_key.read().await.clone();
    *copy.device_signing_key.write().await = engine.device_signing_key.read().await.clone();
    copy.state.write().await.session = engine.state.read().await.session.clone();
    copy.local_store.set_profile(profile_id_from_encryption_key(
        &copy.current_encryption_key().await.unwrap(),
    ));
    copy
}

async fn load_schedule_object(engine: &SyncEngine, id: &str) -> (ScheduleRecord, LocalHead) {
    let item = engine.api.get_object(id).await.unwrap();
    let key = engine.current_encryption_key().await.unwrap();
    let (record, encrypted) = engine
        .decrypt_schedule_object_item(&engine.api, &item, &key)
        .await
        .unwrap();
    let visible = engine
        .local_store
        .persist_local_schedule_present_encrypted(
            StoredObjectIdentity {
                object_id: id,
                created_at: &item.created_at,
                source_device_id: &item.source_device_id.to_string(),
            },
            record.clone(),
            &encrypted,
            item.created_seq,
            item.created_seq,
            RECENT_CLIPBOARD_LIMIT,
        )
        .await
        .unwrap();
    engine.publish_visible_state(visible).await;
    (record, engine.local_head(id).await.unwrap())
}

async fn server_running_timers(engine: &SyncEngine) -> usize {
    let key = engine.current_encryption_key().await.unwrap();
    let page = engine
        .api
        .list_objects(Some(ObjectKind::Schedule), Some(100), None, None)
        .await
        .unwrap();
    let mut running = 0;
    for item in page.items {
        let (record, _) = engine
            .decrypt_schedule_object_item(&engine.api, &item, &key)
            .await
            .unwrap();
        if matches!(record, ScheduleRecord::Actual(actual) if matches!(actual.span, clipper_schedule::ActualSpan::Running { .. }))
        {
            running += 1;
        }
    }
    running
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn gateway_errors_recover_committed_timer_writes() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, upstream) = start_server(temp.path()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let armed = Arc::new(std::sync::Mutex::new(None));
    let proxy = tokio::spawn(failing_proxy(
        listener,
        upstream,
        Arc::clone(&armed),
        Some(502),
        false,
    ));
    let registered = register_proxy_engine(&url, &temp.path().join("registration")).await;
    let engine = copy_session(&registered, &url, &temp.path().join("client")).await;
    *armed.lock().unwrap() = Some(b"/objects/init HTTP/1.1");
    let timer = engine
        .start_actual(None)
        .await
        .expect("recover the committed start after 502");
    assert!(armed.lock().unwrap().is_none());
    assert_eq!(engine.get_state().await.running_actual.unwrap().id, timer);
    assert_eq!(server_running_timers(&engine).await, 1);
    *armed.lock().unwrap() = Some(b"/revisions HTTP/1.1");
    engine
        .stop_actual(&timer)
        .await
        .expect("recover the committed stop after 502");
    assert!(engine.get_state().await.running_actual.is_none());
    for status in [502, 503, 504, 520, 521, 522, 523, 524, 525, 526, 527] {
        let error = ClientError::Api {
            status,
            error: ErrorResponse::new(ApiErrorCode::Unknown, "gateway"),
        };
        assert!(ambiguous_write_error(&error));
    }
    for status in [400, 401, 403, 404, 409, 413, 429, 500, 501, 505, 519, 528] {
        let error = ClientError::Api {
            status,
            error: ErrorResponse::new(ApiErrorCode::Unknown, "rejected"),
        };
        assert!(!ambiguous_write_error(&error));
    }
    registered.logout(true).await.unwrap();
    proxy.abort();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn lost_delete_replies_recover_tombstones_and_competing_heads() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, upstream) = start_server(temp.path()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let armed = Arc::new(std::sync::Mutex::new(None));
    let proxy = tokio::spawn(lossy_proxy(listener, upstream, Arc::clone(&armed)));
    let registered = register_proxy_engine(&url, &temp.path().join("registration")).await;
    let engine = copy_session(&registered, &url, &temp.path().join("client")).await;
    let file = engine
        .upload_file_bytes("delete.txt", Some("text/plain"), b"file")
        .await
        .unwrap();
    let timer = engine.start_actual(None).await.unwrap();
    engine.stop_actual(&timer).await.unwrap();
    for (id, kind) in [(&file, ObjectKind::File), (&timer, ObjectKind::Schedule)] {
        let old = engine.local_head(id).await.unwrap();
        *armed.lock().unwrap() = Some(b"/revisions HTTP/1.1");
        let result = if kind == ObjectKind::File {
            engine.delete_file(id).await
        } else {
            engine.delete_schedule_object(id).await
        };
        result.expect("recover the committed delete");
        assert!(armed.lock().unwrap().is_none());
        let head = engine.local_head(id).await.unwrap();
        assert_eq!(head.revision, old.revision + 1);
        let deleted = engine
            .api
            .get_object_revision(id, head.revision)
            .await
            .unwrap();
        assert_eq!(
            deleted.envelope.body.operation,
            ObjectEnvelopeOperation::Delete
        );
        assert_eq!(
            head.parent_hash,
            crypto::object_envelope_parent_hash(&deleted.envelope.body).unwrap()
        );
    }
    let timer = engine.start_actual(None).await.unwrap();
    let copy = copy_session(
        &engine,
        &format!("http://{upstream}"),
        &temp.path().join("copy"),
    )
    .await;
    load_schedule_object(&copy, &timer).await;
    copy.stop_actual(&timer).await.unwrap();
    assert!(matches!(
        engine.delete_schedule_object(&timer).await,
        Err(ClientError::Api { status: 409, .. })
    ));
    assert_eq!(
        engine.local_head(&timer).await.unwrap(),
        copy.local_head(&timer).await.unwrap()
    );
    assert!(engine.get_state().await.running_actual.is_none());
    copy.delete_schedule_object(&timer).await.unwrap();
    assert!(matches!(
        engine.delete_schedule_object(&timer).await,
        Err(ClientError::Api { status: 409, .. })
    ));
    assert_eq!(
        engine.local_head(&timer).await.unwrap(),
        copy.local_head(&timer).await.unwrap()
    );
    registered.logout(true).await.unwrap();
    proxy.abort();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn lost_timer_replies_recover_committed_starts_and_stops() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, upstream) = start_server(temp.path()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let armed = Arc::new(std::sync::Mutex::new(None));
    let proxy = tokio::spawn(lossy_proxy(listener, upstream, Arc::clone(&armed)));
    let engine = register_proxy_engine(&url, &temp.path().join("client")).await;
    let timer = engine.start_actual(None).await.unwrap();
    *armed.lock().unwrap() = Some(b"/revisions HTTP/1.1");
    let stopped = engine.stop_actual(&timer).await;
    assert!(
        engine.get_state().await.running_actual.is_none(),
        "lost stop reply: {stopped:?}"
    );
    stopped.unwrap();
    let next = engine.start_actual(None).await.unwrap();
    engine.stop_actual(&next).await.unwrap();
    *armed.lock().unwrap() = Some(b"/objects/init HTTP/1.1");
    let timer = engine.start_actual(None).await.unwrap();
    assert!(armed.lock().unwrap().is_none());
    assert_eq!(engine.get_state().await.running_actual.unwrap().id, timer);
    assert_eq!(server_running_timers(&engine).await, 1);
    let copy = copy_session(
        &engine,
        &format!("http://{upstream}"),
        &temp.path().join("copy"),
    )
    .await;
    load_schedule_object(&copy, &timer).await;
    copy.stop_actual(&timer).await.unwrap();
    assert!(engine.get_state().await.running_actual.is_some());
    assert!(matches!(
        engine.stop_actual(&timer).await,
        Err(ClientError::Api { status: 409, .. })
    ));
    assert!(engine.get_state().await.running_actual.is_none());
    let next = engine.start_actual(None).await.unwrap();
    engine.stop_actual(&next).await.unwrap();
    engine.logout(true).await.unwrap();
    proxy.abort();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn calendar_sync_retries_keep_one_snapshot_and_latest_source_settings() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, upstream) = start_server(temp.path()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let armed = Arc::new(std::sync::Mutex::new(None));
    let proxy = tokio::spawn(lossy_proxy(listener, upstream, Arc::clone(&armed)));
    let engine = register_proxy_engine(&url, &temp.path().join("client")).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let feed_url = format!("http://{}/feed.ics", listener.local_addr().unwrap());
    let feed = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 4096];
            if socket.read(&mut buffer).await.unwrap() == 0 {
                continue;
            }
            let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:meeting\r\nSUMMARY:Planning\r\nDTSTART:20260908T090000Z\r\nDTEND:20260908T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/calendar\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let source = engine.add_calendar_source("Work", &feed_url).await.unwrap();
    *armed.lock().unwrap() = Some(b"/revisions HTTP/1.1");
    let _ = engine.sync_calendar_source(&source).await;
    assert!(armed.lock().unwrap().is_none());
    for _ in 0..3 {
        let _ = engine.sync_calendar_source(&source).await;
    }
    let files = engine
        .api
        .list_objects(Some(ObjectKind::File), Some(100), None, None)
        .await
        .unwrap();
    assert!(
        files.items.len() <= 1,
        "{} raw feeds remain after retries",
        files.items.len()
    );
    let copy = copy_session(
        &engine,
        &format!("http://{upstream}"),
        &temp.path().join("copy"),
    )
    .await;
    let (record, head) = load_schedule_object(&copy, &source).await;
    let ScheduleRecord::Source(mut changed) = record else {
        panic!("calendar source")
    };
    changed.name = "Changed".into();
    copy.write_schedule_record(
        &source,
        ScheduleRecord::Source(changed),
        EnvelopePlacement::Revise(head),
    )
    .await
    .unwrap();
    assert!(
        engine
            .sync_calendar_source(&source)
            .await
            .unwrap()
            .feed_unchanged
    );
    assert_eq!(engine.get_state().await.calendar_sources[0].name, "Changed");
    let after = engine
        .api
        .list_objects(Some(ObjectKind::File), Some(100), None, None)
        .await
        .unwrap();
    assert_eq!(
        after.items.iter().map(|item| item.id).collect::<Vec<_>>(),
        files.items.iter().map(|item| item.id).collect::<Vec<_>>()
    );
    assert_eq!(
        engine.local_head(&source).await.unwrap().revision,
        head.revision + 1
    );
    engine.logout(true).await.unwrap();
    feed.abort();
    proxy.abort();
}

async fn check_schedule(first: Arc<SyncEngine>, second: Arc<SyncEngine>, url: &str, data: &Path) {
    let item = ScheduleItem {
        break_reminders: false,
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

    let running_timer = || {
        ScheduleRecord::Actual(Box::new(clipper_schedule::ActualRecord {
            id: clipper_schedule::ActualId::new(),
            planned: None,
            span: clipper_schedule::ActualSpan::Running {
                started: Utc::now(),
            },
        }))
    };
    first
        .create_schedule_record(running_timer())
        .await
        .expect("a timer started on the first device");
    second
        .create_schedule_record(running_timer())
        .await
        .expect("a timer started on the second device before it saw the first");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while first.running_actual_ids().await.len() < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the first device sees both running timers");
    let replacement = first
        .start_actual(None)
        .await
        .expect("start stops every running timer");
    assert_eq!(first.running_actual_ids().await, vec![replacement.clone()]);
    first.stop_actual(&replacement).await.expect("stop");
    wait_for(&second, |state| state.running_actual.is_none()).await;

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

    // Exercise the streamed revision path as well as the inline one. Payload
    // completion counts this revision's payloads, not every historical one.
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
    interrupted_source.pending_imports = interrupted_source
        .active_import
        .take()
        .into_iter()
        .collect();
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
        .expect_err("recover pending import, then reject the newly fetched invalid feed");
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
    assert!(resumed_source.pending_imports.is_empty());
    assert_eq!(
        resumed_source
            .active_import
            .as_ref()
            .map(|batch| batch.object_id),
        Some(first_import.object_id),
        "recovery promotes the saved snapshot before fetching the new response"
    );
    assert_eq!(
        first
            .sync_calendar_source(&source)
            .await
            .expect("replacement sync")
            .unchanged,
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
    assert_eq!(replacement_import.object_id, first_import.object_id);
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
        .push(clipper_schedule::ingest::RetiredImport {
            object_id: uuid::Uuid::new_v4().into(),
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

    // An unsupported imported rule resolves from the encrypted snapshot at
    // runtime. This source stays separate, so the replacement and deletion
    // checks above keep exercising the ordinary one-event feed.
    *feed.write().await = concat!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
        "BEGIN:VEVENT\r\nUID:cadence-1\r\nSUMMARY:Cadence meeting\r\n",
        "DTSTART:20260908T090000Z\r\nDTEND:20260908T100000Z\r\n",
        "RRULE:FREQ=DAILY;COUNT=3\r\nEND:VEVENT\r\n",
        "BEGIN:VEVENT\r\nUID:unsupported-1\r\nSUMMARY:Split-hour meeting\r\n",
        "DTSTART:20260908T090000Z\r\nDTEND:20260908T100000Z\r\n",
        "RRULE:FREQ=DAILY;COUNT=6;BYHOUR=9,17\r\nEND:VEVENT\r\n",
        "END:VCALENDAR\r\n",
    )
    .into();
    let unsupported_source = first
        .add_calendar_source("Unsupported", &feed_url)
        .await
        .expect("unsupported source");
    assert_eq!(
        first
            .sync_calendar_source(&unsupported_source)
            .await
            .expect("unsupported source sync")
            .added,
        2
    );
    let unsupported_source_id = first
        .local_store
        .schedule_records_with_ids()
        .await
        .into_iter()
        .find_map(|(id, record)| {
            (id == unsupported_source)
                .then(|| record.as_source().map(|source| source.id))
                .flatten()
        })
        .expect("stored unsupported source");
    let unsupported_records = first.local_store.schedule_records_with_ids().await;
    let unsupported_event = unsupported_records
        .iter()
        .find_map(|(_, record)| {
            let event = record.as_ingested()?;
            (event.source == unsupported_source_id && event.uid == "unsupported-1").then_some(event)
        })
        .expect("unsupported imported event");
    let (unsupported_import, unsupported_uid) = match &unsupported_event.recurrence {
        Recurrence::Imported { import, uid } => (*import, uid.as_str()),
        recurrence => panic!("expected imported recurrence, got {recurrence:?}"),
    };
    assert_eq!(unsupported_uid, "unsupported-1");
    let unsupported_occurrences = first
        .expand_schedule(from, "2026-09-11T00:00:00Z", "UTC")
        .await
        .expect("unsupported recurrence expands");
    let split_starts: Vec<_> = unsupported_occurrences
        .iter()
        .filter(|event| {
            event.source.as_deref() == Some("Unsupported") && event.title == "Split-hour meeting"
        })
        .map(|event| event.start.as_str())
        .collect();
    assert_eq!(
        split_starts,
        [
            "2026-09-08T09:00:00Z",
            "2026-09-08T17:00:00Z",
            "2026-09-09T09:00:00Z",
            "2026-09-09T17:00:00Z",
            "2026-09-10T09:00:00Z",
            "2026-09-10T17:00:00Z",
        ]
    );
    let split_occurrence = unsupported_occurrences
        .iter()
        .find(|event| event.title == "Split-hour meeting")
        .expect("unsupported occurrence for timer");
    let split_actual = first
        .start_actual(Some(&split_occurrence.plan_context))
        .await
        .expect("start unsupported recurrence timer");
    first
        .stop_actual(&split_actual)
        .await
        .expect("stop unsupported recurrence timer");

    // With the parsed-rule cache cleared, a native client still expands
    // offline from the ciphertext the local store kept.
    first.import_rules.lock().await.clear();
    let import_head = first
        .local_head(&unsupported_import.to_string())
        .await
        .expect("raw import head");
    assert!(
        first
            .local_store
            .import_file_ciphertext(&unsupported_import.to_string(), import_head)
            .await
            .expect("cached raw import")
            .is_some(),
        "native cache stores the complete encrypted raw import"
    );
    assert!(
        first
            .expand_schedule(from, to, "UTC")
            .await
            .expect("offline unsupported recurrence expansion")
            .iter()
            .any(|event| event.title == "Split-hour meeting"),
        "clearing the parsed-rule cache reuses the encrypted import"
    );
    let saved_token = first.api.token().expect("authenticated token");
    first.api.clear_token();
    first.import_rules.lock().await.clear();
    assert!(
        first
            .expand_schedule(from, to, "UTC")
            .await
            .expect("offline unsupported recurrence expansion")
            .iter()
            .any(|event| event.title == "Split-hour meeting"),
        "native encrypted cache supports expansion without an API token"
    );
    first.api.restore_token(saved_token);
    assert!(!first.import_rules.lock().await.is_empty());

    first
        .delete_file(&unsupported_import.to_string())
        .await
        .expect("delete unsupported raw import");
    let after_unsupported_delete = first
        .expand_schedule(from, to, "UTC")
        .await
        .expect("calendar after unsupported raw deletion");
    assert!(
        after_unsupported_delete
            .iter()
            .all(|event| event.title != "Split-hour meeting"),
        "unsupported recurrence is omitted when its raw import is gone"
    );
    assert!(
        after_unsupported_delete
            .iter()
            .any(|event| event.title == "Cadence meeting"),
        "stored cadence remains usable after raw import deletion"
    );
    assert!(
        first
            .get_state()
            .await
            .schedule_warnings
            .iter()
            .any(
                |warning| warning.contains("Split-hour meeting") && warning.contains("unavailable")
            ),
        "missing raw import produces a warning"
    );
    assert!(
        first
            .local_store
            .schedule_records_with_ids()
            .await
            .into_iter()
            .any(|(id, record)| id == split_actual && matches!(record, ScheduleRecord::Actual(_))),
        "recording survives unsupported raw import deletion"
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
    first
        .delete_schedule_object(&unsupported_source)
        .await
        .expect("remove unsupported source");
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
    let third = SyncEngine::new_with_data_dir(url, data.join("third"));
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
    let resumed = SyncEngine::new_with_data_dir(url, data.join("first"));
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
    first.logout(true).await.expect("logout first");
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
    second.logout(true).await.expect("logout second");
    third.logout(true).await.expect("logout third");
}

#[test]
fn imported_source_readiness_requires_a_complete_active_batch() {
    let source_id = SourceId::new();
    let raw_id: ObjectId = uuid::Uuid::new_v4().into();
    let event_id: ObjectId = uuid::Uuid::new_v4().into();
    let batch = clipper_schedule::ingest::CalendarImport {
        object_id: raw_id,
        fetched_at: Utc::now(),
        content_hash: Vec::new(),
        events: vec![event_id],
    };
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:ready\r\nSUMMARY:Ready\r\nDTSTART:20260908T090000Z\r\nDTEND:20260908T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let mut event = clipper_schedule::parse_ics(feed, source_id, raw_id)
        .expect("valid feed")
        .events
        .pop()
        .expect("event");
    event.import = Some(raw_id);
    let head = LocalHead {
        revision: 1,
        parent_hash: [0; crypto::SHA256_BYTES],
    };
    let source_record =
        |active_import, pending_import: Option<clipper_schedule::ingest::CalendarImport>| {
            ScheduleRecord::Source(Box::new(CalendarSource {
                id: source_id,
                name: "Ready".into(),
                kind: SourceKind::Ics {
                    url: "https://example.test/feed.ics".into(),
                },
                enabled: true,
                owner_email: None,
                alarms_on: true,
                active_import,
                pending_imports: pending_import.into_iter().collect(),
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

    for (target, uid, valid) in [
        (raw_id, event.uid.clone(), true),
        (
            ObjectId::from(uuid::Uuid::new_v4()),
            event.uid.clone(),
            false,
        ),
        (raw_id, "another-event".into(), false),
    ] {
        let mut candidate = event.clone();
        candidate.recurrence = Recurrence::Imported {
            import: target,
            uid,
        };
        let mut records = complete.clone();
        records[1].1 = ScheduleRecord::Ingested(Box::new(candidate.clone()));
        assert_eq!(
            calendar_import::ready_sources(&records).contains(&source_id),
            valid
        );
        let source = records[0].1.as_source().unwrap();
        assert_eq!(
            source.contains_event(&event_id.to_string(), &candidate),
            valid
        );
    }

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

/// One connected workflow covering how edits, stale selections, overridden
/// plans and deletion relate, rather than one assertion per field.
async fn exercise_revision_aware_plans(engine: &SyncEngine) {
    use clipper_schedule::{
        ActualSpan, ObjectRevisionRef, OccurrenceOverride, OccurrenceOverrideData, OverrideChange,
        OverrideId, PlannedRef, RecurrenceId,
    };
    let mut item = ScheduleItem {
        break_reminders: false,
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
        historical.context.span.start().to_rfc3339(),
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
    forged.span = clipper_schedule::TimeRange::new(
        forged.span.start() + chrono::TimeDelta::minutes(1),
        forged.span.end(),
    )
    .unwrap();
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
    // A structural edit from another writer is surfaced. It is never read as
    // an override to a different rule, and never allowed to hide its peers.
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
