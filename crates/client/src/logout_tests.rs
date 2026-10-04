use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
};

use super::{
    tests::{activate, request, response},
    *,
};

async fn engine_with_listener() -> (tempfile::TempDir, Arc<SyncEngine>, tokio::net::TcpListener) {
    let directory = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let engine = SyncEngine::new_with_data_dir(
        &format!("http://{}", listener.local_addr().unwrap()),
        directory.path(),
    );
    activate(&engine, "tester", [7; 32]).await;
    *engine.device_signing_key.write().await =
        Some(crypto::generate_device_signing_secret_key().into());
    (directory, engine, listener)
}

#[tokio::test]
async fn logout_lists_an_upload_and_cancels_it_before_clearing_the_session() {
    let (_directory, engine, listener) = engine_with_listener().await;
    let (sent, received) = oneshot::channel();
    let (revoking, revoked) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let (late, later) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut upload, _) = listener.accept().await.unwrap();
        let (_, body) = request(&mut upload).await;
        let init: ObjectInitRequest = postcard::from_bytes(&body).unwrap();
        sent.send(init.id.to_string()).unwrap();
        let (mut logout, _) = listener.accept().await.unwrap();
        let (headers, _) = request(&mut logout).await;
        assert!(headers.starts_with("post /api/auth/logout "));
        revoking.send(()).unwrap();
        released.await.unwrap();
        response(&mut logout, &[]).await;
        later.await.unwrap();
        let body =
            postcard::to_allocvec(&ObjectInitResponse::Complete { created_seq: 100 }).unwrap();
        let _ = upload
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = upload.write_all(&body).await;
    });
    let upload = tokio::spawn({
        let engine = engine.clone();
        async move { engine.upload_file_bytes("notes.txt", None, b"notes").await }
    });
    let id = received.await.unwrap();
    let epoch = engine.history_epoch.load(Ordering::SeqCst);
    let generation = engine.local_store.current_generation().await;
    let version = engine.state_version();
    let state = serde_json::to_value(engine.get_state().await).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), engine.logout(false))
            .await
            .unwrap()
            .unwrap(),
        LogoutOutcome::WorkRunning(vec![RunningWorkView {
            label: "Uploading notes.txt".into()
        }]),
    );
    assert_eq!(engine.api.token().as_deref(), Some("tester"));
    assert_eq!(
        engine.encryption_key.read().await.as_deref(),
        Some(&[7; 32])
    );
    assert_eq!(engine.history_epoch.load(Ordering::SeqCst), epoch);
    assert_eq!(engine.local_store.current_generation().await, generation);
    assert_eq!(engine.state_version(), version);
    assert_eq!(
        serde_json::to_value(engine.get_state().await).unwrap(),
        state
    );

    let work = engine.work_for_epoch(epoch).unwrap();
    let logout = tokio::spawn({
        let engine = engine.clone();
        async move { engine.logout(true).await }
    });
    tokio::time::timeout(Duration::from_secs(2), revoked)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), work.wait())
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), upload)
            .await
            .unwrap()
            .unwrap(),
        Err(ClientError::WorkCancelled)
    ));
    assert!(engine.get_state().await.session.is_some());
    assert!(engine.encryption_key.read().await.is_some());
    release.send(()).unwrap();
    assert_eq!(logout.await.unwrap().unwrap(), LogoutOutcome::SignedOut);
    late.send(()).unwrap();
    server.await.unwrap();
    assert!(engine.encryption_key.read().await.is_none());
    assert!(engine.get_state().await.files.is_empty());
    assert!(engine.local_store.local_head(&id).await.unwrap().is_none());
}

