use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use sea_orm::{Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{error::ServerResult, migration, ws::WsBroadcast};

const AUTH_CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_AUTH_CHALLENGES: usize = 4096;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub db: DatabaseConnection,
    pub data_dir: PathBuf,
    pub ws_tx: broadcast::Sender<WsBroadcast>,
    auth_challenges: std::sync::Mutex<HashMap<String, AuthChallenge>>,
    pending_registrations: std::sync::Mutex<HashMap<String, PendingRegistration>>,
}

pub struct AuthChallenge {
    pub user_id: Uuid,
    pub server_login_state: Vec<u8>,
    expires_at: Instant,
}

pub struct PendingRegistration {
    pub user_id: Uuid,
    pub access_key_hash: String,
    pub opaque_server_setup: Vec<u8>,
    pub encryption_salt: Vec<u8>,
    expires_at: Instant,
}

impl AppState {
    pub(crate) async fn open(data_dir: PathBuf) -> ServerResult<Self> {
        tokio::fs::create_dir_all(&data_dir).await?;
        let db = Self::connect_db(&data_dir).await?;
        Self::open_with_db(db, data_dir).await
    }

    pub(crate) async fn open_with_db(
        db: DatabaseConnection,
        data_dir: PathBuf,
    ) -> ServerResult<Self> {
        let state = Self::new(db, data_dir);
        state.run_migrations().await?;
        state.ensure_storage_dirs().await?;
        Ok(state)
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
        tokio::try_join!(
            tokio::fs::create_dir_all(self.clipboard_dir()),
            tokio::fs::create_dir_all(self.files_dir()),
            tokio::fs::create_dir_all(self.objects_dir()),
        )?;
        Ok(())
    }

    fn new(db: DatabaseConnection, data_dir: PathBuf) -> Self {
        let (ws_tx, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(AppStateInner {
                db,
                data_dir,
                ws_tx,
                auth_challenges: std::sync::Mutex::new(HashMap::new()),
                pending_registrations: std::sync::Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.inner.db
    }

    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    pub fn clipboard_dir(&self) -> PathBuf {
        self.inner.data_dir.join("clipboard")
    }

    pub fn files_dir(&self) -> PathBuf {
        self.inner.data_dir.join("files")
    }

    pub fn objects_dir(&self) -> PathBuf {
        self.inner.data_dir.join("objects")
    }

    pub fn ws_tx(&self) -> &broadcast::Sender<WsBroadcast> {
        &self.inner.ws_tx
    }

    pub fn create_auth_challenge(&self, user_id: Uuid, server_login_state: Vec<u8>) -> String {
        let now = Instant::now();
        let mut challenges = self.inner.auth_challenges.lock().expect("lock poisoned");
        challenges.retain(|_, challenge| challenge.expires_at > now);

        while challenges.len() >= MAX_AUTH_CHALLENGES {
            if let Some(id) = challenges.keys().next().cloned() {
                challenges.remove(&id);
            } else {
                break;
            }
        }

        let challenge_id = uuid::Uuid::new_v4().to_string();
        challenges.insert(
            challenge_id.clone(),
            AuthChallenge {
                user_id,
                server_login_state,
                expires_at: now + AUTH_CHALLENGE_TTL,
            },
        );
        challenge_id
    }

    pub fn take_auth_challenge(&self, challenge_id: &str) -> Option<AuthChallenge> {
        let now = Instant::now();
        let mut challenges = self.inner.auth_challenges.lock().expect("lock poisoned");
        challenges.retain(|_, challenge| challenge.expires_at > now);
        challenges
            .remove(challenge_id)
            .filter(|challenge| challenge.expires_at > now)
    }

    pub fn create_pending_registration(
        &self,
        user_id: Uuid,
        access_key_hash: String,
        opaque_server_setup: Vec<u8>,
        encryption_salt: Vec<u8>,
    ) -> String {
        let now = Instant::now();
        let mut registrations = self
            .inner
            .pending_registrations
            .lock()
            .expect("lock poisoned");
        registrations.retain(|_, registration| registration.expires_at > now);

        while registrations.len() >= MAX_AUTH_CHALLENGES {
            if let Some(id) = registrations.keys().next().cloned() {
                registrations.remove(&id);
            } else {
                break;
            }
        }

        let registration_id = uuid::Uuid::new_v4().to_string();
        registrations.insert(
            registration_id.clone(),
            PendingRegistration {
                user_id,
                access_key_hash,
                opaque_server_setup,
                encryption_salt,
                expires_at: now + AUTH_CHALLENGE_TTL,
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
        registrations
            .remove(registration_id)
            .filter(|registration| registration.expires_at > now)
    }
}
