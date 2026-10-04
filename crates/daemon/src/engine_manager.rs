use std::{path::PathBuf, sync::Arc};

use clipper_client::{
    api_client::ClientError,
    engine::{AppState, SavedProfile, SyncEngine},
};
use tokio::{
    sync::{Mutex, RwLock, watch},
    task::JoinHandle,
};

use crate::keychain::{self, Credentials};

pub struct EngineManager {
    data_dir: PathBuf,
    default_server_url: String,
    stored_creds: RwLock<Option<Credentials>>,
    slot: RwLock<Option<Arc<SyncEngine>>>,
    // Bumped whenever the engine is installed or cleared so the state watcher can
    // (re)subscribe as it comes and goes across login/logout.
    ready: watch::Sender<u64>,
    calendar_refresh: Mutex<Option<JoinHandle<()>>>,
    pub session_change: Mutex<()>,
}

impl EngineManager {
    pub async fn load(data_dir: PathBuf, default_server_url: String) -> Arc<Self> {
        let profile = match keychain::load_profile(&data_dir) {
            Ok(profile) => profile.map(Credentials::from),
            Err(error) => {
                tracing::warn!(%error, "Failed to load remembered login profile");
                None
            }
        };
        let credentials = match keychain::load_credentials(&data_dir) {
            Ok(credentials) => credentials.or(profile),
            Err(error) => {
                tracing::warn!(%error, "Failed to read saved session; showing login with the remembered server URL and username. Ad-hoc macOS code signature changes can deny keychain access");
                profile
            }
        };
        if let Some(credentials) = credentials.as_ref()
            && let Err(error) = keychain::store_profile(&data_dir, &credentials.profile())
        {
            tracing::warn!(%error, "Failed to store remembered login profile");
        }
        let session = credentials
            .as_ref()
            .and_then(|credentials| credentials.session.clone());
        let manager = Self::new(data_dir, default_server_url, credentials);
        if let Some(session) = session {
            let result = async {
                let profile = manager.saved_profile().await.unwrap();
                let (engine, _) = manager.get_or_build(None).await?;
                engine
                    .resume_saved_session(
                        session.material(),
                        &profile.username,
                        &profile.device_name,
                        true,
                    )
                    .await?;
                manager.save_session(&engine).await;
                manager.start_calendar_refresh(engine).await;
                tracing::info!("Resumed saved desktop session");
                Ok::<(), ClientError>(())
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(%error, "Failed to resume saved session; showing login with the remembered server URL and username");
                if matches!(
                    error,
                    ClientError::Api {
                        status: 401 | 403,
                        ..
                    } | ClientError::NoResumableDeviceIdentity
                ) {
                    manager.clear_credentials().await;
                }
                manager.discard_engine().await;
            }
        }
        manager
    }

    pub fn new(
        data_dir: PathBuf,
        default_server_url: String,
        stored_creds: Option<Credentials>,
    ) -> Arc<Self> {
        let (ready, _) = watch::channel(0);
        Arc::new(Self {
            data_dir,
            default_server_url,
            stored_creds: RwLock::new(stored_creds),
            slot: RwLock::new(None),
            ready,
            calendar_refresh: Mutex::new(None),
            session_change: Mutex::new(()),
        })
    }

    /// The engine, if it has been built (the user has logged in or registered at
    /// least once this daemon lifetime).
    pub async fn engine(&self) -> Option<Arc<SyncEngine>> {
        self.slot.read().await.clone()
    }

    /// Profile used to prefill the login form before any engine exists.
    pub async fn saved_profile(&self) -> Option<SavedProfile> {
        self.stored_creds
            .read()
            .await
            .as_ref()
            .map(Credentials::profile)
    }

    /// Snapshot for `get-state`: the live engine state once built, otherwise a
    /// logged-out state carrying the saved-profile prefill.
    pub async fn current_state(&self) -> AppState {
        match self.engine().await {
            Some(engine) => engine.get_state().await,
            None => AppState {
                saved_profile: self.saved_profile().await,
                ..AppState::default()
            },
        }
    }

