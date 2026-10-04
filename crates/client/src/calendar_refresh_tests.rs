use clipper_schedule::ingest::RetiredImport;

use super::*;

fn calendar(start: chrono::DateTime<Utc>, title: &str, rule: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:meeting\r\nSUMMARY:{title}\r\nDTSTART:{}\r\nDTEND:{}\r\n{rule}END:VEVENT\r\nEND:VCALENDAR\r\n",
        start.format("%Y%m%dT%H%M%SZ"),
        (start + chrono::TimeDelta::hours(1)).format("%Y%m%dT%H%M%SZ")
    )
}

fn several_events(start: chrono::DateTime<Utc>, rule: &str) -> String {
    let text = calendar(start, "Meeting", rule);
    let first = text.find("BEGIN:VEVENT").unwrap();
    let last = text.find("END:VEVENT\r\n").unwrap() + "END:VEVENT\r\n".len();
    let events = ["one", "two", "three"]
        .map(|uid| text[first..last].replace("UID:meeting", &format!("UID:{uid}")))
        .join("");
    format!("{}{}{}", &text[..first], events, &text[last..])
}

async fn interrupted_calendar_proxy(
    listener: tokio::net::TcpListener,
    upstream: std::net::SocketAddr,
    failure: Arc<std::sync::Mutex<Option<String>>>,
) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let (mut client, _) = tokio::select! {
            accepted = listener.accept() => accepted.unwrap(),
            _ = connections.join_next(), if !connections.is_empty() => continue,
        };
        let failure = failure.clone();
        connections.spawn(async move {
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            let (end, length) = loop {
                let count = client.read(&mut buffer).await.unwrap();
                if count == 0 {
                    return;
                }
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let fail = {
                        let mut failure = failure.lock().unwrap();
                        let fail = failure.as_ref().is_some_and(|request| headers.starts_with(request));
                        if fail {
                            failure.take();
                        }
                        fail
                    };
                    if fail {
                        client.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                        return;
                    }
                    let length = headers.lines().find_map(|line| {
                        line.to_ascii_lowercase().strip_prefix("content-length:").map(|value| value.trim().parse::<usize>().unwrap())
                    }).unwrap_or(0);
                    break (end, length);
                }
            };
            while bytes.len() < end + 4 + length {
                let count = client.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
            }
            let mut server = tokio::net::TcpStream::connect(upstream).await.unwrap();
            server.write_all(&bytes[..end]).await.unwrap();
            server.write_all(b"\r\nConnection: close\r\n\r\n").await.unwrap();
            server.write_all(&bytes[end + 4..]).await.unwrap();
            tokio::io::copy(&mut server, &mut client).await.unwrap();
        });
    }
}

