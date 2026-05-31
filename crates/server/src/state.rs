use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::Utc;
use sea_orm::{Database, DatabaseConnection, EntityTrait, QueryOrder, QuerySelect};
use sea_orm_migration::MigratorTrait;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    config::ServerConfig, entity::event_log, error::ServerResult, migration, secret::ServerSecrets,
    ws::WsBroadcast,
};

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub db: DatabaseConnection,
    pub data_dir: PathBuf,
    pub config: ServerConfig,
    pub secrets: Arc<ServerSecrets>,
    pub ws_tx: broadcast::Sender<WsBroadcast>,
    auth_challenges: std::sync::Mutex<HashMap<String, AuthChallenge>>,
    pending_registrations: std::sync::Mutex<HashMap<String, PendingRegistration>>,
    /// High-water mark for the application-assigned `event_log.seq` clock.
    event_seq: AtomicI64,
}

pub struct AuthChallenge {
    /// `None` for a fabricated challenge issued to an unknown username; such a
    /// challenge can never produce a verifying finalization.
    pub user_id: Option<Uuid>,
    pub server_login_state: Vec<u8>,
    pub device_proof_challenge: Vec<u8>,
    expires_at: Instant,
}

pub struct PendingRegistration {
    pub user_id: Uuid,
    pub username: String,
    pub access_key_hash: String,
    expires_at: Instant,
}

impl AppState {
    pub(crate) async fn open(config: ServerConfig, secrets: ServerSecrets) -> ServerResult<Self> {
        let data_dir = config.server.data_dir.clone();
        tokio::fs::create_dir_all(&data_dir).await?;
        let db = Self::connect_db(&data_dir).await?;
        Self::open_with_db_and_config(db, config, secrets).await
    }

    #[cfg(test)]
    pub(crate) async fn open_with_db(
        db: DatabaseConnection,
        data_dir: PathBuf,
    ) -> ServerResult<Self> {
        let mut config = ServerConfig::default();
        config.server.data_dir = data_dir;
        Self::open_with_db_and_config(db, config, ServerSecrets::test_fixture()).await
    }

    pub(crate) async fn open_with_db_and_config(
        db: DatabaseConnection,
        config: ServerConfig,
        secrets: ServerSecrets,
    ) -> ServerResult<Self> {
        let state = Self::new(db, config, secrets);
        state.run_migrations().await?;
        tokio::try_join!(state.seed_event_seq(), state.ensure_storage_dirs())?;
        Ok(state)
    }

    /// Seed the in-memory `event_log.seq` clock from the largest seq already
    /// persisted, so a restart (or a wall clock that jumped backward) can never
    /// reissue a value at or below one a client has already observed.
    async fn seed_event_seq(&self) -> ServerResult<()> {
        let max_seq: Option<i64> = event_log::Entity::find()
            .select_only()
            .column(event_log::Column::Seq)
            .order_by_desc(event_log::Column::Seq)
            .into_tuple()
            .one(self.db())
            .await?;
        self.inner
            .event_seq
            .store(max_seq.unwrap_or(0), Ordering::SeqCst);
        Ok(())
    }

    async fn connect_db(data_dir: impl AsRef<Path>) -> ServerResult<DatabaseConnection> {
        let db_path = data_dir.as_ref().join("clipper.db");
        let url = format!("sqlite:{}?mode=rwc", db_path.display());
        let db = Database::connect(&url).await?;
        Ok(db)
    }

    async fn run_migrations(&self) -> ServerResult<()> {
        migration::Migrator::up(self.db(), None).await?;
        Ok(())
    }

    async fn ensure_storage_dirs(&self) -> ServerResult<()> {
        tokio::fs::create_dir_all(self.objects_dir()).await?;
        Ok(())
    }

    fn new(db: DatabaseConnection, config: ServerConfig, secrets: ServerSecrets) -> Self {
        let (ws_tx, _) = broadcast::channel(256);
        let data_dir = config.server.data_dir.clone();
        Self {
            inner: Arc::new(AppStateInner {
                db,
                data_dir,
                config,
                secrets: Arc::new(secrets),
                ws_tx,
                auth_challenges: std::sync::Mutex::new(HashMap::new()),
                pending_registrations: std::sync::Mutex::new(HashMap::new()),
                event_seq: AtomicI64::new(0),
            }),
        }
    }