    /// Return the engine, building it bound to `requested_url` the first time.
    /// Once an engine exists, `requested_url` must match its base URL.
    ///
    /// The returned flag is `true` when this call freshly built the engine and
    /// `false` when it reused an existing one. Callers that then authenticate use
    /// it to decide whether a failure should [`discard_engine`](Self::discard_engine):
    /// a freshly built engine that fails login must not pin its URL for the rest
    /// of the daemon's lifetime.
    pub async fn get_or_build(
        &self,
        requested_url: Option<&str>,
    ) -> Result<(Arc<SyncEngine>, bool), ClientError> {
        if let Some(engine) = self.engine().await {
            ensure_requested_base_url(&engine, requested_url)?;
            return Ok((engine, false));
        }

        let mut slot = self.slot.write().await;
        // Another task may have built it while we waited for the write lock.
        if let Some(engine) = slot.as_ref() {
            ensure_requested_base_url(engine, requested_url)?;
            return Ok((Arc::clone(engine), false));
        }

        // Read the stored profile once: it provides the URL fallback and the
        // login-form prefill for the newly built engine.
        let stored = self.stored_creds.read().await.clone();
        // Prefer the URL this request carries; fall back to the stored profile's
        // server, then the built-in default.
        let url = requested_url
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_string)
            .or_else(|| stored.as_ref().map(|creds| creds.server_url.clone()))
            .unwrap_or_else(|| self.default_server_url.clone());

