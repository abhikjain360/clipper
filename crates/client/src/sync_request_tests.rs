#[cfg(not(target_family = "wasm"))]
use super::tests::{activate, request, response};
use super::*;

const KEY: [u8; 32] = [7; 32];
const SIGNING_KEY: [u8; 32] = [9; 32];

pub(super) fn encrypted_item(
    kind: ObjectKind,
    id: ObjectId,
    seq: i64,
    head: Option<LocalHead>,
) -> (ObjectListItem, Vec<u8>) {
    encrypted_item_with_text(kind, id, seq, head, &format!("clipboard {seq}"))
}

fn encrypted_item_with_text(
    kind: ObjectKind,
    id: ObjectId,
    seq: i64,
    head: Option<LocalHead>,
    text: &str,
) -> (ObjectListItem, Vec<u8>) {
    let payload_id = uuid::Uuid::now_v7().into();
    let placement = head.map_or(EnvelopePlacement::Create, EnvelopePlacement::Revise);
    let aad = object_envelope_body_for_aad(
        id,
        kind,
        placement,
        uuid::Uuid::nil().into(),
        "2026-09-12T00:00:00Z".into(),
        vec![payload_id],
    );
    let (meta_nonce, meta_ciphertext, nonce, ciphertext) = match kind {
        ObjectKind::Schedule => {
            let record = ScheduleRecord::Source(Box::new(CalendarSource {
                id: SourceId::new(),
                name: format!("event {seq}"),
                kind: SourceKind::Ics {
                    url: "https://example.invalid/calendar".into(),
                },
                enabled: true,
                owner_email: None,
                alarms_on: true,
                target_device: None,
                active_import: None,
                pending_imports: Vec::new(),
                retired_imports: Vec::new(),
                retained_imports: Vec::new(),
                event_ids: Default::default(),
                import_anchor: None,
                delta_state: false,
                removing: false,
                superseded: Default::default(),
                pending_retirements: Default::default(),
            }));
            let (meta_nonce, meta_ciphertext) =
                encrypt_schedule_meta(&record.meta(), &KEY, &aad).unwrap();
            let (nonce, ciphertext) =
                encrypt_schedule_payload(&record, &KEY, &aad, payload_id).unwrap();
            (meta_nonce, meta_ciphertext, nonce, ciphertext)
        }
        ObjectKind::Clipboard => {
            let meta = ClipboardMeta {
                mime_type: "text/plain".into(),
                size: Some(text.len() as i64),
            };
            let (meta_nonce, meta_ciphertext) = encrypt_clipboard_meta(&meta, &KEY, &aad).unwrap();
            let (nonce, ciphertext) =
                encrypt_clipboard_payload(text.as_bytes(), &KEY, &aad, payload_id).unwrap();
            (meta_nonce, meta_ciphertext, nonce, ciphertext)
        }
        _ => unreachable!(),
    };
    let payload = ObjectPayloadDescriptor {
        id: payload_id,
        nonce,
        ciphertext_size: ciphertext.len() as i64,
        sha256_ciphertext: crypto::sha256(&ciphertext).to_vec(),
    };
    let body = ObjectEnvelopeBody {
        meta_nonce: meta_nonce.clone(),
        sha256_meta_ciphertext: crypto::sha256(&meta_ciphertext).to_vec(),
        payloads: vec![ObjectEnvelopePayload {
            id: payload.id,
            nonce: payload.nonce.clone(),
            ciphertext_size: payload.ciphertext_size,
            sha256_ciphertext: payload.sha256_ciphertext.clone(),
        }],
        ..aad
    };
    let item = ObjectListItem {
        id,
        kind,
        revision: body.revision,
        created_seq: seq,
        created_at: body.created_at.clone(),
        source_device_id: body.source_device_id,
        source_device_signing_public_key: Some(
            crypto::device_signing_public_key(&SIGNING_KEY).to_vec(),
        ),
        meta_nonce,
        meta_ciphertext,
        payloads: vec![payload],
        envelope: ObjectEnvelope {
            signature: crypto::sign_object_envelope_body(&SIGNING_KEY, &body).unwrap(),
            body,
        },
    };
    (item, ciphertext)
}

