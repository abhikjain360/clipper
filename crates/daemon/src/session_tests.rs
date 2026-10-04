use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use zeroize::Zeroizing;

use super::*;

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

fn register(directory: &Path, url: &str) -> clipper_client::engine::AppState {
    runtime().block_on(async {
        let manager = EngineManager::load(directory.into(), "http://127.0.0.1:8787".into()).await;
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
        manager.current_state().await
    })
}

#[test]
#[ignore = "build clipper-server first; starts an isolated local server"]
fn registered_session_resumes_after_daemon_restart() {
    let directory = tempfile::tempdir().unwrap();
    let (server, url) = start_server(directory.path());
    let data = directory.path().join("desktop");
    let before = register(&data, &url).session.unwrap();
    let after = runtime()
        .block_on(async {
            let restarted = EngineManager::load(data.clone(), "http://127.0.0.1:8787".into()).await;
            restarted.current_state().await
        })
        .session
        .unwrap();
    assert_eq!(after.username, before.username);
    assert_eq!(after.device_id, before.device_id);
    assert_eq!(after.device_name, "Test Mac");
    assert_eq!(after.server_url, url);
    assert!(
        keychain::load_credentials(&data)
            .unwrap()
            .unwrap()
            .session
            .is_some()
    );
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("profile.json")).unwrap()).unwrap();
    assert_eq!(profile.as_object().unwrap().len(), 3);
    assert!(profile.get("session").is_none());
    drop(server);
    runtime().block_on(async {
        let restarted = EngineManager::load(data.clone(), "http://127.0.0.1:8787".into()).await;
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
    register(&data, &url);
    let mut credentials = keychain::load_credentials(&data).unwrap().unwrap();
    credentials.session.as_mut().unwrap().token = Zeroizing::new("invalid-token".into());
    keychain::store_credentials(&data, &credentials).unwrap();
    runtime().block_on(async {
        let manager = EngineManager::load(data.clone(), "http://127.0.0.1:8787".into()).await;
        assert!(manager.engine().await.is_none());
        let state = manager.current_state().await;
        assert!(state.session.is_none());
        let profile = state.saved_profile.unwrap();
        assert_eq!(profile.username, "alice");
        assert_eq!(profile.server_url, url);
        assert!(keychain::load_credentials(&data).unwrap().is_none());
    });
}

#[test]
#[ignore = "build clipper-server first; starts an isolated local server"]
fn login_resumes_after_restart_and_logout_keeps_only_the_profile() {
    let directory = tempfile::tempdir().unwrap();
    let (_server, url) = start_server(directory.path());
    let data = directory.path().join("desktop");
    register(&data, &url);
    runtime().block_on(async {
        let manager = EngineManager::load(data.clone(), "http://127.0.0.1:8787".into()).await;
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
        let manager = EngineManager::load(data.clone(), "http://127.0.0.1:8787".into()).await;
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
        let manager = EngineManager::load(data.clone(), "http://127.0.0.1:8787".into()).await;
        let state = manager.current_state().await;
        assert!(state.session.is_none());
        let profile = state.saved_profile.unwrap();
        assert_eq!(profile.username, "alice");
        assert_eq!(profile.server_url, url);
        assert!(keychain::load_credentials(&data).unwrap().is_none());
    });
}