        let engine = SyncEngine::try_new_with_data_dir(&url, self.data_dir.join("client"))?;
        if let Some(creds) = stored.as_ref() {
            engine
                .set_saved_profile(
                    Some(creds.username.clone()),
                    Some(creds.device_name.clone()),
                )
                .await;
        }
        *slot = Some(Arc::clone(&engine));
        drop(slot);
        // Wake the state watcher now that there is an engine to subscribe to.
        self.bump_ready();
        Ok((engine, true))
    }

    /// Drop a freshly built engine whose authentication failed, releasing the
    /// server URL it was bound to so the next login/register can target a
    /// different server (e.g. the user corrects a mistyped URL and retries).
    ///
    /// Unlike [`clear`](Self::clear), this keeps the stored profile so the
    /// login form stays prefilled: a failed login must not erase the saved
    /// username/server.
    pub async fn discard_engine(&self) {
        *self.slot.write().await = None;
        // Wake the state watcher so it drops its subscription to the gone engine.
        self.bump_ready();
    }

    pub async fn clear(&self) {
        self.stop_calendar_refresh().await;
        self.clear_credentials().await;
        *self.slot.write().await = None;
        self.bump_ready();
    }

    async fn clear_credentials(&self) {
        if let Err(error) = keychain::clear_credentials(&self.data_dir) {
            tracing::warn!(%error, "Failed to clear saved session");
        }
        if let Some(credentials) = self.stored_creds.write().await.as_mut() {
            credentials.session = None;
        }
    }

    pub async fn save_session(&self, engine: &SyncEngine) {
        let state = engine.get_state().await;
        let Some(session) = state.session else {
            return;
        };
        let Some(material) = engine.session_resume_material().await else {
            return;
        };
        let credentials = Credentials {
            username: session.username,
            device_name: session.device_name,
            server_url: session.server_url,
            session: Some(material.into()),
        };
        let mut stored = self.stored_creds.write().await;
        if stored.as_ref() == Some(&credentials) {
            return;
        }
        if let Err(error) = keychain::store_profile(&self.data_dir, &credentials.profile()) {
            tracing::warn!(%error, "Failed to store remembered login profile");
        }
        if let Err(error) = keychain::store_credentials(&self.data_dir, &credentials) {
            tracing::warn!(%error, "Failed to save session credentials; login will be required after restart");
            *stored = Some(Credentials::from(credentials.profile()));
        } else {
            *stored = Some(credentials);
        }
    }

    /// Watch handle for engine install/clear events so the state watcher can
    /// re-subscribe whenever the engine is (re)built or torn down.
    pub fn subscribe_ready(&self) -> watch::Receiver<u64> {
        self.ready.subscribe()
    }

    pub async fn start_calendar_refresh(&self, engine: Arc<SyncEngine>) {
        if engine.get_state().await.session.is_none() {
            return;
        }
        let mut task = self.calendar_refresh.lock().await;
        if task.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        *task = Some(tokio::spawn(crate::calendar_refresh::run(engine)));
    }

    pub async fn stop_calendar_refresh(&self) {
        if let Some(task) = self.calendar_refresh.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
    }

    pub async fn update_calendar_refresh(&self, engine: &Arc<SyncEngine>, state: &AppState) {
        let Ok(_change) = self.session_change.try_lock() else {
            return;
        };
        if state.session.is_some() {
            self.save_session(engine).await;
            self.start_calendar_refresh(engine.clone()).await;
        } else {
            self.stop_calendar_refresh().await;
            if self
                .stored_creds
                .read()
                .await
                .as_ref()
                .is_some_and(|credentials| credentials.session.is_some())
            {
                self.clear_credentials().await;
            }
        }
    }

    fn bump_ready(&self) {
        self.ready
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

/// Guard that the engine's bound URL matches a later per-request URL. An empty or
/// absent request URL imposes no constraint.
fn ensure_requested_base_url(
    engine: &SyncEngine,
    requested: Option<&str>,
) -> Result<(), ClientError> {
    let Some(requested) = requested.map(str::trim).filter(|url| !url.is_empty()) else {
        return Ok(());
    };
    let configured = engine.base_url();
    if normalize_server_url(requested) == normalize_server_url(&configured) {
        return Ok(());
    }
    Err(ClientError::InvalidServerUrl(format!(
        "Server URL is fixed for this session: configured {configured}, requested {requested}"
    )))
}

fn normalize_server_url(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use tracing::instrument::WithSubscriber;

    use super::*;

    struct Log(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Log {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn manager(name: &str, stored: Option<Credentials>) -> Arc<EngineManager> {
        let dir = std::env::temp_dir().join(format!("clipper-engine-manager-test-{name}"));
        EngineManager::new(dir, "http://127.0.0.1:8787".to_string(), stored)
    }

    fn creds(server_url: &str, username: &str) -> Credentials {
        Credentials {
            device_name: "Test Device".to_string(),
            server_url: server_url.to_string(),
            username: username.to_string(),
            session: None,
        }
    }

    #[tokio::test]
    async fn restart_remembers_server_and_username_without_session_credentials() {
        let directory = tempfile::tempdir().unwrap();
        keychain::store_profile(
            directory.path(),
            &creds("https://stored.example", "alice").profile(),
        )
        .unwrap();
        let manager =
            EngineManager::load(directory.path().into(), "http://127.0.0.1:8787".into()).await;
        let state = manager.current_state().await;
        assert!(state.session.is_none());
        let profile = state.saved_profile.unwrap();
        assert_eq!(profile.server_url, "https://stored.example");
        assert_eq!(profile.username, "alice");
        let (engine, _) = manager.get_or_build(None).await.unwrap();
        assert_eq!(engine.base_url(), profile.server_url);
        assert_eq!(
            engine.get_state().await.saved_profile.unwrap().server_url,
            profile.server_url
        );
    }

    #[tokio::test]
    async fn credential_read_failure_keeps_login_prefill() {
        let directory = tempfile::tempdir().unwrap();
        keychain::store_profile(
            directory.path(),
            &creds("https://stored.example", "alice").profile(),
        )
        .unwrap();
        std::fs::create_dir(directory.path().join("credentials.json")).unwrap();
        assert!(keychain::load_credentials(directory.path()).is_err());
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = Arc::clone(&log);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || Log(Arc::clone(&output)))
            .finish();
        let manager = EngineManager::load(directory.path().into(), "http://127.0.0.1:8787".into())
            .with_subscriber(subscriber)
            .await;
        assert!(manager.engine().await.is_none());
        let state = manager.current_state().await;
        assert!(state.session.is_none());
        let profile = state.saved_profile.unwrap();
        assert_eq!(profile.server_url, "https://stored.example");
        assert_eq!(profile.username, "alice");
        assert!(directory.path().join("credentials.json").is_dir());
        let log = String::from_utf8(log.lock().unwrap().clone()).unwrap();
        assert!(log.contains("Failed to read saved session"));
        assert!(log.contains("showing login with the remembered server URL and username"));
        assert!(log.contains("code signature changes can deny keychain access"));
    }

    #[tokio::test]
    async fn missing_device_identity_falls_back_to_remembered_login() {
        let directory = tempfile::tempdir().unwrap();
        let mut credentials = creds("https://stored.example", "alice");
        credentials.session = Some(keychain::StoredSession {
            token: zeroize::Zeroizing::new("saved-token".into()),
            data_key: zeroize::Zeroizing::new([7; 32]),
            device_identity_wrapping_key: zeroize::Zeroizing::new([8; 32]),
            last_confirmed_at: chrono::Utc::now().timestamp_millis(),
        });
        keychain::store_credentials(directory.path(), &credentials).unwrap();
        let manager =
            EngineManager::load(directory.path().into(), "http://127.0.0.1:8787".into()).await;
        assert!(manager.engine().await.is_none());
        let profile = manager.current_state().await.saved_profile.unwrap();
        assert_eq!(profile.server_url, "https://stored.example");
        assert_eq!(profile.username, "alice");
        assert!(
            keychain::load_credentials(directory.path())
                .unwrap()
                .is_none()
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.path().join("profile.json")).unwrap())
                .unwrap();
        assert_eq!(saved.as_object().unwrap().len(), 3);
        assert!(saved.get("session").is_none());
    }

    #[tokio::test]
    async fn builds_engine_with_requested_url_on_first_use() {
        let mgr = manager("first-use", None);
        assert!(mgr.engine().await.is_none());

        let (engine, freshly_built) = mgr
            .get_or_build(Some("https://a.example"))
            .await
            .expect("build engine");
        assert!(freshly_built, "first use builds the engine");
        assert_eq!(engine.base_url(), "https://a.example");
        assert!(mgr.engine().await.is_some());

        // Reusing the existing engine reports it was not freshly built.
        let (_, freshly_built) = mgr
            .get_or_build(Some("https://a.example"))
            .await
            .expect("reuse engine");
        assert!(!freshly_built, "second use reuses the engine");
    }

    #[tokio::test]
    async fn url_is_fixed_until_cleared_then_switchable() {
        let mgr = manager("switch", None);
        mgr.get_or_build(Some("https://a.example"))
            .await
            .expect("build a");

        // A different URL is rejected while the engine is alive.
        let rejected = mgr.get_or_build(Some("https://b.example")).await;
        assert!(matches!(rejected, Err(ClientError::InvalidServerUrl(_))));

        // After clear (logout) the next build may target a different server.
        mgr.clear().await;
        assert!(mgr.engine().await.is_none());
        let (engine, _) = mgr
            .get_or_build(Some("https://b.example"))
            .await
            .expect("rebuild b");
        assert_eq!(engine.base_url(), "https://b.example");
    }

    #[tokio::test]
    async fn discard_engine_releases_url_but_keeps_profile() {
        // A first login against the wrong server pins that URL...
        let mgr = manager("discard", Some(creds("https://stored.example", "alice")));
        let (engine, freshly_built) = mgr
            .get_or_build(Some("https://wrong.example"))
            .await
            .expect("build wrong");
        assert!(freshly_built);
        assert_eq!(engine.base_url(), "https://wrong.example");
        assert!(matches!(
            mgr.get_or_build(Some("https://right.example")).await,
            Err(ClientError::InvalidServerUrl(_))
        ));

        // ...but discarding the failed engine releases the pin while keeping the
        // saved profile prefill (unlike `clear`), so the user can correct the URL.
        mgr.discard_engine().await;
        assert!(mgr.engine().await.is_none());
        assert_eq!(
            mgr.saved_profile().await.expect("profile kept").username,
            "alice"
        );
        let (engine, freshly_built) = mgr
            .get_or_build(Some("https://right.example"))
            .await
            .expect("rebuild right");
        assert!(freshly_built);
        assert_eq!(engine.base_url(), "https://right.example");
    }

    #[tokio::test]
    async fn falls_back_to_stored_url_when_request_omits_it() {
        let mgr = manager("fallback", Some(creds("https://stored.example", "alice")));
        let (engine, _) = mgr.get_or_build(None).await.expect("build from stored");
        assert_eq!(engine.base_url(), "https://stored.example");
    }

    #[tokio::test]
    async fn current_state_carries_saved_profile_before_login() {
        let mgr = manager("prefill", Some(creds("https://stored.example", "bob")));
        let state = mgr.current_state().await;
        let profile = state.saved_profile.expect("prefill present");
        assert_eq!(profile.username, "bob");
        assert_eq!(profile.device_name, "Test Device");
        assert_eq!(profile.server_url, "https://stored.example");

        mgr.clear().await;
        assert_eq!(
            mgr.current_state().await.saved_profile.unwrap().server_url,
            "https://stored.example"
        );
    }

    #[tokio::test]
    async fn stopping_calendar_refresh_drops_running_work_without_waiting_for_it() {
        struct Finish(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for Finish {
            fn drop(&mut self) {
                let _ = self.0.take().unwrap().send(());
            }
        }

        let mgr = manager("stop-calendar-refresh", None);
        let (started, ready) = tokio::sync::oneshot::channel();
        let (finished, dropped) = tokio::sync::oneshot::channel();
        *mgr.calendar_refresh.lock().await = Some(tokio::spawn(async move {
            let _finish = Finish(Some(finished));
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        }));
        ready.await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            mgr.stop_calendar_refresh(),
        )
        .await
        .unwrap();
        dropped.await.unwrap();
        assert!(mgr.calendar_refresh.lock().await.is_none());
    }
}
