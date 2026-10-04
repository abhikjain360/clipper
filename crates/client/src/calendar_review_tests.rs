use super::*;

fn calendar(events: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{events}END:VCALENDAR\r\n")
}

fn meeting(start: chrono::DateTime<Utc>, rule: &str) -> String {
    format!(
        "BEGIN:VEVENT\r\nUID:meeting\r\nSUMMARY:Meeting\r\nDTSTART:{}\r\nDTEND:{}\r\n{rule}BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\n",
        start.format("%Y%m%dT%H%M%SZ"),
        (start + chrono::TimeDelta::hours(1)).format("%Y%m%dT%H%M%SZ")
    )
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_updates_events_that_leave_the_window() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let engine =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("client")).await;
    let now = Utc::now();
    let past = now - chrono::TimeDelta::days(100);
    let until = format!(
        "RRULE:FREQ=DAILY;UNTIL={}\r\n",
        (now - chrono::TimeDelta::days(20)).format("%Y%m%dT%H%M%SZ")
    );
    let original = now - chrono::TimeDelta::days(30);
    let moved = now + chrono::TimeDelta::days(1);
    let override_event = format!(
        "BEGIN:VEVENT\r\nUID:meeting\r\nRECURRENCE-ID:{}\r\nSUMMARY:Moved\r\nDTSTART:{}\r\nDTEND:{}\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\n",
        original.format("%Y%m%dT%H%M%SZ"),
        moved.format("%Y%m%dT%H%M%SZ"),
        (moved + chrono::TimeDelta::hours(1)).format("%Y%m%dT%H%M%SZ")
    );
    let cancelled = format!(
        "BEGIN:VEVENT\r\nUID:meeting\r\nRECURRENCE-ID:{}\r\nSTATUS:CANCELLED\r\nDTSTART:{}\r\nEND:VEVENT\r\n",
        original.format("%Y%m%dT%H%M%SZ"),
        original.format("%Y%m%dT%H%M%SZ")
    );
    let cases = [
        (
            calendar(&meeting(moved, "")),
            calendar(&meeting(now + chrono::TimeDelta::days(120), "")),
            now,
        ),
        (
            calendar(&meeting(past, "RRULE:FREQ=DAILY\r\n")),
            calendar(&meeting(past, &until)),
            now,
        ),
        (
            calendar(&meeting(past, "RRULE:FREQ=DAILY\r\n")),
            calendar(&meeting(
                past,
                &format!(
                    "RRULE:FREQ=DAILY;UNTIL={}\r\n",
                    (now + chrono::TimeDelta::days(5)).format("%Y%m%dT%H%M%SZ")
                ),
            )),
            now + chrono::TimeDelta::days(100),
        ),
        (
            calendar(&(meeting(original, "RRULE:FREQ=DAILY;COUNT=2\r\n") + &override_event)),
            calendar(&(meeting(original, "RRULE:FREQ=DAILY;COUNT=2\r\n") + &cancelled)),
            now,
        ),
    ];
    for (index, (old, new, refresh_time)) in cases.into_iter().enumerate() {
        let (url, feed, _, task) = calendar_feed_server(old).await;
        let id = engine
            .add_calendar_source(&format!("Case {index}"), &url)
            .await
            .unwrap();
        let first = engine.sync_calendar_source(&id).await.unwrap();
        assert_eq!(first.added, 1, "case {index}");
        let source = engine.read_calendar_source(&id).await.unwrap().0;
        let object_id = source.active_import.unwrap().events[0].to_string();
        let item_id = load_schedule_object(&engine, &object_id)
            .await
            .0
            .as_ingested()
            .unwrap()
            .id
            .to_string();
        assert!(
            engine
                .next_alarms(24 * 90, "UTC")
                .await
                .unwrap()
                .iter()
                .any(|alarm| alarm.item_id == item_id),
            "case {index}"
        );
        let before = engine.api.get_object_head(&object_id).await.unwrap();
        *feed.write().await = new;
        let report = engine
            .sync_calendar_source_in_window(&id, refresh_time)
            .await
            .unwrap();
        assert_eq!((report.updated, report.tombstoned), (1, 0), "case {index}");
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let after = engine.api.get_object_head(&object_id).await.unwrap();
        assert_eq!(after.revision, before.revision + 1);
        let source = engine.read_calendar_source(&id).await.unwrap().0;
        let occurrences = engine
            .expand_schedule(
                (refresh_time - chrono::TimeDelta::days(14))
                    .to_rfc3339()
                    .as_str(),
                (refresh_time + chrono::TimeDelta::days(90))
                    .to_rfc3339()
                    .as_str(),
                "UTC",
            )
            .await
            .unwrap();
        assert!(
            !occurrences
                .iter()
                .any(|event| event.source.as_deref() == Some(source.name.as_str())),
            "case {index}: {occurrences:?}"
        );
        if index != 2 {
            assert!(
                !engine
                    .next_alarms(24 * 90, "UTC")
                    .await
                    .unwrap()
                    .iter()
                    .any(|alarm| alarm.item_id == item_id),
                "case {index}"
            );
        }
        task.abort();
    }
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_refresh_recovers_when_original_recurrence_snapshot_was_deleted() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let engine =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("client")).await;
    let now = Utc::now();
    let initial = calendar(&meeting(
        now - chrono::TimeDelta::days(100),
        "RRULE:FREQ=DAILY;BYHOUR=9,17\r\n",
    ));
    let (url, feed, _, task) = calendar_feed_server(initial.clone()).await;
    let id = engine.add_calendar_source("Work", &url).await.unwrap();
    engine.sync_calendar_source(&id).await.unwrap();
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    let batch = source.active_import.unwrap();
    let event_id = batch.events[0].to_string();
    engine
        .delete_file(&batch.object_id.to_string())
        .await
        .unwrap();
    *feed.write().await = initial.replace("SUMMARY:Meeting", "SUMMARY:Changed");
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert_eq!(report.updated, 1);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let source = engine.read_calendar_source(&id).await.unwrap().0;
    let raw = source.active_import.unwrap().object_id;
    let stored = held_event(&engine, &event_id).await.unwrap().unwrap();
    assert_eq!(stored.as_ingested().unwrap().snapshot(), Some(raw));
    assert_eq!(stored.as_ingested().unwrap().title, "Changed");
    engine.delete_file(&raw.to_string()).await.unwrap();
    *feed.write().await = calendar("");
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert_eq!(report.tombstoned, 1);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(held_event(&engine, &event_id).await.unwrap().is_none());
    task.abort();
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OldImport {
    object_id: ObjectId,
    fetched_at: chrono::DateTime<Utc>,
    events: Vec<ObjectId>,
    #[serde(default)]
    content_hash: Vec<u8>,
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_explicit_removal_keeps_all_ownership_after_an_old_save() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let engine =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("client")).await;
    let now = Utc::now();
    let text = calendar(
        &(meeting(now + chrono::TimeDelta::days(1), "")
            + &meeting(now + chrono::TimeDelta::days(2), "").replace("UID:meeting", "UID:second")),
    );
    let (url, feed, _, task) = calendar_feed_server(text.clone()).await;
    let id = engine.add_calendar_source("Work", &url).await.unwrap();
    engine.sync_calendar_source(&id).await.unwrap();
    let anchor = engine
        .read_calendar_source(&id)
        .await
        .unwrap()
        .0
        .import_anchor
        .unwrap();
    *feed.write().await = text.replace("SUMMARY:Meeting", "SUMMARY:Changed");
    engine.sync_calendar_source(&id).await.unwrap();
    let events = event_heads(&engine).await;
    assert_eq!(events.len(), 2);
    let (source, head) = engine.read_calendar_source(&id).await.unwrap();
    let raw = source.active_import.as_ref().unwrap().object_id;
    let mut old: OldSource = serde_json::from_value(serde_json::to_value(source).unwrap()).unwrap();
    old.active_import.as_mut().unwrap().events.clear();
    let stripped: CalendarSource =
        serde_json::from_value(serde_json::to_value(old).unwrap()).unwrap();
    engine
        .write_schedule_record(
            &id,
            ScheduleRecord::Source(Box::new(stripped)),
            EnvelopePlacement::Revise(head),
        )
        .await
        .unwrap();
    engine.delete_schedule_object(&id).await.unwrap();
    for object_id in [anchor.to_string(), raw.to_string()]
        .into_iter()
        .chain(events.into_values().map(|(id, _)| id))
    {
        assert!(
            matches!(
                engine.api.get_object_revision(&object_id, 1).await,
                Err(ClientError::Api { status: 404, .. })
            ),
            "{object_id}"
        );
    }
    task.abort();
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OldRetired {
    object_id: ObjectId,
    events: Vec<ObjectId>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OldSource {
    id: SourceId,
    name: String,
    kind: clipper_schedule::SourceKind,
    enabled: bool,
    owner_email: Option<String>,
    alarms_on: bool,
    target_device: Option<DeviceId>,
    active_import: Option<OldImport>,
    pending_imports: Vec<OldImport>,
    retired_imports: Vec<OldRetired>,
}

#[tokio::test]
#[ignore = "build clipper-server first; starts an isolated local server"]
async fn live_calendar_old_manifest_roundtrip_keeps_all_history_and_snapshots() {
    crate::ensure_crypto_provider();
    let temp = tempfile::tempdir().unwrap();
    let (_server, address) = start_server(temp.path()).await;
    let first =
        register_proxy_engine(&format!("http://{address}"), &temp.path().join("first")).await;
    first.stop_session_work().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let proxy = tokio::spawn(count_calendar_requests(listener, address, requests.clone()));
    let engine = copy_session(&first, &proxy_url, &temp.path().join("counted")).await;
    let now = Utc::now();
    let original = feed(now).replace(
        "SUMMARY:Meeting 2\r\n",
        "SUMMARY:Meeting 2\r\nRRULE:FREQ=DAILY;COUNT=3;BYHOUR=9,17\r\n",
    );
    let (url, feed, _, task) = calendar_feed_server(original.clone()).await;
    let id = engine.add_calendar_source("Work", &url).await.unwrap();
    let legacy = engine
        .stage_calendar_import(&id, &original, now)
        .await
        .unwrap();
    engine
        .finish_calendar_import(&id, &original, &legacy)
        .await
        .unwrap();
    *feed.write().await = original.replace("SUMMARY:Meeting 2\r\n", "SUMMARY:Changed\r\n");
    engine.sync_calendar_source(&id).await.unwrap();
    let heads = event_heads(&engine).await;
    assert_eq!(heads.len(), 1246);
    let (source, head) = engine.read_calendar_source(&id).await.unwrap();
    let latest = source.active_import.as_ref().unwrap().object_id;
    let completed = source.active_import.as_ref().unwrap().clone();
    let mut old: OldSource =
        serde_json::from_value(serde_json::to_value(&source).unwrap()).unwrap();
    let active = old.active_import.as_ref().unwrap();
    assert_eq!(active.object_id, legacy.object_id);
    assert_eq!(active.events.len(), 1246);
    assert!(old.pending_imports.is_empty());
    assert!(old.retired_imports.is_empty());
    for (event_id, record) in engine.local_store.schedule_records_with_ids().await {
        if let Some(event) = record.as_ingested() {
            assert_eq!(event.import, Some(active.object_id));
            assert!(event.has_valid_recurrence());
            assert!(active.events.iter().any(|id| id.to_string() == event_id));
        }
    }
    old.retired_imports.push(OldRetired {
        object_id: latest,
        events: vec![heads["meeting-2"].0.parse().unwrap()],
    });
    let stripped: CalendarSource =
        serde_json::from_value(serde_json::to_value(old).unwrap()).unwrap();
    assert!(!stripped.delta_state);
    assert!(stripped.superseded.is_empty());
    assert!(!stripped.can_cleanup(&stripped.retired_imports[0]));
    engine
        .write_schedule_record(
            &id,
            ScheduleRecord::Source(Box::new(stripped)),
            EnvelopePlacement::Revise(head),
        )
        .await
        .unwrap();
    requests.lock().unwrap().clear();
    let report = engine
        .finish_calendar_import(&id, &feed.read().await.clone(), &completed)
        .await
        .unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(event_heads(&engine).await, heads);
    for raw in [legacy.object_id, latest] {
        assert!(engine.api.get_object(&raw.to_string()).await.is_ok());
    }
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.starts_with("DELETE "))
    );
    *feed.write().await = original.replace("SUMMARY:Meeting 2\r\n", "SUMMARY:Changed again\r\n");
    let report = engine.sync_calendar_source(&id).await.unwrap();
    assert_eq!(report.updated, 1);
    let after = event_heads(&engine).await;
    assert_eq!(after.len(), 1246);
    for (uid, (object_id, head)) in &heads {
        assert_eq!(&after[uid].0, object_id);
        assert_eq!(
            after[uid].1.revision,
            head.revision + u64::from(uid == "meeting-2")
        );
    }
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.starts_with("DELETE "))
    );
    proxy.abort();
    task.abort();
}
