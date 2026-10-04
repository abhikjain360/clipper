use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::*;

async fn cached_offline_session() -> (tempfile::TempDir, Arc<SyncEngine>, Arc<TcpListener>) {
    let directory = tempfile::tempdir().unwrap();
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let url = format!("http://{}", listener.local_addr().unwrap());
    let dropping = {
        let listener = Arc::clone(&listener);
        tokio::spawn(async move {
            loop {
                let _ = listener.accept().await.unwrap();
            }
        })
    };
    let engine = SyncEngine::new_with_data_dir(&url, directory.path());
    let material = saved_session(&engine, chrono::Utc::now().timestamp_millis() - 3_600_000).await;
    engine
        .resume_saved_session(material, "user", "phone", true)
        .await
        .unwrap();
    engine
        .write_app_data(
            "gym.body_weight",
            None,
            AppDataWrite::Value(json!({"time": "2026-10-07T10:00:00Z", "kg": 80.0})),
        )
        .await
        .unwrap();
    engine.stop_session_work().await;
    dropping.abort();
    let _ = dropping.await;
    (directory, engine, listener)
}

fn respond_once(
    listener: Arc<TcpListener>,
    status: u16,
    content_type: &str,
    body: Vec<u8>,
) -> tokio::task::JoinHandle<()> {
    let content_type = content_type.to_owned();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            if socket.peek(&mut [0; 1]).await.unwrap_or(0) == 0 {
                continue;
            }
            super::tests::request(&mut socket).await;
            socket.write_all(format!("HTTP/1.1 {status} Reply\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
            return;
        }
    })
}

async fn saved_session(engine: &SyncEngine, confirmed_at: i64) -> SessionResumeMaterial {
    engine
        .local_store
        .persist_device_signing_identity(
            &profile_id_from_encryption_key(&[7; 32]),
            &DeviceSigningIdentity {
                device_id: Some(uuid::Uuid::now_v7().to_string()),
                signing_secret_key: crypto::generate_device_signing_secret_key().into(),
            },
            &[8; 32],
        )
        .await
        .unwrap();
    SessionResumeMaterial {
        token: "saved-token".into(),
        data_key: Zeroizing::new([7; 32]),
        device_identity_wrapping_key: Zeroizing::new([8; 32]),
        last_confirmed_at: confirmed_at,
    }
}

#[tokio::test]
async fn offline_resume_opens_local_app_data_and_keeps_writes_across_restart() {
    let directory = tempfile::tempdir().unwrap();
    let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:1", directory.path());
    let material = saved_session(&engine, chrono::Utc::now().timestamp_millis()).await;
    engine
        .resume_saved_session(material, "user", "phone", true)
        .await
        .unwrap();
    assert!(engine.get_state().await.session.is_some());
    let id = engine
        .write_app_data(
            "gym.body_weight",
            None,
            AppDataWrite::Value(json!({"time": "2026-10-07T10:00:00Z", "kg": 80.0})),
        )
        .await
        .unwrap();
    assert_eq!(engine.app_data_status().await.unwrap().pending_changes, 1);
    assert!(matches!(
        engine
            .send_clipboard_payload(TEXT_CLIPBOARD_MIME_TYPE, b"offline text")
            .await,
        Err(ClientError::Offline)
    ));
    assert!(matches!(
        engine
            .upload_file_bytes("a.txt", Some("text/plain"), b"a")
            .await,
        Err(ClientError::Offline)
    ));
    assert!(engine.next_alarms(24, "UTC").await.is_ok());
    let material = engine.session_resume_material().await.unwrap();
    engine.stop_session_work().await;
    let restarted = SyncEngine::new_with_data_dir("http://127.0.0.1:1", directory.path());
    restarted
        .resume_saved_session(material, "user", "phone", true)
        .await
        .unwrap();
    let rows = restarted
        .query_app_data("SELECT id FROM gym.body_weight")
        .await
        .unwrap();
    assert_eq!(rows[0]["id"], json!(id));
    assert_eq!(
        restarted.app_data_status().await.unwrap().pending_changes,
        1
    );
    restarted.stop_session_work().await;
}

