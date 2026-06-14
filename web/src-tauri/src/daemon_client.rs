use std::path::PathBuf;

use clipper_app_types::AppState;
use clipper_daemon_types::DaemonCommand;
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum DaemonClientError {
    #[error("daemon not connected")]
    NotConnected,
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("IPC protocol error: {0}")]
    Protocol(String),
}

// ── public interface ──────────────────────────────────────────────────────────

pub use inner::DaemonClient;

impl DaemonClient {
    pub async fn send_ok(&self, command: DaemonCommand) -> Result<(), DaemonClientError> {
        self.send(command).await.map(|_| ())
    }

    pub async fn send_result<T: DeserializeOwned>(
        &self,
        command: DaemonCommand,
    ) -> Result<T, DaemonClientError> {
        let value = self
            .send(command)
            .await?
            .ok_or_else(|| DaemonClientError::Protocol("expected result payload".into()))?;
        serde_json::from_value(value).map_err(|e| DaemonClientError::Protocol(e.to_string()))
    }
}

// ── unix implementation ───────────────────────────────────────────────────────

#[cfg(unix)]
mod inner {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    use clipper_app_types::AppState;
    use clipper_daemon_types::{
        AuthChallenge, AuthenticateParams, AuthenticateResult, DaemonCommand, DaemonEvent,
        DaemonLine, DaemonRequest, DaemonResponse, IPC_AUTH_NONCE_BYTES, IPC_AUTH_TAG_BYTES,
        IPC_AUTH_VERSION, ipc_client_auth_message, ipc_daemon_auth_message, ipc_path,
    };
    use hmac::{Hmac, Mac};
    use rand::RngExt;
    use sha2::Sha256;
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::unix::{OwnedReadHalf, OwnedWriteHalf},
        sync::{Notify, RwLock, mpsc, oneshot},
        time::{Duration, sleep},
    };
    use tracing::{debug, info, warn};
    use zeroize::Zeroizing;

    use super::DaemonClientError;
    use crate::ipc_secret;

    type HmacSha256 = Hmac<Sha256>;
    const MAX_LINE: usize = 32 * 1024 * 1024;
    const INIT_DELAY: Duration = Duration::from_millis(200);
    const MAX_DELAY: Duration = Duration::from_secs(5);

    struct PendingRequest {
        command: DaemonCommand,
        reply_tx: oneshot::Sender<Result<Option<serde_json::Value>, DaemonClientError>>,
    }

    struct Shared {
        state: RwLock<AppState>,
        version: AtomicU64,
        notify: Notify,
    }

    pub struct DaemonClient {
        tx: mpsc::UnboundedSender<PendingRequest>,
        shared: Arc<Shared>,
    }

    impl DaemonClient {
        pub fn new_with_future(
            data_dir: PathBuf,
        ) -> (Self, impl std::future::Future<Output = ()>) {
            let (tx, rx) = mpsc::unbounded_channel::<PendingRequest>();
            let shared = Arc::new(Shared {
                state: RwLock::new(AppState::default()),
                version: AtomicU64::new(0),
                notify: Notify::new(),
            });
            let shared2 = Arc::clone(&shared);
            let fut = connection_loop(data_dir, rx, shared2);
            (Self { tx, shared }, fut)
        }

        pub async fn get_state(&self) -> AppState {
            self.shared.state.read().await.clone()
        }

        pub fn state_version(&self) -> u64 {
            self.shared.version.load(Ordering::Relaxed)
        }

        pub async fn wait_for_state_change_after(&self, seen: u64) -> u64 {
            loop {
                // Subscribe before reading version to avoid a lost-notify race.
                let notified = self.shared.notify.notified();
                let cur = self.state_version();
                if cur != seen {
                    return cur;
                }
                notified.await;
            }
        }

        pub async fn send(
            &self,
            command: DaemonCommand,
        ) -> Result<Option<serde_json::Value>, DaemonClientError> {
            let (reply_tx, reply_rx) = oneshot::channel();
            self.tx
                .send(PendingRequest { command, reply_tx })
                .map_err(|_| DaemonClientError::NotConnected)?;
            reply_rx
                .await
                .map_err(|_| DaemonClientError::NotConnected)?
        }
    }

    async fn connection_loop(
        data_dir: PathBuf,
        mut rx: mpsc::UnboundedReceiver<PendingRequest>,
        shared: Arc<Shared>,
    ) {
        let mut delay = INIT_DELAY;
        loop {
            let path = ipc_path::socket_path();
            match session(&path, &data_dir, &mut rx, &shared).await {
                Ok(()) => {
                    delay = INIT_DELAY;
                }
                Err(e) => {
                    debug!("Daemon IPC disconnected: {}", e);
                    sleep(delay).await;
                    delay = (delay * 2).min(MAX_DELAY);
                }
            }
        }
    }

    async fn session(
        path: &Path,
        data_dir: &Path,
        rx: &mut mpsc::UnboundedReceiver<PendingRequest>,
        shared: &Arc<Shared>,
    ) -> Result<(), String> {
        let stream = tokio::net::UnixStream::connect(path)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        authenticate(&mut reader, &mut write_half, data_dir).await?;
        info!("Daemon IPC authenticated");

        run(&mut reader, &mut write_half, rx, shared).await
    }

    async fn authenticate(
        reader: &mut BufReader<OwnedReadHalf>,
        writer: &mut OwnedWriteHalf,
        data_dir: &Path,
    ) -> Result<(), String> {
        let line = read_line(reader)
            .await
            .map_err(|e| format!("auth read: {e}"))?;
        let DaemonEvent::AuthChallenge {
            auth_challenge: AuthChallenge { protocol_version, daemon_nonce },
        } = serde_json::from_str::<DaemonEvent>(&line)
            .map_err(|e| format!("auth parse: {e}"))?
        else {
            return Err("expected AuthChallenge".into());
        };

        if protocol_version != IPC_AUTH_VERSION {
            return Err(format!("unsupported protocol version {protocol_version}"));
        }

        let secret = ipc_secret::load_ipc_secret(data_dir)
            .map_err(|e| format!("load IPC secret: {e}"))?;

        let mut client_nonce = [0u8; IPC_AUTH_NONCE_BYTES];
        rand::rng().fill(&mut client_nonce);

        let tag = hmac_tag(&secret, &ipc_client_auth_message(&daemon_nonce, &client_nonce))?;

        let req = DaemonRequest::new(
            "auth".into(),
            DaemonCommand::Authenticate(AuthenticateParams {
                protocol_version: IPC_AUTH_VERSION,
                client_nonce: client_nonce.to_vec(),
                tag,
            }),
        );
        write_line(writer, &serde_json::to_string(&req).map_err(|e| e.to_string())?)
            .await
            .map_err(|e| format!("auth write: {e}"))?;

        let line = read_line(reader)
            .await
            .map_err(|e| format!("auth result read: {e}"))?;
        match serde_json::from_str::<DaemonResponse>(&line)
            .map_err(|e| format!("auth result parse: {e}"))?
        {
            DaemonResponse::Success { result: Some(val), .. } => {
                let result: AuthenticateResult = serde_json::from_value(val)
                    .map_err(|e| format!("auth result decode: {e}"))?;
                if result.tag.len() != IPC_AUTH_TAG_BYTES {
                    return Err("daemon auth tag wrong length".into());
                }
                let expected =
                    hmac_tag(&secret, &ipc_daemon_auth_message(&daemon_nonce, &client_nonce))?;
                if result.tag != expected {
                    return Err("daemon HMAC verification failed".into());
                }
                Ok(())
            }
            DaemonResponse::Success { result: None, .. } => Err("auth: missing result".into()),
            DaemonResponse::Error { error, .. } => Err(format!("auth error: {}", error.message)),
        }
    }

    async fn run(
        reader: &mut BufReader<OwnedReadHalf>,
        writer: &mut OwnedWriteHalf,
        rx: &mut mpsc::UnboundedReceiver<PendingRequest>,
        shared: &Arc<Shared>,
    ) -> Result<(), String> {
        let mut in_flight: HashMap<
            String,
            oneshot::Sender<Result<Option<serde_json::Value>, DaemonClientError>>,
        > = HashMap::new();
        let mut next_id: u64 = 1;

        loop {
            tokio::select! {
                pending = rx.recv() => {
                    let Some(pending) = pending else { return Ok(()); };
                    let id = next_id.to_string();
                    next_id += 1;
                    let req = DaemonRequest::new(id.clone(), pending.command);
                    let json = serde_json::to_string(&req).map_err(|e| e.to_string())?;
                    write_line(writer, &json).await.map_err(|e| format!("write: {e}"))?;
                    in_flight.insert(id, pending.reply_tx);
                }
                line = read_line(reader) => {
                    let line = line.map_err(|e| format!("read: {e}"))?;
                    if line.is_empty() { continue; }
                    match serde_json::from_str::<DaemonLine>(&line) {
                        Ok(DaemonLine::Response(resp)) => {
                            let (id, result) = match resp {
                                DaemonResponse::Success { id, result } => (id, Ok(result)),
                                DaemonResponse::Error { id, error } => {
                                    (id, Err(DaemonClientError::Daemon(error.message)))
                                }
                            };
                            if let Some(tx) = in_flight.remove(&id) {
                                let _ = tx.send(result);
                            }
                        }
                        Ok(DaemonLine::Event(DaemonEvent::StateChanged { state })) => {
                            *shared.state.write().await = state;
                            shared.version.fetch_add(1, Ordering::Relaxed);
                            shared.notify.notify_waiters();
                        }
                        Ok(DaemonLine::Event(DaemonEvent::AuthChallenge { .. })) => {
                            warn!("Unexpected AuthChallenge after auth");
                        }
                        Err(e) => warn!("Failed to parse daemon line: {e}"),
                    }
                }
            }
        }
    }

    async fn read_line(reader: &mut BufReader<OwnedReadHalf>) -> Result<String, String> {
        let mut buf = Vec::new();
        loop {
            let available = reader
                .fill_buf()
                .await
                .map_err(|e| format!("read: {e}"))?;
            if available.is_empty() {
                return Err("daemon disconnected".into());
            }
            let take = available
                .iter()
                .position(|&b| b == b'\n')
                .map_or(available.len(), |p| p + 1);
            if buf.len() + take > MAX_LINE {
                return Err("line too long".into());
            }
            buf.extend_from_slice(&available[..take]);
            reader.consume(take);
            if buf.ends_with(b"\n") {
                break;
            }
        }
        String::from_utf8(buf)
            .map(|s| s.trim().to_owned())
            .map_err(|e| format!("utf8: {e}"))
    }

    async fn write_line(writer: &mut OwnedWriteHalf, line: &str) -> std::io::Result<()> {
        writer.write_all(format!("{}\n", line).as_bytes()).await
    }

    fn hmac_tag(secret: &Zeroizing<Vec<u8>>, message: &[u8]) -> Result<Vec<u8>, String> {
        let mut mac =
            HmacSha256::new_from_slice(secret).map_err(|e| format!("HMAC init: {e}"))?;
        mac.update(message);
        Ok(mac.finalize().into_bytes().to_vec())
    }
}

// ── stub for non-unix platforms ───────────────────────────────────────────────

#[cfg(not(unix))]
mod inner {
    use std::path::PathBuf;

    use clipper_app_types::AppState;
    use clipper_daemon_types::DaemonCommand;

    use super::DaemonClientError;

    pub struct DaemonClient;

    impl DaemonClient {
        pub fn new_with_future(
            _data_dir: PathBuf,
        ) -> (Self, impl std::future::Future<Output = ()>) {
            (Self, async {})
        }

        pub async fn get_state(&self) -> AppState {
            AppState::default()
        }

        pub fn state_version(&self) -> u64 {
            0
        }

        pub async fn wait_for_state_change_after(&self, _seen: u64) -> u64 {
            tokio::time::sleep(tokio::time::Duration::from_secs(u32::MAX as u64)).await;
            0
        }

        pub async fn send(
            &self,
            _command: DaemonCommand,
        ) -> Result<Option<serde_json::Value>, DaemonClientError> {
            Err(DaemonClientError::NotConnected)
        }
    }
}