#[derive(Default)]
#[cfg(not(target_family = "wasm"))]
struct ServerState {
    objects: Vec<(ObjectListItem, Vec<u8>)>,
    requests: Vec<String>,
    damage_payload: bool,
}

#[cfg(not(target_family = "wasm"))]
async fn serve(listener: tokio::net::TcpListener, state: Arc<std::sync::Mutex<ServerState>>) {
    loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (headers, _) = request(&mut socket).await;
        let target = headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap();
        let url = url::Url::parse(&format!("http://localhost{target}")).unwrap();
        let body = {
            let mut state = state.lock().unwrap();
            state.requests.push(target.into());
            if url.path() == "/api/objects" {
                let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
                let limit: usize = query["limit"].parse().unwrap();
                assert_eq!(limit, 500);
                let after: i64 = query
                    .get("after_created_seq")
                    .map(|seq| seq.parse().unwrap())
                    .unwrap_or(0);
                let items: Vec<_> = state
                    .objects
                    .iter()
                    .filter(|(item, _)| item.created_seq > after)
                    .take(limit)
                    .map(|(item, _)| item.clone())
                    .collect();
                let next_after = (items.len() == limit).then(|| {
                    let item = items.last().unwrap();
                    ObjectListCursor {
                        id: item.id,
                        created_seq: item.created_seq,
                    }
                });
                postcard::to_allocvec(&ObjectListResponse { items, next_after }).unwrap()
            } else if url.path() == "/api/objects/init" {
                postcard::to_allocvec(&ObjectInitResponse::Complete { created_seq: 3 }).unwrap()
            } else {
                let id = url.path().split('/').nth(3).unwrap();
                let (item, ciphertext) = state
                    .objects
                    .iter()
                    .find(|(item, _)| item.id.to_string() == id)
                    .unwrap();
                if url.path().contains("/payloads/") {
                    let mut bytes = ciphertext.clone();
                    if state.damage_payload {
                        bytes[0] ^= 1;
                    }
                    bytes
                } else {
                    postcard::to_allocvec(item).unwrap()
                }
            }
        };
        response(&mut socket, &body).await;
    }
}

#[cfg(not(target_family = "wasm"))]
async fn snapshot(engine: &Arc<SyncEngine>, kind: ObjectKind, seq: i64) -> u64 {
    let generation = engine.local_store.start_generation().await;
    match kind {
        ObjectKind::Schedule => tokio::time::timeout(
            Duration::from_secs(15),
            engine.snapshot_schedule(generation, seq),
        )
        .await
        .expect("schedule snapshot returns")
        .unwrap(),
        ObjectKind::Clipboard => tokio::time::timeout(
            Duration::from_secs(15),
            engine.snapshot_clipboard(generation, seq),
        )
        .await
        .expect("clipboard snapshot returns")
        .unwrap(),
        _ => unreachable!(),
    }
    generation
}

