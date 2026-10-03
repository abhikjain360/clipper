use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use zeroize::Zeroizing;

use super::*;
use crate::keychain::CredentialStore;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn start_server(directory: &Path) -> (Server, String) {
    let binary = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/clipper-server");
    assert!(binary.exists(), "cargo build -p clipper-server first");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let config = directory.join("config.toml");
    std::fs::write(&config, format!(
        "[server]\ndata_dir = {:?}\naddr = {:?}\n[rate_limit]\nauth_per_client_per_minute = 200\nauth_per_username_per_minute = 200\n",
        directory.join("server").to_str().unwrap(), address.to_string()
    )).unwrap();
    let command = || {
        let mut command = Command::new(&binary);
        command
            .env("CLIPPER_SERVER_SECRET", STANDARD.encode([42_u8; 32]))
            .env_remove("CLIPPER_SERVER_SECRET_FILE")
            .env("RUST_LOG", "warn")
            .arg("--config")
            .arg(&config)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        command
    };
    assert!(command().arg("init").status().unwrap().success());
    assert!(
        command()
            .args(["add-access-key", "--access-key", "test-invite"])
            .status()
            .unwrap()
            .success()
    );
    let server = Server(command().arg("serve").spawn().unwrap());
    runtime().block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while tokio::net::TcpStream::connect(address).await.is_err() {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
    });
    (server, format!("http://{address}"))
}

async fn load(directory: &Path, store: Arc<dyn CredentialStore>) -> Arc<EngineManager> {
    EngineManager::load_with_store(
        directory.into(),
        "http://127.0.0.1:8787".into(),
        store,
        false,
    )
    .await
}

fn register(
    directory: &Path,
    url: &str,
    store: Arc<dyn CredentialStore>,
) -> clipper_client::engine::AppState {
    runtime().block_on(async {
        let manager = load(directory, store).await;
        let response = cmd_register(
            "register".into(),
            RegisterParams {
                access_key: Zeroizing::new("test-invite".into()),
                username: "alice".into(),
                passphrase: Zeroizing::new("test-passphrase".into()),
                device_name: Some("Test Mac".into()),
                server_url: Some(url.into()),
            },
            &manager,
        )
        .await;
        assert!(matches!(response, DaemonResponse::Success { .. }));
        assert!(!manager.engine().await.unwrap().clipboard_watching_enabled());
        manager.current_state().await
    })
}

#[test]
#[ignore = "build clipper-server first; starts an isolated local server"]
fn registered_session_resumes_after_daemon_restart() {
    let directory = tempfile::tempdir().unwrap();
    let (server, url) = start_server(directory.path());
    let data = directory.path().join("desktop");
    let store = Arc::new(keychain::TestStore::default());
    let before = register(&data, &url, store.clone()).session.unwrap();
    let after = runtime()
        .block_on(async {
            let restarted = load(&data, store.clone()).await;
            assert!(
                !restarted
                    .engine()
                    .await
                    .unwrap()
                    .clipboard_watching_enabled()
            );
            restarted.current_state().await
        })
        .session
        .unwrap();
    assert_eq!(after.username, before.username);
    assert_eq!(after.device_id, before.device_id);
    assert_eq!(after.device_name, "Test Mac");
    assert_eq!(after.server_url, url);
    assert!(store.load().unwrap().unwrap().session.is_some());
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("profile.json")).unwrap()).unwrap();
    assert_eq!(profile.as_object().unwrap().len(), 5);
    assert!(profile["resume"].get("session_id").is_some());
    assert_eq!(profile["resume"]["store"], "test");
    assert!(profile.get("session").is_none());
    drop(server);
    runtime().block_on(async {
        let restarted = load(&data, store.clone()).await;
        let state = restarted.current_state().await;
        assert!(state.offline);
        let session = state.session.unwrap();
        assert_eq!(session.device_id, before.device_id);
        assert_eq!(session.server_url, url);
    });
}

