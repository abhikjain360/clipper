use super::*;

#[path = "calendar_review_tests.rs"]
mod review_tests;

#[path = "calendar_refresh_tests.rs"]
mod refresh_tests;

async fn held_event(engine: &SyncEngine, id: &str) -> Result<Option<ScheduleRecord>, ClientError> {
    match engine.api.get_object(id).await {
        Ok(_) => Ok(Some(load_schedule_object(engine, id).await.0)),
        Err(ClientError::Api { status: 404, .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

fn feed(anchor: chrono::DateTime<Utc>) -> String {
    let events = (0..1246).map(|index| {
        let start = if index < 100 { anchor + chrono::TimeDelta::days(1) } else { anchor - chrono::TimeDelta::days(500) };
        format!("BEGIN:VEVENT\r\nUID:meeting-{index}\r\nSUMMARY:Meeting {index}\r\nDTSTART:{}\r\nDTEND:{}\r\nEND:VEVENT\r\n", start.format("%Y%m%dT%H%M%SZ"), (start + chrono::TimeDelta::hours(1)).format("%Y%m%dT%H%M%SZ"))
    }).collect::<String>();
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{events}END:VCALENDAR\r\n")
}

async fn event_heads(engine: &SyncEngine) -> HashMap<String, (String, LocalHead)> {
    engine
        .local_store
        .schedule_records_with_heads()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|(id, record, head)| {
            record
                .as_ingested()
                .map(|event| (event.uid.clone(), (id, head)))
        })
        .collect()
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_window_imports_only_eligible_events_and_writes_only_deltas() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let first =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("first")).await;
    first.stop_session_work().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let proxy = tokio::spawn(count_calendar_requests(listener, address, requests.clone()));
    let engine = copy_session(&first, &url, &temp.path().join("counted")).await;
    let now = Utc::now();
    let original = feed(now);
    let (feed_url, text, _, feed_task) = calendar_feed_server(original.clone()).await;
    let id = engine.add_calendar_source("Work", &feed_url).await.unwrap();
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert_eq!(report.added, 100);
    let before = event_heads(&engine).await;
    assert_eq!(before.len(), 100);
    requests.lock().unwrap().clear();
    *text.write().await = original
        .replace("SUMMARY:Meeting 2\r\n", "SUMMARY:Changed\r\n")
        .replace("SUMMARY:Meeting 1000\r\n", "SUMMARY:Changed history\r\n");
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert_eq!((report.added, report.updated, report.tombstoned), (0, 1, 0));
    let after = event_heads(&engine).await;
    assert_eq!(after.len(), 100);
    for (uid, (object_id, head)) in &before {
        let current = &after[uid];
        assert_eq!(&current.0, object_id);
        assert_eq!(
            current.1.revision,
            head.revision + u64::from(uid == "meeting-2")
        );
    }
    let paths = requests.lock().unwrap().clone();
    assert!(
        paths
            .iter()
            .filter(|path| path.starts_with("POST "))
            .count()
            <= 8,
        "{paths:?}"
    );
    for (uid, (event_id, _)) in &before {
        let writes = paths
            .iter()
            .filter(|path| path.starts_with("POST ") && path.contains(event_id))
            .count();
        assert_eq!(writes, usize::from(uid == "meeting-2"));
    }
    assert!(!paths.iter().any(
        |path| path.starts_with("DELETE ") && before.values().any(|(id, _)| path.contains(id))
    ));
    *text.write().await = original.replace("UID:meeting-1\r\n", "UID:replacement\r\n");
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert_eq!((report.added, report.tombstoned), (1, 1));
    let old_id = &before["meeting-1"].0;
    assert_eq!(
        engine
            .api
            .get_object_head(old_id)
            .await
            .unwrap()
            .envelope
            .body
            .operation,
        ObjectEnvelopeOperation::Delete
    );
    *text.write().await = original;
    engine.sync_calendar_source(&id).await.unwrap();
    assert_eq!(&event_heads(&engine).await["meeting-1"].0, old_id);
    proxy.abort();
    feed_task.abort();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_window_preserves_legacy_ids_and_outside_history() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let engine =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("client")).await;
    let now = Utc::now();
    let original = feed(now);
    let (feed_url, text, _, feed_task) = calendar_feed_server(original.clone()).await;
    let id = engine.add_calendar_source("Work", &feed_url).await.unwrap();
    let legacy = engine
        .stage_calendar_import(&id, &original, now)
        .await
        .unwrap();
    engine
        .finish_calendar_import(&id, &original, &legacy)
        .await
        .unwrap();
    let before = event_heads(&engine).await;
    assert_eq!(before.len(), 1246);
    *text.write().await = original
        .replace("SUMMARY:Meeting 2\r\n", "SUMMARY:Changed\r\n")
        .replace("SUMMARY:Meeting 1000\r\n", "SUMMARY:Changed history\r\n");
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert_eq!((report.added, report.updated, report.tombstoned), (0, 1, 0));
    let after = event_heads(&engine).await;
    assert_eq!(after.len(), 1246);
    for (uid, (id, head)) in &before {
        assert_eq!(&after[uid].0, id);
        assert_eq!(
            after[uid].1.revision,
            head.revision + u64::from(uid == "meeting-2")
        );
    }
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    assert!(
        source
            .retained_imports
            .iter()
            .any(|batch| batch.object_id == legacy.object_id && batch.events.len() == 1245)
    );
    assert!(
        engine
            .api
            .get_object(&legacy.object_id.to_string())
            .await
            .is_ok()
    );
    *text.write().await = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n".into();
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert_eq!(report.tombstoned, 100);
    let history = event_heads(&engine).await;
    assert_eq!(history.len(), 1146);
    for (uid, entry) in history {
        assert_eq!(entry, before[&uid]);
    }
    *text.write().await = original;
    engine.sync_calendar_source(&id).await.unwrap();
    let restored = event_heads(&engine).await;
    assert_eq!(restored.len(), 1246);
    for (uid, (object_id, _)) in before {
        assert_eq!(restored[&uid].0, object_id);
    }
    feed_task.abort();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_window_advances_even_when_the_feed_is_unchanged() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let engine =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("client")).await;
    let now = Utc::now();
    let text = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:future\r\nSUMMARY:Future\r\nDTSTART:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        (now + chrono::TimeDelta::days(100)).format("%Y%m%dT%H%M%SZ")
    );
    let (feed_url, _, _, feed_task) = calendar_feed_server(text).await;
    let id = engine.add_calendar_source("Work", &feed_url).await.unwrap();
    assert_eq!(
        engine
            .sync_calendar_source_in_window(&id, now)
            .await
            .unwrap()
            .added,
        0
    );
    let report = engine
        .sync_calendar_source_in_window(&id, now + chrono::TimeDelta::days(20))
        .await
        .unwrap();
    assert!(!report.feed_unchanged);
    assert_eq!(report.added, 1);
    let before = event_heads(&engine).await;
    let report = engine
        .sync_calendar_source_in_window(&id, now + chrono::TimeDelta::days(150))
        .await
        .unwrap();
    assert!(report.feed_unchanged);
    assert_eq!(report.tombstoned, 0);
    assert_eq!(event_heads(&engine).await, before);
    feed_task.abort();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_delta_devices_keep_the_newest_feed_on_the_same_event_ids() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let url = format!("http://{address}");
    let first = register_proxy_engine(&url, &temp.path().join("first")).await;
    let second = copy_session(&first, &url, &temp.path().join("second")).await;
    let now = Utc::now();
    let text = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:meeting\r\nSUMMARY:Initial\r\nDTSTART:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        (now + chrono::TimeDelta::hours(2)).format("%Y%m%dT%H%M%SZ")
    );
    let id = first
        .add_calendar_source("Work", "http://127.0.0.1/feed.ics")
        .await
        .unwrap();
    let initial = first
        .stage_windowed_calendar_import(&id, &text, now)
        .await
        .unwrap();
    first
        .finish_calendar_import(&id, &text, &initial)
        .await
        .unwrap();
    for parallel in [false, true] {
        let older_text = text.replace("Initial", "Older");
        let newer_text = text.replace("Initial", "Newer");
        let fetched_at = Utc::now();
        let older = first
            .stage_windowed_calendar_import(&id, &older_text, fetched_at)
            .await
            .unwrap();
        let newer = second
            .stage_windowed_calendar_import(
                &id,
                &newer_text,
                fetched_at + chrono::TimeDelta::microseconds(1),
            )
            .await
            .unwrap();
        assert_eq!(older.events, initial.events);
        assert_eq!(newer.events, initial.events);
        if parallel {
            let (left, right) = tokio::join!(
                first.finish_calendar_import(&id, &older_text, &older),
                second.finish_calendar_import(&id, &newer_text, &newer)
            );
            assert!(left.unwrap().skipped.is_empty());
            assert!(right.unwrap().skipped.is_empty());
        } else {
            second
                .finish_calendar_import(&id, &newer_text, &newer)
                .await
                .unwrap();
            assert!(
                first
                    .finish_calendar_import(&id, &older_text, &older)
                    .await
                    .unwrap()
                    .superseded
            );
        }
        let source = first.read_calendar_source(&id).await.unwrap().0;
        assert_eq!(source.active_import, Some(newer));
        assert!(source.pending_imports.is_empty());
        assert!(source.retired_imports.is_empty());
        let (record, _) = load_schedule_object(&first, &initial.events[0].to_string()).await;
        assert_eq!(record.as_ingested().unwrap().title, "Newer");
        let batch = first
            .stage_windowed_calendar_import(&id, &text, Utc::now())
            .await
            .unwrap();
        first
            .finish_calendar_import(&id, &text, &batch)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_delta_rebases_competing_changes_and_removals() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let url = format!("http://{address}");
    let first = register_proxy_engine(&url, &temp.path().join("first")).await;
    let second = copy_session(&first, &url, &temp.path().join("second")).await;
    let now = Utc::now();
    let original = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:first\r\nSUMMARY:First\r\nDTSTART:{}\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:second\r\nSUMMARY:Second\r\nDTSTART:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        (now + chrono::TimeDelta::hours(2)).format("%Y%m%dT%H%M%SZ"),
        (now + chrono::TimeDelta::hours(3)).format("%Y%m%dT%H%M%SZ")
    );
    let id = first
        .add_calendar_source("Work", "http://127.0.0.1/feed.ics")
        .await
        .unwrap();
    let initial = first
        .stage_windowed_calendar_import(&id, &original, now)
        .await
        .unwrap();
    first
        .finish_calendar_import(&id, &original, &initial)
        .await
        .unwrap();
    let initial_ids = event_heads(&first).await;
    for removed in [false, true] {
        let older_text = original
            .replace("SUMMARY:First", "SUMMARY:Older first")
            .replace("UID:second\r\n", "UID:replacement\r\n");
        let newer_text = if removed {
            let start = original.find("BEGIN:VEVENT").unwrap();
            let end =
                original[start..].find("END:VEVENT\r\n").unwrap() + start + "END:VEVENT\r\n".len();
            format!("{}{}", &original[..start], &original[end..])
        } else {
            original.replace("SUMMARY:Second", "SUMMARY:Newer second")
        };
        let fetched_at = Utc::now();
        let older = first
            .stage_windowed_calendar_import(&id, &older_text, fetched_at)
            .await
            .unwrap();
        let newer = second
            .stage_windowed_calendar_import(
                &id,
                &newer_text,
                fetched_at + chrono::TimeDelta::microseconds(1),
            )
            .await
            .unwrap();
        assert!(
            first
                .finish_calendar_import(&id, &older_text, &older)
                .await
                .unwrap()
                .skipped
                .is_empty()
        );
        let report = second
            .finish_calendar_import(&id, &newer_text, &newer)
            .await
            .unwrap();
        assert!(!report.superseded);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let source = first.read_calendar_source(&id).await.unwrap().0;
        assert_eq!(
            source.active_import.as_ref().unwrap().object_id,
            newer.object_id
        );
        assert!(source.pending_imports.is_empty());
        assert!(source.retired_imports.is_empty());
        if removed {
            assert!(
                held_event(&first, &initial_ids["first"].0)
                    .await
                    .unwrap()
                    .is_none()
            );
        } else {
            let event = held_event(&first, &initial_ids["first"].0)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event.as_ingested().unwrap().title, "First");
        }
        let event = held_event(&first, &initial_ids["second"].0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            event.as_ingested().unwrap().title,
            if removed { "Second" } else { "Newer second" }
        );
        let replacement = IngestedEvent::derive_id(source.id, "replacement").to_string();
        assert!(held_event(&first, &replacement).await.unwrap().is_none());
        let reset = first
            .stage_windowed_calendar_import(&id, &original, Utc::now())
            .await
            .unwrap();
        first
            .finish_calendar_import(&id, &original, &reset)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_delta_cleans_late_writes_without_erasing_history() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let url = format!("http://{address}");
    let first = register_proxy_engine(&url, &temp.path().join("first")).await;
    let second = copy_session(&first, &url, &temp.path().join("second")).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let arrived = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Semaphore::new(0));
    let proxy = tokio::spawn(paused_import_proxy(
        listener,
        address,
        armed.clone(),
        arrived.clone(),
        resume.clone(),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    ));
    let slow = copy_session(&first, &proxy_url, &temp.path().join("slow")).await;
    let now = Utc::now();
    let text = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:meeting\r\nSUMMARY:Older\r\nDTSTART:{}\r\nRRULE:FREQ=DAILY;COUNT=3;BYHOUR=9,17\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        (now + chrono::TimeDelta::days(1)).format("%Y%m%dT%H%M%SZ")
    );
    let id = slow
        .add_calendar_source("Work", "http://127.0.0.1/feed.ics")
        .await
        .unwrap();
    let older = slow
        .stage_windowed_calendar_import(&id, &text, now)
        .await
        .unwrap();
    armed.store(true, Ordering::SeqCst);
    let uploading = {
        let slow = slow.clone();
        let id = id.clone();
        let text = text.clone();
        let older = older.clone();
        tokio::spawn(async move { slow.finish_calendar_import(&id, &text, &older).await })
    };
    tokio::time::timeout(Duration::from_secs(10), arrived.notified())
        .await
        .unwrap();
    second
        .finish_calendar_import(&id, &text, &older)
        .await
        .unwrap();
    let newer_text = text
        .replace("UID:meeting", "UID:newer")
        .replace("SUMMARY:Older", "SUMMARY:Newer");
    let newer = second
        .stage_windowed_calendar_import(&id, &newer_text, Utc::now())
        .await
        .unwrap();
    assert!(
        second
            .finish_calendar_import(&id, &newer_text, &newer)
            .await
            .unwrap()
            .skipped
            .is_empty()
    );
    resume.add_permits(1);
    let report = tokio::time::timeout(Duration::from_secs(10), uploading)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(report.superseded);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let source = second.read_calendar_source(&id).await.unwrap().0;
    assert_eq!(
        source.active_import.as_ref().unwrap().object_id,
        newer.object_id
    );
    assert!(source.pending_imports.is_empty());
    assert!(source.retired_imports.is_empty());
    let old_id = older.events[0].to_string();
    assert!(held_event(&second, &old_id).await.unwrap().is_none());
    assert!(
        second.api.get_object_revision(&old_id, 1).await.is_ok(),
        "history remains available"
    );
    assert!(
        second
            .api
            .get_object_revision(&older.object_id.to_string(), 1)
            .await
            .is_ok()
    );
    let event = held_event(&second, &newer.events[0].to_string())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.as_ingested().unwrap().title, "Newer");
    proxy.abort();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_delta_recovers_legacy_events_after_a_removal_is_overtaken() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let engine =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("client")).await;
    let now = Utc::now();
    let text = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:meeting\r\nSUMMARY:Meeting\r\nDTSTART:{}\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:history\r\nSUMMARY:History\r\nDTSTART:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        (now + chrono::TimeDelta::days(1)).format("%Y%m%dT%H%M%SZ"),
        (now - chrono::TimeDelta::days(500)).format("%Y%m%dT%H%M%SZ")
    );
    let id = engine
        .add_calendar_source("Work", "http://127.0.0.1/feed.ics")
        .await
        .unwrap();
    let legacy = engine.stage_calendar_import(&id, &text, now).await.unwrap();
    engine
        .finish_calendar_import(&id, &text, &legacy)
        .await
        .unwrap();
    let heads = event_heads(&engine).await;
    let event_id = &heads["meeting"].0;
    let record = load_schedule_object(&engine, event_id).await.0;
    let empty = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n";
    let removal = engine
        .stage_windowed_calendar_import(&id, empty, Utc::now())
        .await
        .unwrap();
    engine
        .finish_calendar_import(&id, empty, &removal)
        .await
        .unwrap();
    let head = engine.api.get_object_head(event_id).await.unwrap();
    engine
        .write_schedule_record(
            event_id,
            record,
            EnvelopePlacement::Revise(LocalHead {
                revision: head.revision,
                parent_hash: crypto::object_envelope_parent_hash(&head.envelope.body).unwrap(),
            }),
        )
        .await
        .unwrap();
    let next = engine
        .stage_windowed_calendar_import(&id, empty, Utc::now())
        .await
        .unwrap();
    let report = engine
        .finish_calendar_import(&id, empty, &next)
        .await
        .unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(held_event(&engine, event_id).await.unwrap().is_none());
    assert!(engine.api.get_object_revision(event_id, 1).await.is_ok());
    assert_eq!(event_heads(&engine).await["history"], heads["history"]);
    assert!(
        engine
            .api
            .get_object(&legacy.object_id.to_string())
            .await
            .is_ok()
    );
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    assert!(source.retired_imports.is_empty());
    assert!(source.pending_imports.is_empty());
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_delta_replaces_event_ordering_from_a_clock_too_far_ahead() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let engine =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("client")).await;
    let now = Utc::now();
    let text = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:meeting\r\nSUMMARY:Initial\r\nDTSTART:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        (now + chrono::TimeDelta::days(1)).format("%Y%m%dT%H%M%SZ")
    );
    let id = engine
        .add_calendar_source("Work", "http://127.0.0.1/feed.ics")
        .await
        .unwrap();
    let initial = engine
        .stage_windowed_calendar_import(&id, &text, now)
        .await
        .unwrap();
    engine
        .finish_calendar_import(&id, &text, &initial)
        .await
        .unwrap();
    let event_id = initial.events[0].to_string();
    let (record, head) = load_schedule_object(&engine, &event_id).await;
    let mut event = record.as_ingested().unwrap().clone();
    let future = now + chrono::TimeDelta::days(1);
    event.import_fetched_at = Some(future);
    engine
        .write_schedule_record(
            &event_id,
            ScheduleRecord::Ingested(Box::new(event)),
            EnvelopePlacement::Revise(head),
        )
        .await
        .unwrap();
    let (mut source, head) = engine.read_calendar_source(&id).await.unwrap();
    source.active_import.as_mut().unwrap().fetched_at = future;
    engine
        .write_schedule_record(
            &id,
            ScheduleRecord::Source(Box::new(source)),
            EnvelopePlacement::Revise(head),
        )
        .await
        .unwrap();
    let changed = text.replace("Initial", "Fresh");
    let batch = engine
        .stage_windowed_calendar_import(&id, &changed, Utc::now())
        .await
        .unwrap();
    let report = engine
        .finish_calendar_import(&id, &changed, &batch)
        .await
        .unwrap();
    assert!(!report.superseded);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let event = held_event(&engine, &event_id).await.unwrap().unwrap();
    assert_eq!(event.as_ingested().unwrap().title, "Fresh");
    assert_eq!(
        event.as_ingested().unwrap().snapshot(),
        Some(batch.object_id)
    );
    assert_eq!(
        engine
            .read_calendar_source(&id)
            .await
            .unwrap()
            .0
            .active_import
            .as_ref()
            .unwrap()
            .object_id,
        batch.object_id
    );
}
