use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use clipper_core::crypto;
use sea_orm::{Database, DatabaseConnection, EntityTrait, QueryOrder, QuerySelect};
use sea_orm_migration::MigratorTrait;
use tokio::sync::broadcast::{self, Receiver};
use uuid::Uuid;

use crate::{
    auth::AuthInfo, config::ServerConfig, entity::event_log, error::ServerResult, migration,
    rate_limit::RateLimiter, secret::ServerSecrets, ws::WsBroadcast,
};

const WS_TICKET_BYTES: usize = 32;
const WS_TICKET_TTL_SECS: i64 = 60;
const WS_BROADCAST_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub db: DatabaseConnection,
    pub data_dir: PathBuf,
    pub config: ServerConfig,
    pub secrets: Arc<ServerSecrets>,
    rate_limiter: RateLimiter,
    ws_channels: std::sync::Mutex<HashMap<Uuid, broadcast::Sender<WsBroadcast>>>,
    /// Count of live WebSocket connections per user, bounding concurrent
    /// connections so one authenticated account cannot exhaust FDs/tasks. Slots
    /// are reserved in `try_acquire_ws_slot` and released when the returned guard
    /// drops; an entry is removed once its count returns to zero.
    ws_connections: std::sync::Mutex<HashMap<Uuid, u64>>,
    auth_challenges: std::sync::Mutex<HashMap<String, AuthChallenge>>,
    pending_registrations: std::sync::Mutex<HashMap<String, PendingRegistration>>,
    ws_tickets: std::sync::Mutex<HashMap<Vec<u8>, WsTicket>>,
    /// High-water mark for the application-assigned `event_log.seq` clock.
    event_seq: AtomicI64,
    /// Bounds concurrent memory-hard Argon2 access-key hashing. Each hash holds
    /// ~19 MiB, so an unbounded burst of registration attempts could exhaust
    /// memory; this caps the peak regardless of how many requests arrive.
    argon2_semaphore: Arc<tokio::sync::Semaphore>,
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

pub struct WsTicket {
    pub auth: AuthInfo,
    expires_at: Instant,
}

pub struct IssuedWsTicket {
    pub ticket: String,
    pub expires_at: DateTime<Utc>,
}

/// Holds a reserved live WebSocket connection slot (see
/// [`AppState::try_acquire_ws_slot`]); releasing it on drop returns the slot to
/// the user's per-user budget so a clean or abrupt disconnect frees capacity.
pub struct WsConnectionGuard {
    state: AppState,
    user_id: Uuid,
}

impl Drop for WsConnectionGuard {
    fn drop(&mut self) {
        self.state.release_ws_slot(self.user_id);
    }
}

impl AppState {
    pub(crate) async fn open(config: ServerConfig, secrets: ServerSecrets) -> ServerResult<Self> {
        let data_dir = config.server.data_dir.clone();
        Self::ensure_private_dir(&data_dir).await?;
        let db = Self::connect_db(&data_dir).await?;
        Self::open_with_db_and_config(db, config, secrets).await
    }