#[test]
#[ignore = "build clipper-server first; starts an isolated local server"]
fn refused_saved_session_keeps_the_login_profile() {
    let directory = tempfile::tempdir().unwrap();
    let (_server, url) = start_server(directory.path());
    let data = directory.path().join("desktop");
    let store = Arc::new(keychain::TestStore::default());
    register(&data, &url, store.clone());
    let mut credentials = store.load().unwrap().unwrap();
    credentials.session.as_mut().unwrap().token = Zeroizing::new("invalid-token".into());
    store.store(&credentials).unwrap();
    runtime().block_on(async {
        let manager = load(&data, store.clone()).await;
        assert!(manager.engine().await.is_none());
        let state = manager.current_state().await;
        assert!(state.session.is_none());
        let profile = state.saved_profile.unwrap();
        assert_eq!(profile.username, "alice");
        assert_eq!(profile.server_url, url);
        assert!(store.load().unwrap().is_none());
    });
}

#[test]
#[ignore = "build clipper-server first; starts an isolated local server"]
fn login_resumes_after_restart_and_logout_keeps_only_the_profile() {
    let directory = tempfile::tempdir().unwrap();
    let (_server, url) = start_server(directory.path());
    let data = directory.path().join("desktop");
    let store = Arc::new(keychain::TestStore::default());
    register(&data, &url, store.clone());
    runtime().block_on(async {
        let manager = load(&data, store.clone()).await;
        assert!(matches!(
            cmd_logout("logout".into(), true, &manager).await,
            DaemonResponse::Success { .. }
        ));
        let response = cmd_login(
            "login".into(),
            LoginParams {
                username: "alice".into(),
                passphrase: Zeroizing::new("test-passphrase".into()),
                device_name: Some("Test Mac".into()),
                server_url: Some(url.clone()),
            },
            &manager,
        )
        .await;
        assert!(matches!(response, DaemonResponse::Success { .. }));
    });
    runtime().block_on(async {
        let manager = load(&data, store.clone()).await;
        assert_eq!(
            manager.current_state().await.session.unwrap().server_url,
            url
        );
        assert!(matches!(
            cmd_logout("logout".into(), true, &manager).await,
            DaemonResponse::Success { .. }
        ));
    });
    runtime().block_on(async {
        let manager = load(&data, store.clone()).await;
        let state = manager.current_state().await;
        assert!(state.session.is_none());
        let profile = state.saved_profile.unwrap();
        assert_eq!(profile.username, "alice");
        assert_eq!(profile.server_url, url);
        assert!(store.load().unwrap().is_none());
    });
}

#[test]
#[ignore = "build clipper-server first; starts an isolated local server"]
fn a_platform_without_a_secret_store_keeps_only_the_login_profile() {
    let directory = tempfile::tempdir().unwrap();
    let (_server, url) = start_server(directory.path());
    let data = directory.path().join("desktop");
    let store = Arc::new(keychain::PlatformStore);
    assert!(!store.supports_resume());
    let state = register(&data, &url, store.clone());
    assert!(state.session.is_some());
    assert!(
        store
            .load(&keychain::load_profile(&data).unwrap().unwrap())
            .unwrap()
            .is_none()
    );
    assert!(!data.join("credentials.json").exists());
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("profile.json")).unwrap()).unwrap();
    assert_eq!(profile.as_object().unwrap().len(), 4);
    assert!(profile.get("session").is_none());
    runtime().block_on(async {
        let restarted = load(&data, store).await;
        let state = restarted.current_state().await;
        assert!(state.session.is_none());
        let profile = state.saved_profile.unwrap();
        assert_eq!(profile.username, "alice");
        assert_eq!(profile.server_url, url);
    });
}

