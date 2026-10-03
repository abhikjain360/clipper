use wasm_bindgen::prelude::*;
use wasm_bindgen_test::*;

use super::{sync_request_tests::encrypted_item, *};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen(inline_js = "
let savedFetch;
let payloadRequests = 0;
export function setReplies(list, item, payload) {
    if (!savedFetch) savedFetch = window.fetch;
    const replies = [list.slice(), item.slice(), payload.slice()];
    payloadRequests = 0;
    window.fetch = async (input) => {
        const url = new URL(input instanceof Request ? input.url : String(input), window.location.href);
        const path = url.pathname;
        const isPayload = path.includes('/payloads/');
        if (isPayload) payloadRequests++;
        const bytes = replies[isPayload ? 2 : path.endsWith('/api/objects') ? 0 : 1];
        const response = new Response(bytes, {headers: {'Content-Type': 'application/vnd.clipper.postcard', 'Content-Length': String(bytes.length)}});
        Object.defineProperty(response, 'url', {value: url.href});
        return response;
    };
}
export function downloads() { return payloadRequests; }
export function restoreFetch() { window.fetch = savedFetch; savedFetch = undefined; }
")]
extern "C" {
    #[wasm_bindgen(js_name = setReplies)]
    fn set_replies(list: &[u8], item: &[u8], payload: &[u8]);
    fn downloads() -> u32;
    #[wasm_bindgen(js_name = restoreFetch)]
    fn restore_fetch();
}

struct FetchGuard;

impl Drop for FetchGuard {
    fn drop(&mut self) {
        restore_fetch();
    }
}

fn serve(item: &ObjectListItem, payload: &[u8]) {
    set_replies(
        &postcard::to_allocvec(&ObjectListResponse {
            items: vec![item.clone()],
            next_after: None,
        })
        .unwrap(),
        &postcard::to_allocvec(item).unwrap(),
        payload,
    );
}

async fn engine(profile: &str) -> Arc<SyncEngine> {
    let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:8787", "");
    *engine.session_work.lock().unwrap() = (
        engine.history_epoch.load(Ordering::SeqCst),
        crate::session_work::SessionWork::new(),
    );
    engine.local_store.set_profile(profile.into());
    *engine.encryption_key.write().await = Some(Zeroizing::new([7; 32]));
    engine.api.restore_token("browser-test".into());
    engine.state.write().await.session = Some(AuthenticatedSession {
        username: "browser-test".into(),
        device_id: uuid::Uuid::now_v7().to_string(),
        device_name: "Test".into(),
        server_url: engine.base_url(),
    });
    engine
}

async fn snapshot(engine: &Arc<SyncEngine>, kind: ObjectKind, seq: i64) {
    let generation = engine.local_store.start_generation().await;
    match kind {
        ObjectKind::Schedule => engine.snapshot_schedule(generation, seq).await.unwrap(),
        ObjectKind::Clipboard => engine.snapshot_clipboard(generation, seq).await.unwrap(),
        _ => unreachable!(),
    }
}

#[wasm_bindgen_test]
async fn another_tabs_persisted_head_cannot_skip_this_tabs_missing_or_older_content() {
    let _fetch = FetchGuard;
    for kind in [ObjectKind::Schedule, ObjectKind::Clipboard] {
        let profile = format!("browser-sync-test-{}", uuid::Uuid::now_v7());
        let first = engine(&profile).await;
        let second = engine(&profile).await;
        let id = uuid::Uuid::now_v7().into();
        let (item, payload) = encrypted_item(kind, id, 1, None);
        serve(&item, &payload);
        snapshot(&first, kind, 1).await;
        let previous = first.local_head(&id.to_string()).await.unwrap();
        let (current, payload) = encrypted_item(kind, id, 2, Some(previous));
        serve(&current, &payload);
        snapshot(&second, kind, 2).await;
        let head = second.local_head(&id.to_string()).await.unwrap();
        assert_eq!(first.local_head(&id.to_string()).await.unwrap(), head);
        assert!(!first.holds_listed_head(&current).await.unwrap());
        assert!(
            first
                .local_store
                .schedule_record_at_head(&id.to_string(), head)
                .await
                .unwrap()
                .is_none()
        );
        if kind == ObjectKind::Schedule {
            let held = first
                .local_store
                .schedule_records_with_heads()
                .await
                .unwrap();
            assert_eq!(held[0].2, previous);
            assert_eq!(held[0].1.as_source().unwrap().name, "event 1");
        }
        serve(&current, &payload);
        snapshot(&first, kind, 2).await;
        assert_eq!(downloads(), 1);
        assert!(first.holds_listed_head(&current).await.unwrap());
        let empty = engine(&profile).await;
        serve(&current, &payload);
        snapshot(&empty, kind, 2).await;
        assert_eq!(downloads(), 1);
        if kind == ObjectKind::Clipboard {
            assert_eq!(
                first.get_state().await.clipboard_items[0].text,
                "clipboard 2"
            );
        }
    }
}

#[wasm_bindgen_test]
async fn a_calendar_source_read_downloads_the_revision_another_tab_persisted() {
    let _fetch = FetchGuard;
    let profile = format!("browser-source-test-{}", uuid::Uuid::now_v7());
    let first = engine(&profile).await;
    let second = engine(&profile).await;
    let id = uuid::Uuid::now_v7().into();
    let (item, payload) = encrypted_item(ObjectKind::Schedule, id, 1, None);
    serve(&item, &payload);
    first.read_calendar_source(&id.to_string()).await.unwrap();
    let previous = first.local_head(&id.to_string()).await.unwrap();
    let (current, payload) = encrypted_item(ObjectKind::Schedule, id, 2, Some(previous));
    serve(&current, &payload);
    second.read_calendar_source(&id.to_string()).await.unwrap();
    serve(&current, &payload);
    let (source, head) = first.read_calendar_source(&id.to_string()).await.unwrap();
    assert_eq!(downloads(), 1);
    assert_eq!(source.name, "event 2");
    assert_eq!(head.revision, 2);
    assert_eq!(
        head.parent_hash,
        crypto::object_envelope_parent_hash(&current.envelope.body).unwrap()
    );
    serve(&current, &payload);
    let checks = first.local_store.cache_check_count();
    let reads = first.local_store.payload_read_count();
    assert_eq!(
        first
            .read_calendar_source(&id.to_string())
            .await
            .unwrap()
            .0
            .name,
        "event 2"
    );
    assert_eq!(downloads(), 0);
    assert_eq!(first.local_store.cache_check_count(), checks + 1);
    assert_eq!(first.local_store.payload_read_count(), reads);
}