async fn visible_calendar(engine: &Arc<SyncEngine>, now: chrono::DateTime<Utc>, changed: &[&str]) {
    let from = now.to_rfc3339();
    let to = (now + chrono::TimeDelta::days(10)).to_rfc3339();
    let occurrences = engine.expand_schedule(&from, &to, "UTC").await.unwrap();
    assert_eq!(occurrences.len(), 9, "{occurrences:?}");
    assert_eq!(
        occurrences
            .iter()
            .map(|occurrence| &occurrence.item_id)
            .collect::<HashSet<_>>()
            .len(),
        3
    );
    let expected_hour = |title: &str| {
        if title == "Changed one" {
            [10, 18]
        } else {
            [9, 17]
        }
    };
    for occurrence in &occurrences {
        let start = chrono::DateTime::parse_from_rfc3339(&occurrence.start).unwrap();
        assert!(expected_hour(&occurrence.title).contains(&chrono::Timelike::hour(&start)));
    }
    for title in changed {
        assert_eq!(
            occurrences
                .iter()
                .filter(|occurrence| occurrence.title == *title)
                .count(),
            3
        );
    }
    for alarms in [
        engine.next_alarms(24 * 10, "UTC").await.unwrap(),
        engine.desktop_alarms(&from, &to, "UTC").await.unwrap(),
    ] {
        assert_eq!(alarms.len(), 9, "{alarms:?}");
        for alarm in alarms {
            let occurrence = occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.item_id == alarm.item_id
                        && chrono::DateTime::parse_from_rfc3339(&occurrence.start)
                            .unwrap()
                            .timestamp_millis()
                            == alarm.occurrence_start_millis
                })
                .unwrap();
            assert_eq!(
                alarm.fire_at_millis,
                alarm.occurrence_start_millis - 300_000
            );
            assert!(alarm.label.contains(&occurrence.title));
        }
    }
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_pending_delta_keeps_events_and_alarms_visible() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let url = format!("http://{address}");
    let first = register_proxy_engine(&url, &temp.path().join("first")).await;
    first.stop_session_work().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let failure = Arc::new(std::sync::Mutex::new(None));
    let proxy = tokio::spawn(interrupted_calendar_proxy(
        listener,
        address,
        failure.clone(),
    ));
    let engine = copy_session(&first, &proxy_url, &temp.path().join("refreshing")).await;
    let now = Utc::now();
    let start = (now + chrono::TimeDelta::days(1))
        .date_naive()
        .and_hms_opt(9, 0, 0)
        .unwrap()
        .and_utc();
    let original = several_events(start, "RRULE:FREQ=DAILY;COUNT=3;BYHOUR=9,17\r\n");
    let (feed_url, feed, _, task) = calendar_feed_server(original.clone()).await;
    let id = engine.add_calendar_source("Work", &feed_url).await.unwrap();
    let device = engine.current_device_id().await.unwrap();
    engine
        .set_calendar_source_target_device(&id, Some(&device))
        .await
        .unwrap();
    engine.sync_calendar_source(&id).await.unwrap();
    visible_calendar(&engine, now, &[]).await;
    let before = event_heads(&engine).await;
    let mut updated = original
        .replacen("SUMMARY:Meeting", "SUMMARY:Changed one", 1)
        .replacen("BYHOUR=9,17", "BYHOUR=10,18", 1);
    updated = updated.replacen("SUMMARY:Meeting", "SUMMARY:Changed two", 1);
    *feed.write().await = updated;
    *failure.lock().unwrap() = Some(format!("POST /api/objects/{}/revisions ", before["two"].0));
    let result = tokio::time::timeout(Duration::from_secs(20), engine.sync_calendar_source(&id))
        .await
        .unwrap();
    assert!(result.is_err(), "{result:?}");
    assert!(failure.lock().unwrap().is_none());
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    assert_eq!(source.pending_imports.len(), 1);
    let pending = source.pending_imports[0].object_id;
    let held = event_heads(&engine).await;
    assert_eq!(held["one"].1.revision, before["one"].1.revision + 1);
    assert_eq!(held["two"], before["two"]);
    assert_eq!(held["three"], before["three"]);
    let event = held_event(&engine, &before["one"].0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.as_ingested().unwrap().snapshot(), Some(pending));
    visible_calendar(&engine, now, &["Changed one"]).await;
    let second = copy_session(&first, &url, &temp.path().join("second")).await;
    let generation = second.local_store.start_generation().await;
    let seq = Utc::now().timestamp_micros();
    second.snapshot_files(generation, seq).await.unwrap();
    second.snapshot_schedule(generation, seq).await.unwrap();
    visible_calendar(&second, now, &["Changed one"]).await;
    assert_eq!(
        second
            .read_calendar_source(&id)
            .await
            .unwrap()
            .0
            .pending_imports
            .len(),
        1
    );
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    assert!(source.pending_imports.is_empty());
    assert_eq!(source.active_import.as_ref().unwrap().object_id, pending);
    assert_eq!(event_heads(&engine).await.len(), 3);
    visible_calendar(&engine, now, &["Changed one", "Changed two"]).await;
    let generation = second.local_store.start_generation().await;
    let seq = Utc::now().timestamp_micros();
    second.snapshot_files(generation, seq).await.unwrap();
    second.snapshot_schedule(generation, seq).await.unwrap();
    visible_calendar(&second, now, &["Changed one", "Changed two"]).await;
    assert!(
        engine
            .sync_calendar_source(&id)
            .await
            .unwrap()
            .feed_unchanged
    );
    proxy.abort();
    task.abort();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_removals_finish_once_and_leave_no_history_work() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let first =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("first")).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let proxy = tokio::spawn(count_calendar_requests(listener, address, requests.clone()));
    let engine = copy_session(&first, &proxy_url, &temp.path().join("counted")).await;
    let now = Utc::now();
    let text = several_events(now + chrono::TimeDelta::days(1), "");
    let (url, feed, _, task) = calendar_feed_server(text.clone()).await;
    let id = engine.add_calendar_source("Work", &url).await.unwrap();
    engine.sync_calendar_source(&id).await.unwrap();
    let before = event_heads(&engine).await;
    let removed: Vec<ObjectId> = ["two", "three"]
        .map(|uid| before[uid].0.parse().unwrap())
        .into();
    *feed.write().await =
        calendar(now + chrono::TimeDelta::days(1), "Meeting", "").replace("UID:meeting", "UID:one");
    assert_eq!(
        engine.sync_calendar_source(&id).await.unwrap().tombstoned,
        2
    );
    let (mut source, head) = engine.read_calendar_source(&id).await.unwrap();
    assert!(source.imports().all(|batch| batch.removed.is_empty()));
    source.active_import.as_mut().unwrap().removed = removed.clone();
    source.retained_imports[0].removed = removed.clone();
    engine
        .write_schedule_record(
            &id,
            ScheduleRecord::Source(Box::new(source)),
            EnvelopePlacement::Revise(head),
        )
        .await
        .unwrap();
    engine.sync_calendar_source(&id).await.unwrap();
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    assert!(source.imports().all(|batch| batch.removed.is_empty()));
    requests.lock().unwrap().clear();
    for _ in 0..3 {
        assert!(
            engine
                .sync_calendar_source(&id)
                .await
                .unwrap()
                .feed_unchanged
        );
    }
    let paths = requests.lock().unwrap().clone();
    assert!(
        !paths
            .iter()
            .any(|path| removed.iter().any(|id| path.contains(&id.to_string()))),
        "{paths:?}"
    );
    *feed.write().await = text.replace("SUMMARY:Meeting", "SUMMARY:Changed");
    engine.sync_calendar_source(&id).await.unwrap();
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    assert!(source.imports().all(|batch| batch.removed.is_empty()));
    proxy.abort();
    task.abort();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_planning_reuses_cached_raw_snapshots() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let first =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("first")).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let proxy = tokio::spawn(count_calendar_requests(listener, address, requests.clone()));
    let engine = copy_session(&first, &proxy_url, &temp.path().join("counted")).await;
    let now = Utc::now();
    let text = several_events(
        now + chrono::TimeDelta::days(1),
        "RRULE:FREQ=DAILY;COUNT=3;BYHOUR=9,17\r\n",
    );
    let (url, feed, _, task) = calendar_feed_server(text).await;
    let id = engine.add_calendar_source("Work", &url).await.unwrap();
    engine.sync_calendar_source(&id).await.unwrap();
    {
        let mut text = feed.write().await;
        *text = text.replacen("BYHOUR=9,17", "BYHOUR=10,18", 1);
    }
    assert_eq!(engine.sync_calendar_source(&id).await.unwrap().updated, 1);
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    let raw: Vec<_> = source
        .imports()
        .map(|batch| batch.object_id.to_string())
        .collect();
    assert_eq!(raw.len(), 2);
    requests.lock().unwrap().clear();
    engine.import_rules.lock().await.clear();
    for _ in 0..3 {
        assert!(
            engine
                .sync_calendar_source(&id)
                .await
                .unwrap()
                .feed_unchanged
        );
    }
    let paths = requests.lock().unwrap().clone();
    assert!(
        !paths
            .iter()
            .any(|path| raw.iter().any(|id| path.contains(id))),
        "{paths:?}"
    );
    proxy.abort();
    task.abort();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_retired_history_keeps_one_group_per_snapshot() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let engine =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("client")).await;
    let now = Utc::now();
    let text = several_events(now + chrono::TimeDelta::days(1), "");
    let later = several_events(now + chrono::TimeDelta::days(120), "");
    let (url, feed, _, task) = calendar_feed_server(text).await;
    let id = engine.add_calendar_source("Work", &url).await.unwrap();
    engine.sync_calendar_source(&id).await.unwrap();
    let batch = engine
        .stage_windowed_calendar_import(&id, &later, Utc::now())
        .await
        .unwrap();
    assert_eq!(batch.events.len(), 3);
    let (mut source, head) = engine.read_calendar_source(&id).await.unwrap();
    let anchor = source.import_anchor.unwrap();
    let parsed = clipper_schedule::parse_ics(&later, source.id, batch.object_id).unwrap();
    for event in parsed.events {
        let mut event = event;
        event.import = Some(anchor);
        event.raw_import = Some(batch.object_id);
        event.import_fetched_at = Some(batch.fetched_at);
        let event_id = event.id.to_string();
        let (_, event_head) = load_schedule_object(&engine, &event_id).await;
        engine
            .write_schedule_record(
                &event_id,
                ScheduleRecord::Ingested(Box::new(event)),
                EnvelopePlacement::Revise(event_head),
            )
            .await
            .unwrap();
    }
    let winner = source.active_import.as_ref().unwrap().object_id;
    source.pending_imports.clear();
    let mut retired = RetiredImport::from(batch.clone());
    retired.superseded_by = Some(winner);
    source.superseded.insert(batch.object_id, winner);
    source.retired_imports.push(retired);
    engine
        .write_schedule_record(
            &id,
            ScheduleRecord::Source(Box::new(source)),
            EnvelopePlacement::Revise(head),
        )
        .await
        .unwrap();
    *feed.write().await = later;
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    let groups: Vec<_> = source
        .retained_imports
        .iter()
        .filter(|group| group.object_id == batch.object_id)
        .collect();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].events.len(), 3);
    assert_eq!(groups[0].uids.len(), 3);
    assert_eq!(groups[0].hashes.len(), 3);
    assert!(source.retired_imports.is_empty());
    assert!(
        engine
            .api
            .get_object(&batch.object_id.to_string())
            .await
            .is_ok()
    );
    assert!(
        engine
            .sync_calendar_source(&id)
            .await
            .unwrap()
            .feed_unchanged
    );
    task.abort();
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_interrupted_revisions_resume_with_missing_snapshots() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let server = format!("http://{address}");
    let engine = register_proxy_engine(&server, &temp.path().join("client")).await;
    let now = Utc::now();
    for keep_active in [false, true] {
        for keep_pending in [false, true] {
            let first = calendar(
                now + chrono::TimeDelta::days(1),
                "First",
                "RRULE:FREQ=DAILY;COUNT=3;BYHOUR=9,17\r\n",
            );
            let later = calendar(
                now + chrono::TimeDelta::days(120),
                "Later",
                "RRULE:FREQ=DAILY;COUNT=3;BYHOUR=10,18\r\n",
            );
            let (url, feed, _, task) = calendar_feed_server(first).await;
            let id = engine.add_calendar_source("Work", &url).await.unwrap();
            engine.sync_calendar_source(&id).await.unwrap();
            let source = engine.read_calendar_source(&id).await.unwrap().0;
            let anchor = source.import_anchor.unwrap();
            let event_id = source.active_import.as_ref().unwrap().events[0].to_string();
            if !keep_active {
                engine.delete_file(&anchor.to_string()).await.unwrap();
            }
            let batch = engine
                .stage_windowed_calendar_import(&id, &later, Utc::now())
                .await
                .unwrap();
            let (_, head) = load_schedule_object(&engine, &event_id).await;
            let mut event = clipper_schedule::parse_ics(&later, source.id, batch.object_id)
                .unwrap()
                .events
                .remove(0);
            event.import = Some(anchor);
            event.raw_import = Some(batch.object_id);
            event.import_fetched_at = Some(batch.fetched_at);
            engine
                .write_schedule_record(
                    &event_id,
                    ScheduleRecord::Ingested(Box::new(event)),
                    EnvelopePlacement::Revise(head),
                )
                .await
                .unwrap();
            if !keep_pending {
                let raw = batch.object_id.to_string();
                engine
                    .write_tombstone(&raw, ObjectKind::File)
                    .await
                    .unwrap();
                let response = engine.api.delete_object(&raw).await.unwrap();
                engine
                    .local_store
                    .apply_local_delete(
                        ObjectKind::File,
                        &raw,
                        response.deleted_seq,
                        RECENT_CLIPBOARD_LIMIT,
                    )
                    .await
                    .unwrap();
            }
            *feed.write().await = later;
            let restarted = copy_session(
                &engine,
                &server,
                &temp
                    .path()
                    .join(format!("restart-{keep_active}-{keep_pending}")),
            )
            .await;
            let report =
                tokio::time::timeout(Duration::from_secs(20), restarted.sync_calendar_source(&id))
                    .await
                    .unwrap()
                    .unwrap();
            assert!(report.skipped.is_empty(), "{:?}", report.skipped);
            let source = restarted.read_calendar_source(&id).await.unwrap().0;
            assert!(source.pending_imports.is_empty());
            assert!(source.retired_imports.is_empty());
            let stored = held_event(&restarted, &event_id).await.unwrap().unwrap();
            assert_eq!(stored.as_ingested().unwrap().title, "Later");
            assert!(source.contains_event(&event_id, stored.as_ingested().unwrap()));
            assert!(
                restarted
                    .expand_schedule(
                        now.to_rfc3339().as_str(),
                        (now + chrono::TimeDelta::days(90)).to_rfc3339().as_str(),
                        "UTC"
                    )
                    .await
                    .unwrap()
                    .iter()
                    .all(|event| event.item_id != stored.as_ingested().unwrap().id.to_string())
            );
            assert!(
                restarted
                    .sync_calendar_source(&id)
                    .await
                    .unwrap()
                    .feed_unchanged
            );
            task.abort();
        }
    }
}