    /// Allocate the next `event_log.seq`: the current Unix time in microseconds,
    /// forced strictly above the previous value so it never collides or moves
    /// backward (even under concurrent inserts or a backward clock step). Callers
    /// must allocate this while their transaction already holds the write lock so
    /// seq order matches commit order.
    pub fn next_event_seq(&self) -> i64 {
        let now = Utc::now().timestamp_micros();
        let mut prev = self.inner.event_seq.load(Ordering::Relaxed);
        loop {
            let candidate = now.max(prev + 1);
            match self.inner.event_seq.compare_exchange_weak(
                prev,
                candidate,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => return candidate,
                Err(actual) => prev = actual,
            }
        }
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.inner.db
    }

    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    pub fn config(&self) -> &ServerConfig {
        &self.inner.config
    }

    pub fn secrets(&self) -> &ServerSecrets {
        &self.inner.secrets
    }

    pub fn objects_dir(&self) -> PathBuf {
        self.inner.data_dir.join("objects")
    }

    pub fn ws_tx(&self) -> &broadcast::Sender<WsBroadcast> {
        &self.inner.ws_tx
    }

    pub fn create_auth_challenge(
        &self,
        user_id: Option<Uuid>,
        server_login_state: Vec<u8>,
        device_proof_challenge: Vec<u8>,
    ) -> String {
        let now = Instant::now();
        let mut challenges = self.inner.auth_challenges.lock().expect("lock poisoned");
        challenges.retain(|_, challenge| challenge.expires_at > now);

        while challenges.len() >= self.config().auth.max_pending_challenges {
            if let Some(id) = challenges.keys().next().cloned() {
                challenges.remove(&id);
            } else {
                break;
            }
        }

        let challenge_id = uuid::Uuid::now_v7().to_string();
        challenges.insert(
            challenge_id.clone(),
            AuthChallenge {
                user_id,
                server_login_state,
                device_proof_challenge,
                expires_at: now + Duration::from_secs(self.config().auth.challenge_ttl_secs),
            },
        );
        challenge_id
    }

    pub fn take_auth_challenge(&self, challenge_id: &str) -> Option<AuthChallenge> {
        let now = Instant::now();
        let mut challenges = self.inner.auth_challenges.lock().expect("lock poisoned");
        challenges.retain(|_, challenge| challenge.expires_at > now);
        challenges.remove(challenge_id)
    }

    pub fn create_pending_registration(
        &self,
        user_id: Uuid,
        username: String,
        access_key_hash: String,
    ) -> String {
        let now = Instant::now();
        let mut registrations = self
            .inner
            .pending_registrations
            .lock()
            .expect("lock poisoned");
        registrations.retain(|_, registration| registration.expires_at > now);

        while registrations.len() >= self.config().auth.max_pending_challenges {
            if let Some(id) = registrations.keys().next().cloned() {
                registrations.remove(&id);
            } else {
                break;
            }
        }

        let registration_id = uuid::Uuid::now_v7().to_string();
        registrations.insert(
            registration_id.clone(),
            PendingRegistration {
                user_id,
                username,
                access_key_hash,
                expires_at: now + Duration::from_secs(self.config().auth.challenge_ttl_secs),
            },
        );
        registration_id
    }

    pub fn take_pending_registration(&self, registration_id: &str) -> Option<PendingRegistration> {
        let now = Instant::now();
        let mut registrations = self
            .inner
            .pending_registrations
            .lock()
            .expect("lock poisoned");
        registrations.retain(|_, registration| registration.expires_at > now);
        registrations.remove(registration_id)
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::Database;

    use super::*;

    async fn test_state() -> AppState {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        AppState::open_with_db(db, dir.path().to_path_buf())
            .await
            .expect("state")
    }

    // A tight loop forces many allocations into the same microsecond, exercising
    // the monotonic `prev + 1` path that keeps the sync cursor unique.
    #[tokio::test]
    async fn next_event_seq_is_strictly_increasing_and_unique() {
        let state = test_state().await;
        let mut prev = state.next_event_seq();
        for _ in 0..10_000 {
            let next = state.next_event_seq();
            assert!(next > prev, "seq must strictly increase: {next} !> {prev}");
            prev = next;
        }
    }
}