#[tokio::test]
async fn offline_resume_refuses_expired_missing_and_future_confirmations() {
    let now = chrono::Utc::now().timestamp_millis();
    for confirmed_at in [now - 3 * 24 * 60 * 60 * 1000 - 1, now + 60_000, 0] {
        let directory = tempfile::tempdir().unwrap();
        let engine = SyncEngine::new_with_data_dir("http://127.0.0.1:1", directory.path());
        let material = saved_session(&engine, confirmed_at).await;
        assert!(matches!(
            engine
                .resume_saved_session(material, "user", "phone", true)
                .await,
            Err(ClientError::OfflineUnlockExpired)
        ));
        assert!(engine.get_state().await.session.is_none());
        assert!(engine.session_resume_material().await.is_none());
        assert!(
            engine
                .query_app_data("SELECT id FROM gym.body_weight")
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn offline_resume_never_accepts_an_http_rejection() {
    for status in [401, 403, 500, 503] {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let read = socket.read(&mut request).await.unwrap();
            assert!(read > 0);
            socket.write_all(format!("HTTP/1.1 {status} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let engine = SyncEngine::new_with_data_dir(&url, directory.path());
        let material = saved_session(&engine, chrono::Utc::now().timestamp_millis()).await;
        assert!(
            matches!(engine.resume_saved_session(material, "user", "phone", true).await, Err(ClientError::Api { status: received, .. }) if received == status)
        );
        assert!(engine.get_state().await.session.is_none());
        assert!(engine.session_resume_material().await.is_none());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn offline_resume_unlocks_after_confirmation_times_out() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let engine = SyncEngine::new_with_data_dir(&url, directory.path());
    let material = saved_session(&engine, chrono::Utc::now().timestamp_millis()).await;
    tokio::time::timeout(
        Duration::from_secs(15),
        engine.resume_saved_session(material, "user", "phone", true),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(engine.get_state().await.session.is_some());
    assert_eq!(engine.app_data_status().await.unwrap().pending_changes, 0);
    engine.stop_session_work().await;
    server.abort();
}

#[tokio::test]
async fn unrelated_successful_responses_never_refresh_confirmation() {
    for kind in ["html", "empty", "wrong user", "wrong device"] {
        let (_directory, engine, listener) = cached_offline_session().await;
        let session = engine.get_state().await.session.unwrap();
        let saved = engine.session_resume_material().await.unwrap();
        let body = match kind {
            "html" => b"<html>Sign in to this network</html>".to_vec(),
            "empty" => b"{}".to_vec(),
            "wrong user" => serde_json::to_vec(
                &json!({"username": "someone else", "device_id": session.device_id}),
            )
            .unwrap(),
            _ => serde_json::to_vec(
                &json!({"username": session.username, "device_id": uuid::Uuid::now_v7()}),
            )
            .unwrap(),
        };
        let content_type = if kind == "html" {
            "text/html"
        } else {
            "application/json"
        };
        let server = respond_once(Arc::clone(&listener), 200, content_type, body.clone());
        let epoch = engine.history_epoch.load(Ordering::SeqCst);
        assert!(matches!(
            engine.confirm_session(epoch).await,
            Err(ClientError::UnexpectedResponse(_))
        ));
        assert_eq!(
            engine
                .session_resume_material()
                .await
                .unwrap()
                .last_confirmed_at,
            saved.last_confirmed_at
        );
        assert!(engine.get_state().await.offline);
        assert_eq!(
            engine
                .local_store
                .app_data_pending_counts()
                .await
                .unwrap()
                .pending,
            1
        );
        server.await.unwrap();
        let server = respond_once(listener, 200, content_type, body);
        assert!(matches!(
            engine
                .resume_saved_session(saved, "user", "phone", true)
                .await,
            Err(ClientError::UnexpectedResponse(_))
        ));
        assert!(engine.get_state().await.session.is_none());
        assert_eq!(engine.last_confirmed_at.load(Ordering::SeqCst), 0);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn both_auth_rejections_erase_data_on_resume_and_background_confirmation() {
    for status in [401, 403] {
        for resume in [false, true] {
            let (_directory, engine, listener) = cached_offline_session().await;
            let saved = engine.session_resume_material().await.unwrap();
            let server = respond_once(listener, status, "application/json", Vec::new());
            let rejected = if resume {
                engine
                    .resume_saved_session(saved, "user", "phone", true)
                    .await
            } else {
                engine
                    .confirm_session(engine.history_epoch.load(Ordering::SeqCst))
                    .await
            };
            assert!(
                matches!(rejected, Err(ClientError::Api { status: received, .. }) if received == status)
            );
            assert!(engine.get_state().await.session.is_none());
            assert!(engine.session_resume_material().await.is_none());
            assert!(engine.local_store.app_data_rows().await.unwrap().is_empty());
            assert_eq!(
                engine
                    .local_store
                    .app_data_pending_counts()
                    .await
                    .unwrap()
                    .pending,
                0
            );
            server.await.unwrap();
        }
    }
}

#[tokio::test]
async fn valid_confirmation_enables_http_without_a_websocket() {
    let (_directory, engine, listener) = cached_offline_session().await;
    let session = engine.get_state().await.session.unwrap();
    let saved = engine.session_resume_material().await.unwrap();
    let server = respond_once(
        listener,
        200,
        "application/json",
        serde_json::to_vec(&json!({"username": session.username, "device_id": session.device_id}))
            .unwrap(),
    );
    let epoch = engine.history_epoch.load(Ordering::SeqCst);
    engine.confirm_session(epoch).await.unwrap();
    let state = engine.get_state().await;
    assert!(!state.offline);
    assert_ne!(state.connection_status, ConnectionStatus::Connected);
    assert!(engine.credentials_for_session(epoch).await.is_ok());
    assert!(
        engine
            .session_resume_material()
            .await
            .unwrap()
            .last_confirmed_at
            > saved.last_confirmed_at
    );
    server.await.unwrap();
}

#[tokio::test]
async fn both_websocket_auth_rejections_erase_data_on_reconnect() {
    for status in [401, 403] {
        let (_directory, engine, listener) = cached_offline_session().await;
        let session = engine.get_state().await.session.unwrap();
        let server = tokio::spawn(async move {
            respond_once(
                Arc::clone(&listener),
                200,
                "application/json",
                serde_json::to_vec(
                    &json!({"username": session.username, "device_id": session.device_id}),
                )
                .unwrap(),
            )
            .await
            .unwrap();
            let (mut socket, _) = listener.accept().await.unwrap();
            let (headers, _) = super::tests::request(&mut socket).await;
            assert!(headers.starts_with("get /api/ws "));
            socket
                .write_all(
                    format!("HTTP/1.1 {status} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
        });
        let epoch = engine.history_epoch.load(Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(5), engine.ws_loop(epoch))
            .await
            .unwrap();
        assert!(engine.get_state().await.session.is_none());
        assert!(engine.session_resume_material().await.is_none());
        assert!(engine.local_store.app_data_rows().await.unwrap().is_empty());
        assert_eq!(
            engine
                .local_store
                .app_data_pending_counts()
                .await
                .unwrap()
                .pending,
            0
        );
        server.await.unwrap();
    }
}