fn large_calendar(now: chrono::DateTime<Utc>, changed: bool) -> String {
    let events = (0..1246).map(|index| {
        let start = if index < 700 { now + chrono::TimeDelta::days(1) } else { now - chrono::TimeDelta::days(500) };
        let title = if changed && index < 700 { "Changed" } else { "Meeting" };
        format!("BEGIN:VEVENT\r\nUID:work-{index}@example.com\r\nSUMMARY:{title} {index}\r\nDTSTART:{}\r\nDTEND:{}\r\nEND:VEVENT\r\n", start.format("%Y%m%dT%H%M%SZ"), (start + chrono::TimeDelta::hours(1)).format("%Y%m%dT%H%M%SZ"))
    }).collect::<String>();
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{events}END:VCALENDAR\r\n")
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_large_deltas_fit_and_preserve_legacy_ids() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let engine =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("client")).await;
    let now = Utc::now();
    let first = large_calendar(now, false);
    let later = large_calendar(now, true);
    let id = engine
        .add_calendar_source("Work", "http://127.0.0.1/feed.ics")
        .await
        .unwrap();
    let legacy = engine
        .stage_calendar_import(&id, &first, now)
        .await
        .unwrap();
    engine
        .finish_calendar_import(&id, &first, &legacy)
        .await
        .unwrap();
    let before = event_heads(&engine).await;
    assert_eq!(before.len(), 1246);
    let batch = engine
        .stage_windowed_calendar_import(&id, &later, Utc::now())
        .await
        .unwrap();
    assert_eq!(batch.events.len(), 700);
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    let record = ScheduleRecord::Source(Box::new(source.clone()));
    let packed = serde_json::to_vec(&record).unwrap();
    assert!(packed.len() < 262_144, "{}", packed.len());
    let mut plain = serde_json::to_value(&record).unwrap();
    plain["delta"] = serde_json::json!({
        "active": source.active_import, "pending": source.pending_imports,
        "retired": source.retired_imports, "retained": source.retained_imports,
        "event_ids": source.event_ids, "removing": source.removing,
        "superseded": source.superseded, "pending_retirements": source.pending_retirements,
    });
    assert!(serde_json::to_vec(&plain).unwrap().len() > 262_144);
    let report = engine
        .finish_calendar_import(&id, &later, &batch)
        .await
        .unwrap();
    assert_eq!(
        (report.updated, report.added, report.tombstoned),
        (700, 0, 0)
    );
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let after = event_heads(&engine).await;
    assert_eq!(after.len(), 1246);
    for index in 0..1246 {
        let uid = format!("work-{index}@example.com");
        assert_eq!(before[&uid].0, after[&uid].0);
        assert_eq!(
            before[&uid].1.revision + u64::from(index < 700),
            after[&uid].1.revision
        );
    }
    let batch = engine
        .stage_windowed_calendar_import(&id, &first, Utc::now())
        .await
        .unwrap();
    assert_eq!(batch.events.len(), 700);
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    assert!(
        serde_json::to_vec(&ScheduleRecord::Source(Box::new(source)))
            .unwrap()
            .len()
            < 262_144
    );
    let report = engine
        .finish_calendar_import(&id, &first, &batch)
        .await
        .unwrap();
    assert_eq!(report.updated, 700);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_old_readers_skip_changed_and_new_imported_rules() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let engine =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("client")).await;
    let now = Utc::now();
    let first = calendar(
        (now + chrono::TimeDelta::days(1))
            .date_naive()
            .and_hms_opt(9, 0, 0)
            .unwrap()
            .and_utc(),
        "Meeting",
        "RRULE:FREQ=DAILY;COUNT=3;BYHOUR=9,17\r\n",
    );
    let (url, feed, _, task) = calendar_feed_server(first.clone()).await;
    let id = engine.add_calendar_source("Work", &url).await.unwrap();
    engine.sync_calendar_source(&id).await.unwrap();
    let anchor = engine
        .read_calendar_source(&id)
        .await
        .unwrap()
        .0
        .import_anchor
        .unwrap();
    let changed = first.replace("BYHOUR=9,17", "BYHOUR=10,18");
    let start = changed.find("BEGIN:VEVENT").unwrap();
    let end = changed.find("END:VEVENT\r\n").unwrap() + "END:VEVENT\r\n".len();
    let added = changed[start..end].replace("UID:meeting", "UID:added");
    *feed.write().await = changed.replace("END:VCALENDAR", &format!("{added}END:VCALENDAR"));
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert_eq!((report.updated, report.added), (1, 1));
    let event_id = event_heads(&engine).await["meeting"].0.clone();
    let (record, head) = load_schedule_object(&engine, &event_id).await;
    let mut event = record.as_ingested().unwrap().clone();
    if let Recurrence::Imported { import, .. } = &mut event.recurrence {
        *import = anchor;
    }
    engine
        .write_schedule_record(
            &event_id,
            ScheduleRecord::Ingested(Box::new(event)),
            EnvelopePlacement::Revise(head),
        )
        .await
        .unwrap();
    assert_eq!(engine.sync_calendar_source(&id).await.unwrap().updated, 1);
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    let raw = source.active_import.as_ref().unwrap().object_id;
    assert_ne!(raw, anchor);
    let mut count = 0;
    for (_, record) in engine.local_store.schedule_records_with_ids().await {
        let Some(event) = record.as_ingested() else {
            continue;
        };
        count += 1;
        assert!(
            matches!(event.recurrence, Recurrence::Imported { import, .. } if Some(import) == event.snapshot())
        );
        assert!(event.has_valid_recurrence());
        let mut value = serde_json::to_value(event).unwrap();
        value.as_object_mut().unwrap().remove("raw_import");
        value.as_object_mut().unwrap().remove("import_fetched_at");
        let old: IngestedEvent = serde_json::from_value(value).unwrap();
        assert_eq!(old.import, Some(anchor));
        assert!(!old.belongs_to_import(anchor));
        assert!(
            matches!(old.recurrence, Recurrence::Imported { import, .. } if Some(import) == event.snapshot())
        );
    }
    assert_eq!(count, 2);
    let occurrences = engine
        .expand_schedule(
            now.to_rfc3339().as_str(),
            (now + chrono::TimeDelta::days(10)).to_rfc3339().as_str(),
            "UTC",
        )
        .await
        .unwrap();
    assert!(!occurrences.is_empty());
    assert!(occurrences.iter().all(|event| event.start.ends_with("T10:00:00Z") || event.start.ends_with("T18:00:00Z")), "{occurrences:?}");
    let alarms = engine.next_alarms(24 * 10, "UTC").await.unwrap();
    assert!(!alarms.is_empty());
    assert!(
        alarms.iter().all(|alarm| {
            let time = chrono::DateTime::from_timestamp_millis(alarm.fire_at_millis)
                .unwrap()
                .format("%H:%M:%S")
                .to_string();
            time == "09:55:00" || time == "17:55:00"
        }),
        "{alarms:?}"
    );
    task.abort();
}
