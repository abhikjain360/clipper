//! Lazily-initialized [`SyncEngine`] holder.
//!
//! The daemon does not know which server to talk to until the user logs in or
//! registers, so the engine is built on the first login/register using the URL
//! that request carries, rather than being constructed eagerly at startup with a
//! hardcoded default. Once built it lives for the rest of the daemon's lifetime;
//! the engine (API client, WebSocket, session keys, in-memory state) is bound to
//! one server, so a later per-request URL is required to match
//! (see [`ensure_requested_base_url`]).

use std::{path::PathBuf, sync::Arc};

use clipper_client::{
    api_client::ClientError,
    engine::{AppState, SavedProfile, SyncEngine},
};
use tokio::sync::{RwLock, watch};

use crate::keychain::Credentials;

pub struct EngineManager {
    data_dir: PathBuf,
    default_server_url: String,
    stored_creds: RwLock<Option<Credentials>>,
    slot: RwLock<Option<Arc<SyncEngine>>>,
    // Bumped whenever the engine is installed or cleared so the state watcher can
    // (re)subscribe as it comes and goes across login/logout.
    ready: watch::Sender<u64>,
}

impl EngineManager {
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
            .map(|creds| SavedProfile {
                username: creds.username.clone(),
                device_name: creds.device_name.clone(),
            })
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

    /// Drop the current engine (on logout) so the next login/register can target a
    /// different server without restarting the daemon. Also forgets the stored
    /// profile, since the credentials have just been cleared.
    pub async fn clear(&self) {
        *self.slot.write().await = None;
        *self.stored_creds.write().await = None;
        self.bump_ready();
    }

    /// Watch handle for engine install/clear events so the state watcher can
    /// re-subscribe whenever the engine is (re)built or torn down.
    pub fn subscribe_ready(&self) -> watch::Receiver<u64> {
        self.ready.subscribe()
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
    use super::*;

    fn manager(name: &str, stored: Option<Credentials>) -> Arc<EngineManager> {
        let dir = std::env::temp_dir().join(format!("clipper-engine-manager-test-{name}"));
        EngineManager::new(dir, "http://127.0.0.1:8787".to_string(), stored)
    }

    fn creds(server_url: &str, username: &str) -> Credentials {
        Credentials {
            device_name: "Test Device".to_string(),
            server_url: server_url.to_string(),
            username: username.to_string(),
        }
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

        // Clearing forgets the prefill, matching the cleared keychain on logout.
        mgr.clear().await;
        assert!(mgr.current_state().await.saved_profile.is_none());
    }
}