#[tokio::test]
#[cfg(not(target_family = "wasm"))]
async fn reconnect_snapshots_of_held_objects_request_only_list_pages() {
    for kind in [ObjectKind::Schedule, ObjectKind::Clipboard] {
        let directory = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let engine = SyncEngine::new_with_data_dir(&url, directory.path());
        activate(&engine, "profile", KEY).await;
        let state = Arc::new(std::sync::Mutex::new(ServerState {
            objects: (1..=501)
                .map(|seq| encrypted_item(kind, uuid::Uuid::now_v7().into(), seq, None))
                .collect(),
            ..Default::default()
        }));
        let server = tokio::spawn(serve(listener, state.clone()));
        snapshot(&engine, kind, 501).await;
        assert_eq!(state.lock().unwrap().requests.len(), 503);
        state.lock().unwrap().requests.clear();
        let restarted = SyncEngine::new_with_data_dir(&url, directory.path());
        activate(&restarted, "profile", KEY).await;
        let visible = restarted
            .local_store
            .hydrate_ciphertext_cache(&KEY, RECENT_CLIPBOARD_LIMIT)
            .await
            .unwrap();
        restarted.publish_visible_state(visible).await;
        let payload_reads = restarted.local_store.payload_read_count();
        let generation = snapshot(&restarted, kind, 501).await;
        assert_eq!(restarted.local_store.payload_read_count(), payload_reads);
        assert_eq!(state.lock().unwrap().requests.len(), 2);
        let item = state.lock().unwrap().objects[0].0.clone();
        assert!(
            restarted
                .local_store
                .local_head(&item.id.to_string())
                .await
                .unwrap()
                .is_some()
        );
        state.lock().unwrap().requests.clear();
        restarted
            .materialize_object(generation, kind, item.id, item.created_seq)
            .await
            .unwrap();
        assert_eq!(state.lock().unwrap().requests.len(), 1);
        assert!(!state.lock().unwrap().requests[0].contains("/payloads/"));
        assert_eq!(restarted.local_store.payload_read_count(), payload_reads);
        server.abort();
    }
}

#[tokio::test]
#[cfg(not(target_family = "wasm"))]
async fn clipboard_reconnect_finishes_when_database_reads_wait() {
    for legacy in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(std::sync::Mutex::new(ServerState {
            objects: (1..=2)
                .map(|seq| {
                    encrypted_item(
                        ObjectKind::Clipboard,
                        uuid::Uuid::now_v7().into(),
                        seq,
                        None,
                    )
                })
                .collect(),
            ..Default::default()
        }));
        let server = tokio::spawn(serve(listener, state.clone()));
        let first = SyncEngine::new_with_data_dir(&url, directory.path());
        activate(&first, "profile", KEY).await;
        snapshot(&first, ObjectKind::Clipboard, 2).await;
        let engine = SyncEngine::new_with_data_dir(&url, directory.path());
        activate(&engine, "profile", KEY).await;
        *engine.device_signing_key.write().await = Some(Zeroizing::new(SIGNING_KEY));
        engine
            .local_store
            .hydrate_ciphertext_cache(&KEY, RECENT_CLIPBOARD_LIMIT)
            .await
            .unwrap();
        if legacy {
            engine.local_store.remove_payload_metadata_for_test().await;
        }
        state.lock().unwrap().requests.clear();
        let generation = engine.local_store.start_generation().await;
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let holding = engine.clone();
        let database = tokio::spawn(async move {
            holding
                .local_store
                .hold_database_for_test(entered, released)
                .await
        });
        ready.await.unwrap();
        let reconciling = engine.clone();
        let reconcile =
            tokio::spawn(async move { reconciling.snapshot_clipboard(generation, 2).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !engine.local_store.sync_locked_for_test() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reconciliation reaches the held database");
        release.send(()).unwrap();
        database.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), reconcile)
            .await
            .expect("reconciliation resumes after the database is released")
            .unwrap()
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            engine.expand_schedule("2026-10-10T00:00:00Z", "2026-10-11T00:00:00Z", "UTC"),
        )
        .await
        .expect("schedule expansion is not blocked by clipboard reconciliation")
        .unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            engine.add_calendar_source("Work", "https://example.invalid/calendar"),
        )
        .await
        .expect("schedule writes are not blocked by clipboard reconciliation")
        .unwrap();
        assert_eq!(
            state.lock().unwrap().requests.len(),
            if legacy { 4 } else { 2 }
        );
        let objects = state.lock().unwrap().objects.clone();
        for (item, _) in &objects {
            assert!(
                engine
                    .local_store
                    .local_head(&item.id.to_string())
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        server.abort();
    }
}