#[tokio::test]
async fn logout_lists_a_calendar_sync_and_cancels_it_without_waiting_for_the_feed() {
    use super::adversarial_history_tests::encrypted_schedule_object;

    let (_directory, engine, listener) = engine_with_listener().await;
    let id = uuid::Uuid::now_v7().to_string();
    let source = CalendarSource {
        id: SourceId::new(),
        name: "Work".into(),
        kind: SourceKind::Ics {
            url: format!("{}/work.ics", engine.base_url()),
        },
        active_import: None,
        pending_import: None,
        retired_imports: Vec::new(),
        enabled: true,
    };
    let encrypted = encrypted_schedule_object(
        &ScheduleRecord::Source(Box::new(source.clone())),
        &id,
        1,
        None,
    );
    let visible = engine
        .local_store
        .persist_local_schedule_present_encrypted(
            StoredObjectIdentity {
                object_id: &id,
                created_at: "2026-10-06T00:00:00Z",
                source_device_id: "22222222-2222-4222-8222-222222222222",
            },
            ScheduleRecord::Source(Box::new(source)),
            &encrypted,
            1,
            1,
            RECENT_CLIPBOARD_LIMIT,
        )
        .await
        .unwrap();
    engine.publish_visible_state(visible).await;
    let (sent, received) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut feed, _) = listener.accept().await.unwrap();
        let (headers, _) = request(&mut feed).await;
        assert!(headers.starts_with("get /work.ics "));
        sent.send(()).unwrap();
        let (mut logout, _) = listener.accept().await.unwrap();
        let (headers, _) = request(&mut logout).await;
        assert!(headers.starts_with("post /api/auth/logout "));
        response(&mut logout, &[]).await;
    });
    let sync = tokio::spawn({
        let engine = engine.clone();
        async move { engine.sync_calendar_source(&id).await }
    });
    received.await.unwrap();
    let state = serde_json::to_value(engine.get_state().await).unwrap();
    let generation = engine.local_store.current_generation().await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), engine.logout(false))
            .await
            .unwrap()
            .unwrap(),
        LogoutOutcome::WorkRunning(vec![RunningWorkView {
            label: "Syncing calendar Work".into()
        }]),
    );
    assert_eq!(
        serde_json::to_value(engine.get_state().await).unwrap(),
        state
    );
    assert_eq!(engine.api.token().as_deref(), Some("tester"));
    assert_eq!(engine.local_store.current_generation().await, generation);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), engine.logout(true))
            .await
            .unwrap()
            .unwrap(),
        LogoutOutcome::SignedOut,
    );
    assert!(matches!(
        sync.await.unwrap(),
        Err(ClientError::WorkCancelled)
    ));
    assert!(engine.get_state().await.session.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn logout_cancels_background_work_without_asking() {
    let (_directory, engine, listener) = engine_with_listener().await;
    let (sent, received) = oneshot::channel();
    let (dropped, drop_received) = oneshot::channel();
    struct Signal(Option<oneshot::Sender<()>>);
    impl Drop for Signal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let epoch = engine.history_epoch.load(Ordering::SeqCst);
    engine.spawn_session_work(epoch, async move {
        let _signal = Signal(Some(dropped));
        sent.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    received.await.unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        request(&mut socket).await;
        drop_received.await.unwrap();
        response(&mut socket, &[]).await;
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), engine.logout(false))
            .await
            .unwrap()
            .unwrap(),
        LogoutOutcome::SignedOut,
    );
    assert!(engine.get_state().await.session.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn logout_waits_for_a_file_write_already_running() {
    let (_directory, engine, listener) = engine_with_listener().await;
    let work = engine
        .work_for_epoch(engine.history_epoch.load(Ordering::SeqCst))
        .unwrap();
    let (sent, received) = oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let path = _directory.path().join("notes.txt");
    let writer = tokio::spawn({
        let engine = engine.clone();
        let path = path.clone();
        async move {
            engine
                .run_work(
                    Some("Downloading notes.txt".into()),
                    work.blocking(move || {
                        sent.send(()).unwrap();
                        released.recv().unwrap();
                        std::fs::write(path, b"notes").map_err(|source| ClientError::Io {
                            context: "write file",
                            source,
                        })
                    }),
                )
                .await
        }
    });
    received.await.unwrap();
    let mut logout = Box::pin(engine.logout(true));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut logout)
            .await
            .is_err()
    );
    assert!(engine.get_state().await.session.is_some());
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), writer)
            .await
            .unwrap()
            .unwrap(),
        Err(ClientError::WorkCancelled)
    ));
    assert!(!path.exists());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        request(&mut socket).await;
        assert_eq!(std::fs::read(path).unwrap(), b"notes");
        response(&mut socket, &[]).await;
    });
    release.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), logout)
            .await
            .unwrap()
            .unwrap(),
        LogoutOutcome::SignedOut
    );
    server.await.unwrap();
}

#[tokio::test]
async fn a_refused_websocket_cancels_other_work_and_ends_its_own_session() {
    let (_directory, engine, listener) = engine_with_listener().await;
    let (sent, received) = oneshot::channel();
    let upload = tokio::spawn({
        let engine = engine.clone();
        async move { engine.upload_file_bytes("notes.txt", None, b"notes").await }
    });
    let server = tokio::spawn(async move {
        let (mut upload, _) = listener.accept().await.unwrap();
        request(&mut upload).await;
        sent.send(()).unwrap();
        let (mut websocket, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 4096];
        assert!(websocket.read(&mut bytes).await.unwrap() > 0);
        websocket
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut byte = [0; 1];
        assert_eq!(upload.read(&mut byte).await.unwrap(), 0);
    });
    received.await.unwrap();
    let epoch = engine.history_epoch.load(Ordering::SeqCst);
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(2),
            engine.run_work(None, async {
                engine.ws_loop(epoch).await;
                Ok(())
            })
        )
        .await
        .unwrap()
        .unwrap(),
        (),
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), upload)
            .await
            .unwrap()
            .unwrap(),
        Err(ClientError::WorkCancelled)
    ));
    assert!(engine.get_state().await.session.is_none());
    assert!(engine.encryption_key.read().await.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn removing_this_device_cancels_work_holding_the_calendar_and_keys() {
    let (_directory, engine, listener) = engine_with_listener().await;
    let device_id = engine.current_device_id().await.unwrap();
    let (sent, received) = oneshot::channel();
    let worker = tokio::spawn({
        let engine = engine.clone();
        async move {
            engine
                .run_work(None, async {
                    let _calendar = engine.calendar_write.lock().await;
                    let _keys = engine.encryption_key.read().await;
                    sent.send(()).unwrap();
                    std::future::pending::<Result<(), ClientError>>().await
                })
                .await
        }
    });
    received.await.unwrap();
    let epoch = engine.history_epoch.load(Ordering::SeqCst);
    let history = engine.schedule_history.lock().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (headers, _) = request(&mut socket).await;
        assert!(headers.starts_with(&format!("delete /api/auth/devices/{device_id} ")));
        response(&mut socket, &postcard::to_allocvec(&OkResponse {}).unwrap()).await;
    });
    let removal = tokio::spawn({
        let engine = engine.clone();
        async move {
            let device_id = engine.current_device_id().await.unwrap();
            engine.remove_device(&device_id).await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while engine.history_epoch.load(Ordering::SeqCst) == epoch {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        engine.refresh().await,
        Err(ClientError::NotAuthenticated)
    ));
    drop(history);
    tokio::time::timeout(Duration::from_secs(2), removal)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        worker.await.unwrap(),
        Err(ClientError::WorkCancelled)
    ));
    assert!(engine.get_state().await.session.is_none());
    assert!(engine.encryption_key.read().await.is_none());
    server.await.unwrap();
}