#[test]
#[ignore = "build clipper-server first; starts an isolated local server"]
fn failed_credential_deletion_cannot_restore_a_signed_out_session() {
    use std::sync::atomic::Ordering;

    let directory = tempfile::tempdir().unwrap();
    let (server, url) = start_server(directory.path());
    let data = directory.path().join("desktop");
    let store = Arc::new(keychain::TestStore::default());
    register(&data, &url, store.clone());
    drop(server);
    store.fail_deletes.store(true, Ordering::SeqCst);
    runtime().block_on(async {
        let manager = load(&data, store.clone()).await;
        assert!(manager.current_state().await.session.is_some());
        assert!(matches!(
            cmd_logout("logout".into(), true, &manager).await,
            DaemonResponse::Success { .. }
        ));
        assert!(manager.current_state().await.session.is_none());
    });
    assert!(store.load().unwrap().is_some());
    assert!(keychain::load_profile(&data).unwrap().unwrap().signed_out);
    let reads = store.reads.load(Ordering::SeqCst);
    let deletes = store.deletes.load(Ordering::SeqCst);
    store.fail_reads.store(true, Ordering::SeqCst);
    runtime().block_on(async {
        let restarted = load(&data, store.clone()).await;
        let state = restarted.current_state().await;
        assert!(state.session.is_none());
        let profile = state.saved_profile.unwrap();
        assert_eq!(profile.username, "alice");
        assert_eq!(profile.server_url, url);
    });
    assert_eq!(store.reads.load(Ordering::SeqCst), reads);
    assert_eq!(store.deletes.load(Ordering::SeqCst), deletes + 1);
    store.fail_deletes.store(false, Ordering::SeqCst);
    runtime().block_on(async {
        let restarted = load(&data, store.clone()).await;
        assert!(restarted.current_state().await.session.is_none());
    });
    assert!(store.credentials.lock().unwrap().is_none());
    assert_eq!(store.reads.load(Ordering::SeqCst), reads);
    assert_eq!(store.deletes.load(Ordering::SeqCst), deletes + 2);
}

#[test]
#[ignore = "build clipper-server first; starts an isolated local server"]
fn logout_never_reports_success_when_the_signed_out_marker_cannot_be_saved() {
    let directory = tempfile::tempdir().unwrap();
    let (server, url) = start_server(directory.path());
    let data = directory.path().join("desktop");
    let store = Arc::new(keychain::TestStore::default());
    register(&data, &url, store.clone());
    drop(server);
    runtime().block_on(async {
        let manager = load(&data, store.clone()).await;
        std::fs::remove_file(data.join("profile.json")).unwrap();
        std::fs::create_dir(data.join("profile.json")).unwrap();
        assert!(!matches!(
            cmd_logout("logout".into(), true, &manager).await,
            DaemonResponse::Success { .. }
        ));
    });
}

#[test]
#[ignore = "build clipper-server first; starts an isolated local server"]
fn a_locked_credential_store_at_confirmation_does_not_break_resume_after_restart() {
    use std::sync::atomic::Ordering;

    let directory = tempfile::tempdir().unwrap();
    let (server, url) = start_server(directory.path());
    let data = directory.path().join("desktop");
    let store = Arc::new(keychain::TestStore::default());
    let before = register(&data, &url, store.clone()).session.unwrap();
    let mut saved = store.load().unwrap().unwrap();
    saved.session.as_mut().unwrap().last_confirmed_at -= 6 * 60 * 60 * 1000 + 60_000;
    store.store(&saved).unwrap();
    let profile_bytes = std::fs::read(data.join("profile.json")).unwrap();
    store.fail_writes.store(true, Ordering::SeqCst);
    let writes = store.writes.load(Ordering::SeqCst);
    runtime().block_on(async {
        let manager = load(&data, store.clone()).await;
        assert_eq!(
            manager.current_state().await.session.unwrap().device_id,
            before.device_id
        );
    });
    assert!(store.writes.load(Ordering::SeqCst) > writes);
    assert!(store.load().unwrap().as_ref() == Some(&saved));
    assert_eq!(
        std::fs::read(data.join("profile.json")).unwrap(),
        profile_bytes
    );
    assert!(!keychain::load_profile(&data).unwrap().unwrap().signed_out);
    drop(server);
    runtime().block_on(async {
        let restarted = load(&data, store.clone()).await;
        let state = restarted.current_state().await;
        assert!(state.offline);
        let session = state.session.unwrap();
        assert_eq!(session.device_id, before.device_id);
        assert_eq!(session.username, "alice");
        assert_eq!(session.server_url, url);
    });
}