#[tokio::test]
#[cfg(not(target_family = "wasm"))]
async fn a_reconnect_with_large_held_clipboard_payloads_does_not_read_the_cached_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let engine = SyncEngine::new_with_data_dir(&url, directory.path());
    activate(&engine, "profile", KEY).await;
    let text = "x".repeat(8 * 1024 * 1024);
    let state = Arc::new(std::sync::Mutex::new(ServerState {
        objects: (1..=2)
            .map(|seq| {
                encrypted_item_with_text(
                    ObjectKind::Clipboard,
                    uuid::Uuid::now_v7().into(),
                    seq,
                    None,
                    &text,
                )
            })
            .collect(),
        ..Default::default()
    }));
    let server = tokio::spawn(serve(listener, state.clone()));
    snapshot(&engine, ObjectKind::Clipboard, 2).await;
    let restarted = SyncEngine::new_with_data_dir(&url, directory.path());
    activate(&restarted, "profile", KEY).await;
    let visible = restarted
        .local_store
        .hydrate_ciphertext_cache(&KEY, RECENT_CLIPBOARD_LIMIT)
        .await
        .unwrap();
    restarted.publish_visible_state(visible).await;
    assert_eq!(restarted.local_store.payload_read_count(), 2);
    state.lock().unwrap().requests.clear();
    snapshot(&restarted, ObjectKind::Clipboard, 2).await;
    assert_eq!(state.lock().unwrap().requests.len(), 1);
    assert_eq!(restarted.local_store.payload_read_count(), 2);
    assert_eq!(restarted.local_store.cache_check_count(), 2);
    server.abort();
}

#[tokio::test]
#[cfg(not(target_family = "wasm"))]
async fn an_unchanged_calendar_source_is_checked_once_without_reading_cached_payload_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let engine = SyncEngine::new_with_data_dir(
        &format!("http://{}", listener.local_addr().unwrap()),
        directory.path(),
    );
    activate(&engine, "profile", KEY).await;
    let id = uuid::Uuid::now_v7().into();
    let state = Arc::new(std::sync::Mutex::new(ServerState {
        objects: vec![encrypted_item(ObjectKind::Schedule, id, 1, None)],
        ..Default::default()
    }));
    let server = tokio::spawn(serve(listener, state.clone()));
    engine.read_calendar_source(&id.to_string()).await.unwrap();
    state.lock().unwrap().requests.clear();
    let checks = engine.local_store.cache_check_count();
    let reads = engine.local_store.payload_read_count();
    let (source, head) = engine.read_calendar_source(&id.to_string()).await.unwrap();
    assert_eq!(source.name, "event 1");
    assert_eq!(head.revision, 1);
    assert_eq!(state.lock().unwrap().requests.len(), 1);
    assert_eq!(engine.local_store.cache_check_count(), checks + 1);
    assert_eq!(engine.local_store.payload_read_count(), reads);
    server.abort();
}

#[tokio::test]
#[cfg(not(target_family = "wasm"))]
async fn missing_native_payloads_are_downloaded_even_with_a_present_record_and_preview() {
    for kind in [ObjectKind::Schedule, ObjectKind::Clipboard] {
        for live in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let engine = SyncEngine::new_with_data_dir(
                &format!("http://{}", listener.local_addr().unwrap()),
                directory.path(),
            );
            activate(&engine, "profile", KEY).await;
            let id = uuid::Uuid::now_v7().into();
            let state = Arc::new(std::sync::Mutex::new(ServerState {
                objects: vec![encrypted_item(kind, id, 1, None)],
                ..Default::default()
            }));
            let server = tokio::spawn(serve(listener, state.clone()));
            let generation = snapshot(&engine, kind, 1).await;
            let item = state.lock().unwrap().objects[0].0.clone();
            let head = engine.local_head(&id.to_string()).await.unwrap();
            engine
                .local_store
                .remove_payloads_for_object(&id.to_string())
                .await
                .unwrap();
            assert_eq!(engine.local_head(&id.to_string()).await.unwrap(), head);
            assert!(!engine.holds_listed_head(&item).await.unwrap());
            assert!(
                engine
                    .local_store
                    .schedule_record_at_head(&id.to_string(), head)
                    .await
                    .unwrap()
                    .is_none()
            );
            state.lock().unwrap().requests.clear();
            if live {
                engine
                    .materialize_object(generation, kind, id, 1)
                    .await
                    .unwrap();
            } else {
                snapshot(&engine, kind, 1).await;
            }
            assert_eq!(state.lock().unwrap().requests.len(), 2);
            assert_eq!(
                state
                    .lock()
                    .unwrap()
                    .requests
                    .iter()
                    .filter(|path| path.contains("/payloads/"))
                    .count(),
                1
            );
            assert!(engine.holds_listed_head(&item).await.unwrap());
            server.abort();
        }
    }
}

