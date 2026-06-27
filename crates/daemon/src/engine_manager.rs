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
    pub async fn get_or_build(
        &self,
        requested_url: Option<&str>,
    ) -> Result<Arc<SyncEngine>, ClientError> {
        if let Some(engine) = self.engine().await {
            ensure_requested_base_url(&engine, requested_url)?;
            return Ok(engine);
        }

        let mut slot = self.slot.write().await;
        // Another task may have built it while we waited for the write lock.
        if let Some(engine) = slot.as_ref() {
            ensure_requested_base_url(engine, requested_url)?;
            return Ok(Arc::clone(engine));
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
        Ok(engine)
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

        let engine = mgr
            .get_or_build(Some("https://a.example"))
            .await
            .expect("build engine");
        assert_eq!(engine.base_url(), "https://a.example");
        assert!(mgr.engine().await.is_some());
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
        let engine = mgr
            .get_or_build(Some("https://b.example"))
            .await
            .expect("rebuild b");
        assert_eq!(engine.base_url(), "https://b.example");
    }

    #[tokio::test]
    async fn falls_back_to_stored_url_when_request_omits_it() {
        let mgr = manager("fallback", Some(creds("https://stored.example", "alice")));
        let engine = mgr.get_or_build(None).await.expect("build from stored");
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