    /// Create a directory owner-only (0700 on Unix). The server data dir and its
    /// objects subdir hold the SQLite database and all ciphertext payloads, so
    /// they must not be group/world readable.
    async fn ensure_private_dir(path: &Path) -> ServerResult<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .await?;
            // Tighten a directory that predates this enforcement (DirBuilder only
            // sets the mode on dirs it creates).
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
        }
        #[cfg(not(unix))]
        {
            tokio::fs::create_dir_all(path).await?;
        }
        Ok(())
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
        // The database file (and any WAL/SHM sidecars) hold every user's wrapped
        // auth blobs and object metadata; keep them owner-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for suffix in ["", "-wal", "-shm"] {
                let path = data_dir.as_ref().join(format!("clipper.db{suffix}"));
                if tokio::fs::metadata(&path).await.is_ok() {
                    _ = tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .await;
                }
            }
        }
        Ok(db)
    }

    async fn run_migrations(&self) -> ServerResult<()> {
        migration::Migrator::up(self.db(), None).await?;
        Ok(())
    }

    async fn ensure_storage_dirs(&self) -> ServerResult<()> {
        Self::ensure_private_dir(&self.objects_dir()).await?;
        Ok(())
    }

    fn new(db: DatabaseConnection, config: ServerConfig, secrets: ServerSecrets) -> Self {
        let data_dir = config.server.data_dir.clone();
        let rate_limiter = RateLimiter::new(&config.rate_limit);
        // Argon2 is CPU-bound, so allowing more than the core count to run at
        // once only inflates peak memory without improving throughput.
        let argon2_permits = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            inner: Arc::new(AppStateInner {
                db,
                data_dir,
                config,
                secrets: Arc::new(secrets),
                rate_limiter,
                ws_channels: std::sync::Mutex::new(HashMap::new()),
                ws_connections: std::sync::Mutex::new(HashMap::new()),
                auth_challenges: std::sync::Mutex::new(HashMap::new()),
                pending_registrations: std::sync::Mutex::new(HashMap::new()),
                ws_tickets: std::sync::Mutex::new(HashMap::new()),
                event_seq: AtomicI64::new(0),
                argon2_semaphore: Arc::new(tokio::sync::Semaphore::new(argon2_permits)),
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

    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.inner.rate_limiter
    }

    /// Permits for concurrent Argon2 access-key hashing; acquire one before
    /// running the hash so a registration burst cannot exhaust memory.
    pub fn argon2_semaphore(&self) -> Arc<tokio::sync::Semaphore> {
        self.inner.argon2_semaphore.clone()
    }

    pub fn objects_dir(&self) -> PathBuf {
        self.inner.data_dir.join("objects")
    }

    pub fn subscribe_ws_broadcasts(&self, user_id: Uuid) -> Receiver<WsBroadcast> {
        let mut channels = self.inner.ws_channels.lock().expect("lock poisoned");
        channels
            .entry(user_id)
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(WS_BROADCAST_CAPACITY);
                tx
            })
            .subscribe()
    }

    /// Fan a live event out to the user's WebSocket subscribers.
    ///
    /// Delivery order is NOT guaranteed to match `seq`/commit order: each handler
    /// broadcasts after its own commit, across an await boundary, so two
    /// concurrent mutations can be sent in either order. Every consumer must
    /// treat each event as an independent per-`object_id` signal and reconcile by
    /// object id with last-writer-wins by `seq` (as the client's local store
    /// does), never as a strictly increasing cursor. The connect-time
    /// `stream_start_seq` boundary is a fixed snapshot watermark and is safe.
    pub fn broadcast_ws_event(&self, event: WsBroadcast) {
        let sender = {
            let channels = self.inner.ws_channels.lock().expect("lock poisoned");
            channels.get(&event.user_id).cloned()
        };

        if let Some(sender) = sender {
            _ = sender.send(event);
        }
    }

    pub fn prune_idle_ws_broadcast_channel(&self, user_id: Uuid) {
        let mut channels = self.inner.ws_channels.lock().expect("lock poisoned");
        if channels
            .get(&user_id)
            .is_some_and(|sender| sender.receiver_count() == 0)
        {
            channels.remove(&user_id);
        }
    }

    #[cfg(test)]
    fn ws_broadcast_channel_count(&self) -> usize {
        self.inner.ws_channels.lock().expect("lock poisoned").len()
    }

    /// Reserve a live WebSocket connection slot for `user_id`, or return `None`
    /// if the user is already at `limits.max_user_ws_connections`. The returned
    /// guard releases the slot when dropped (i.e. when the connection ends), so
    /// callers must hold it for the connection's lifetime.
    pub fn try_acquire_ws_slot(&self, user_id: Uuid) -> Option<WsConnectionGuard> {
        let cap = self.config().limits.max_user_ws_connections;
        let mut conns = self.inner.ws_connections.lock().expect("lock poisoned");
        let count = conns.entry(user_id).or_insert(0);
        if *count >= cap {
            // The entry already existed at the cap (cap >= 1, so a freshly
            // inserted 0 never lands here and is incremented below).
            return None;
        }
        *count += 1;
        Some(WsConnectionGuard {
            state: self.clone(),
            user_id,
        })
    }

    fn release_ws_slot(&self, user_id: Uuid) {
        let mut conns = self.inner.ws_connections.lock().expect("lock poisoned");
        if let Some(count) = conns.get_mut(&user_id) {
            *count -= 1;
            if *count == 0 {
                conns.remove(&user_id);
            }
        }
    }

    #[cfg(test)]
    fn ws_connection_count(&self, user_id: Uuid) -> u64 {
        self.inner
            .ws_connections
            .lock()
            .expect("lock poisoned")
            .get(&user_id)
            .copied()
            .unwrap_or(0)
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

        // Evict oldest-first (smallest expires_at; all challenges share one TTL)
        // rather than an arbitrary HashMap entry, so a flood at the cap displaces
        // its own stale challenges before a victim's in-flight login.
        while challenges.len() >= self.config().auth.max_pending_challenges {
            let Some(oldest) = challenges
                .iter()
                .min_by_key(|(_, challenge)| challenge.expires_at)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            challenges.remove(&oldest);
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

        // Oldest-first eviction (see create_auth_challenge) so a registration
        // flood cannot displace a victim's in-flight pending registration.
        while registrations.len() >= self.config().auth.max_pending_challenges {
            let Some(oldest) = registrations
                .iter()
                .min_by_key(|(_, registration)| registration.expires_at)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            registrations.remove(&oldest);
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

    pub fn create_ws_ticket(&self, auth: AuthInfo) -> IssuedWsTicket {
        let now = Instant::now();
        let expires_at = Utc::now() + chrono::Duration::seconds(WS_TICKET_TTL_SECS);
        let mut tickets = self.inner.ws_tickets.lock().expect("lock poisoned");
        tickets.retain(|_, ticket| ticket.expires_at > now);

        let user_id = auth.user_id;
        // All tickets share a fixed TTL, so the smallest `expires_at` is the
        // oldest ticket. Evict oldest-first within this user so a burst from
        // one account does not displace another user's ticket.
        while tickets
            .values()
            .filter(|ticket| ticket.auth.user_id == user_id)
            .count()
            >= self.config().auth.max_pending_ws_tickets
        {
            let Some(oldest) = tickets
                .iter()
                .filter(|(_, ticket)| ticket.auth.user_id == user_id)
                .min_by_key(|(_, ticket)| ticket.expires_at)
                .map(|(hash, _)| hash.clone())
            else {
                break;
            };
            tickets.remove(&oldest);
        }

        let ticket = URL_SAFE_NO_PAD.encode(crypto::generate_random_bytes(WS_TICKET_BYTES));
        tickets.insert(
            crypto::sha256(ticket.as_bytes()).to_vec(),
            WsTicket {
                auth,
                expires_at: now + Duration::from_secs(WS_TICKET_TTL_SECS as u64),
            },
        );
        IssuedWsTicket { ticket, expires_at }
    }

    pub fn consume_ws_ticket(&self, ticket: &str) -> Option<AuthInfo> {
        let now = Instant::now();
        let key = crypto::sha256(ticket.as_bytes()).to_vec();
        let mut tickets = self.inner.ws_tickets.lock().expect("lock poisoned");
        // Check only this ticket's expiry instead of sweeping the whole map: this
        // path is unauthenticated (the ticket is the credential), so it must not
        // run an O(n) scan over every user's tickets under the global lock on each
        // connect. Stale tickets are reaped at mint time and capped per user.
        let entry = tickets.remove(&key)?;
        (entry.expires_at > now).then_some(entry.auth)
    }
}

#[cfg(test)]
mod tests {
    use clipper_core::models::{ObjectEventType, ObjectKind};
    use sea_orm::Database;
    use tokio::sync::broadcast::error::TryRecvError;

    use super::*;

    fn ws_broadcast(user_id: Uuid) -> WsBroadcast {
        WsBroadcast {
            user_id,
            source_device_id: Uuid::now_v7(),
            seq: 1,
            event_type: ObjectEventType::Created,
            object_kind: ObjectKind::File,
            object_id: Uuid::now_v7().into(),
            created_at: Utc::now().to_rfc3339(),
        }
    }

    async fn test_state() -> AppState {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        AppState::open_with_db(db, dir.path().to_path_buf())
            .await
            .expect("state")
    }

    async fn test_state_with_ws_ticket_cap(cap: usize) -> AppState {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        let mut config = ServerConfig::default();
        config.server.data_dir = dir.path().to_path_buf();
        config.auth.max_pending_ws_tickets = cap;
        AppState::open_with_db_and_config(db, config, ServerSecrets::test_fixture())
            .await
            .expect("state")
    }

    async fn test_state_with_ws_connection_cap(cap: u64) -> AppState {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        let mut config = ServerConfig::default();
        config.server.data_dir = dir.path().to_path_buf();
        config.limits.max_user_ws_connections = cap;
        AppState::open_with_db_and_config(db, config, ServerSecrets::test_fixture())
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

    #[tokio::test]
    async fn ws_broadcasts_are_partitioned_by_user() {
        let state = test_state().await;
        let user_id = Uuid::now_v7();
        let other_user_id = Uuid::now_v7();
        let mut user_rx = state.subscribe_ws_broadcasts(user_id);
        let mut other_user_rx = state.subscribe_ws_broadcasts(other_user_id);

        state.broadcast_ws_event(ws_broadcast(user_id));

        let event = user_rx.try_recv().expect("user broadcast");
        assert_eq!(event.user_id, user_id);
        assert!(matches!(other_user_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn one_users_burst_does_not_lag_another_users_receiver() {
        let state = test_state().await;
        let user_id = Uuid::now_v7();
        let other_user_id = Uuid::now_v7();
        let mut user_rx = state.subscribe_ws_broadcasts(user_id);
        let mut other_user_rx = state.subscribe_ws_broadcasts(other_user_id);

        for _ in 0..=WS_BROADCAST_CAPACITY {
            state.broadcast_ws_event(ws_broadcast(user_id));
        }
        state.broadcast_ws_event(ws_broadcast(other_user_id));

        assert!(matches!(user_rx.try_recv(), Err(TryRecvError::Lagged(_))));
        let event = other_user_rx.try_recv().expect("other user broadcast");
        assert_eq!(event.user_id, other_user_id);
        assert!(matches!(other_user_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn idle_ws_broadcast_channels_are_pruned_after_last_receiver_drops() {
        let state = test_state().await;
        let user_id = Uuid::now_v7();
        let other_user_id = Uuid::now_v7();
        let user_rx = state.subscribe_ws_broadcasts(user_id);
        let _other_user_rx = state.subscribe_ws_broadcasts(other_user_id);

        assert_eq!(state.ws_broadcast_channel_count(), 2);
        state.prune_idle_ws_broadcast_channel(user_id);
        assert_eq!(state.ws_broadcast_channel_count(), 2);

        drop(user_rx);
        state.prune_idle_ws_broadcast_channel(user_id);
        assert_eq!(state.ws_broadcast_channel_count(), 1);

        let mut new_user_rx = state.subscribe_ws_broadcasts(user_id);
        state.broadcast_ws_event(ws_broadcast(user_id));

        let event = new_user_rx.try_recv().expect("user broadcast");
        assert_eq!(event.user_id, user_id);
        assert_eq!(state.ws_broadcast_channel_count(), 2);
    }

    #[tokio::test]
    async fn ws_tickets_are_single_use() {
        let state = test_state().await;
        let auth = AuthInfo {
            session_id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            device_id: Uuid::now_v7(),
        };
        let issued = state.create_ws_ticket(auth.clone());

        let consumed = state
            .consume_ws_ticket(&issued.ticket)
            .expect("ticket should be valid once");
        assert_eq!(consumed.session_id, auth.session_id);
        assert!(state.consume_ws_ticket(&issued.ticket).is_none());
    }

    #[tokio::test]
    async fn ws_tickets_evict_oldest_first_at_capacity() {
        let state = test_state_with_ws_ticket_cap(2).await;
        let user_id = Uuid::now_v7();
        let auth = || AuthInfo {
            session_id: Uuid::now_v7(),
            user_id,
            device_id: Uuid::now_v7(),
        };

        // Sleep between mints so each ticket gets a distinct `expires_at`,
        // making oldest-first eviction unambiguous.
        let first = state.create_ws_ticket(auth());
        tokio::time::sleep(Duration::from_millis(2)).await;
        let second = state.create_ws_ticket(auth());
        tokio::time::sleep(Duration::from_millis(2)).await;
        let third = state.create_ws_ticket(auth());

        // Cap is 2: minting the third evicts the oldest ticket, not a fresh one
        // that is about to connect.
        assert!(state.consume_ws_ticket(&first.ticket).is_none());
        assert!(state.consume_ws_ticket(&second.ticket).is_some());
        assert!(state.consume_ws_ticket(&third.ticket).is_some());
    }

    #[tokio::test]
    async fn ws_ticket_capacity_is_per_user() {
        let state = test_state_with_ws_ticket_cap(1).await;
        let user_id = Uuid::now_v7();
        let other_user_id = Uuid::now_v7();
        let auth = |user_id| AuthInfo {
            session_id: Uuid::now_v7(),
            user_id,
            device_id: Uuid::now_v7(),
        };

        let first = state.create_ws_ticket(auth(user_id));
        let other = state.create_ws_ticket(auth(other_user_id));
        let second = state.create_ws_ticket(auth(user_id));

        assert!(state.consume_ws_ticket(&first.ticket).is_none());
        assert!(state.consume_ws_ticket(&other.ticket).is_some());
        assert!(state.consume_ws_ticket(&second.ticket).is_some());
    }

    #[tokio::test]
    async fn ws_connection_slots_are_capped_per_user() {
        let state = test_state_with_ws_connection_cap(2).await;
        let user_id = Uuid::now_v7();

        let first = state.try_acquire_ws_slot(user_id).expect("first slot");
        let second = state.try_acquire_ws_slot(user_id).expect("second slot");
        assert_eq!(state.ws_connection_count(user_id), 2);
        // At the cap: the next connection is rejected.
        assert!(state.try_acquire_ws_slot(user_id).is_none());

        // Releasing a slot frees capacity for a new connection.
        drop(first);
        assert_eq!(state.ws_connection_count(user_id), 1);
        let third = state
            .try_acquire_ws_slot(user_id)
            .expect("slot after release");

        drop(second);
        drop(third);
        // The entry is removed once the user has no live connections.
        assert_eq!(state.ws_connection_count(user_id), 0);
    }

    #[tokio::test]
    async fn ws_connection_cap_is_per_user() {
        let state = test_state_with_ws_connection_cap(1).await;
        let user_id = Uuid::now_v7();
        let other_user_id = Uuid::now_v7();

        let _user_slot = state.try_acquire_ws_slot(user_id).expect("user slot");
        assert!(state.try_acquire_ws_slot(user_id).is_none());
        // A different user has an independent budget.
        let _other_slot = state
            .try_acquire_ws_slot(other_user_id)
            .expect("other user slot");
    }
}