#[tokio::test]
#[cfg(not(target_family = "wasm"))]
async fn changed_heads_download_and_verify_the_payload_while_bad_heads_keep_the_held_copy() {
    let directory = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let engine = SyncEngine::new_with_data_dir(
        &format!("http://{}", listener.local_addr().unwrap()),
        directory.path(),
    );
    activate(&engine, "profile", KEY).await;
    let id = uuid::Uuid::now_v7().into();
    let kind = ObjectKind::Schedule;
    let state = Arc::new(std::sync::Mutex::new(ServerState {
        objects: vec![encrypted_item(kind, id, 1, None)],
        ..Default::default()
    }));
    let server = tokio::spawn(serve(listener, state.clone()));
    let generation = snapshot(&engine, kind, 1).await;
    let first = engine.local_head(&id.to_string()).await.unwrap();
    state.lock().unwrap().objects = vec![encrypted_item(kind, id, 2, Some(first))];
    state.lock().unwrap().requests.clear();
    state.lock().unwrap().damage_payload = true;
    assert!(
        engine
            .materialize_object(generation, kind, id, 2)
            .await
            .is_err()
    );
    assert_eq!(state.lock().unwrap().requests.len(), 2);
    assert_eq!(engine.local_head(&id.to_string()).await.unwrap(), first);
    state.lock().unwrap().damage_payload = false;
    state.lock().unwrap().requests.clear();
    snapshot(&engine, kind, 2).await;
    assert_eq!(state.lock().unwrap().requests.len(), 2);
    let second = engine.local_head(&id.to_string()).await.unwrap();
    assert_eq!(second.revision, 2);
    assert_eq!(
        engine
            .local_store
            .schedule_record_at_head(&id.to_string(), second)
            .await
            .unwrap()
            .unwrap()
            .as_source()
            .unwrap()
            .name,
        "event 2"
    );
    let generation = engine.local_store.start_generation().await;
    state.lock().unwrap().objects = vec![encrypted_item(kind, id, 3, Some(second))];
    state.lock().unwrap().requests.clear();
    engine
        .materialize_object(generation, kind, id, 3)
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().requests.len(), 2);
    let third = engine.local_head(&id.to_string()).await.unwrap();
    let current = state.lock().unwrap().objects[0].clone();
    for bad in [
        encrypted_item(kind, id, 2, Some(first)),
        encrypted_item(kind, id, 3, Some(second)),
        encrypted_item(kind, id, 4, Some(first)),
    ] {
        state.lock().unwrap().objects = vec![bad];
        state.lock().unwrap().requests.clear();
        engine
            .materialize_object(generation, kind, id, 4)
            .await
            .unwrap();
        assert_eq!(state.lock().unwrap().requests.len(), 1);
        assert_eq!(engine.local_head(&id.to_string()).await.unwrap(), third);
    }
    state.lock().unwrap().objects = vec![current];
    state.lock().unwrap().objects[0].0.envelope.signature[0] ^= 1;
    state.lock().unwrap().requests.clear();
    assert!(
        engine
            .materialize_object(generation, kind, id, 4)
            .await
            .is_err()
    );
    assert_eq!(state.lock().unwrap().requests.len(), 1);
    server.abort();
}
