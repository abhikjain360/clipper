use sea_orm::DatabaseConnection;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::ws::WsBroadcast;

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
}

struct AuthChallenge {
    nonce: [u8; 32],
    expires_at: Instant,
}

impl AppState {
    pub fn new(db: DatabaseConnection, data_dir: PathBuf) -> Self {
        let (ws_tx, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(AppStateInner {
                db,
                data_dir,
                ws_tx,
                auth_challenges: std::sync::Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.inner.db
    }

    pub fn clipboard_dir(&self) -> PathBuf {
        self.inner.data_dir.join("clipboard")
    }

    pub fn files_dir(&self) -> PathBuf {
        self.inner.data_dir.join("files")
    }

    pub fn ws_tx(&self) -> &broadcast::Sender<WsBroadcast> {
        &self.inner.ws_tx
    }

    pub fn create_auth_challenge(&self) -> (String, [u8; 32]) {
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
        let nonce = clipper_core::crypto::generate_token();
        challenges.insert(
            challenge_id.clone(),
            AuthChallenge {
                nonce,
                expires_at: now + AUTH_CHALLENGE_TTL,
            },
        );
        (challenge_id, nonce)
    }

    pub fn take_auth_challenge(&self, challenge_id: &str) -> Option<[u8; 32]> {
        let now = Instant::now();
        let mut challenges = self.inner.auth_challenges.lock().expect("lock poisoned");
        challenges.retain(|_, challenge| challenge.expires_at > now);
        challenges
            .remove(challenge_id)
            .filter(|challenge| challenge.expires_at > now)
            .map(|challenge| challenge.nonce)
    }
}
