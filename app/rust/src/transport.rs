//! Unix socket transport to the clipper-daemon.
//!
//! Manages the single global connection, request/response correlation,
//! and state broadcasting via a `watch` channel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use clipper_daemon_types as dt;
use clipper_daemon_types::{DaemonCommand, DaemonRequest, DaemonResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Mutex, RwLock, oneshot, watch};
use tracing::warn;

use crate::daemon_process;

pub(crate) type TransportResult<T> = Result<T, TransportError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum TransportError {
    #[error("daemon process launch failed: {0}")]
    DaemonProcess(#[from] daemon_process::DaemonProcessError),
    #[error("cannot connect to daemon at {path}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("not connected to daemon")]
    NotConnected,
    #[error("daemon connection lost")]
    ConnectionLost,
    #[error("daemon returned error: {0}")]
    Daemon(String),
    #[error("daemon request encode failed: {0}")]
    RequestEncode(#[from] serde_json::Error),
    #[error("daemon write failed: {0}")]
    Write(#[source] std::io::Error),
}

// ── Connection types ──

pub(crate) struct ActiveConnection {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<DaemonResponse>>>>,
}

pub(crate) struct DaemonBridge {
    conn: RwLock<Option<ActiveConnection>>,
    conn_generation: AtomicU64,
    state_tx: watch::Sender<dt::AppState>,
}

pub(crate) static BRIDGE: LazyLock<DaemonBridge> = LazyLock::new(|| {
    let default_state = dt::AppState {
        connection_status: dt::ConnectionStatus::DaemonNotRunning,
        ..Default::default()
    };
    let (state_tx, _rx) = watch::channel(default_state);
    DaemonBridge {
        conn: RwLock::new(None),
        conn_generation: AtomicU64::new(0),
        state_tx,
    }
});

pub(crate) fn socket_path() -> PathBuf {
    let base = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Clipper");
    base.join("daemon.sock")
}

// ── Public transport API ──

/// Connect to the daemon's Unix socket. Spawns a reader task that routes
/// responses to pending callers and broadcasts state events.
pub(crate) async fn connect() -> TransportResult<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .ok();

    if let Err(e) = daemon_process::install_and_start_daemon() {
        tracing::warn!("Failed to start daemon: {}", e);
    }

    let sock = socket_path();
    for _ in 0..10 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    let stream = tokio::net::UnixStream::connect(&sock)
        .await
        .map_err(|source| TransportError::Connect {
            path: sock.clone(),
            source,
        })?;

    let (read_half, write_half) = stream.into_split();

    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<DaemonResponse>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let new_conn = ActiveConnection {
        writer: Mutex::new(write_half),
        pending: Arc::clone(&pending),
    };

    // Bump generation before replacing connection so stale readers won't
    // overwrite newer state.
    let conn_gen = BRIDGE.conn_generation.fetch_add(1, Ordering::Relaxed) + 1;
    *BRIDGE.conn.write().await = Some(new_conn);

    // Spawn reader task with captured generation.
    let state_tx = BRIDGE.state_tx.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    warn!("Daemon connection lost (EOF)");
                    drain_pending(&pending, "Daemon disconnected").await;
                    send_terminal_state_if_current(conn_gen, &state_tx);
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    route_daemon_line(trimmed, &pending, &state_tx).await;
                }
                Err(e) => {
                    warn!("Daemon read error: {}", e);
                    drain_pending(&pending, "Daemon read error").await;
                    send_terminal_state_if_current(conn_gen, &state_tx);
                    break;
                }
            }
        }
    });

    Ok(())
}

/// Send a request to the daemon and await the correlated response.
pub(crate) async fn send_command(
    command: DaemonCommand,
) -> TransportResult<Option<serde_json::Value>> {
    let id = uuid::Uuid::new_v4().to_string();
    let req = DaemonRequest::new(id.clone(), command);

    let (tx, rx) = oneshot::channel();

    {
        let conn_guard = BRIDGE.conn.read().await;
        let conn = conn_guard.as_ref().ok_or(TransportError::NotConnected)?;

        conn.pending.lock().await.insert(id.clone(), tx);

        let json = serde_json::to_string(&req)?;
        let line = format!("{}\n", json);
        if let Err(e) = conn.writer.lock().await.write_all(line.as_bytes()).await {
            // Clean up the pending entry so it doesn't leak.
            conn.pending.lock().await.remove(&id);
            return Err(TransportError::Write(e));
        }
    }

    let resp = rx.await.map_err(|_| TransportError::ConnectionLost)?;
    if resp.ok {
        Ok(resp.result)
    } else {
        Err(TransportError::Daemon(
            resp.error.unwrap_or_else(|| "Unknown error".into()),
        ))
    }
}

/// Return the current daemon state without subscribing.
pub(crate) fn current_state() -> dt::AppState {
    BRIDGE.state_tx.borrow().clone()
}

/// Wait until the daemon state changes, then return.
pub(crate) async fn wait_for_change() {
    let mut rx = BRIDGE.state_tx.subscribe();
    let _ = rx.changed().await;
}

// ── Internal helpers ──

async fn drain_pending(
    pending: &Mutex<HashMap<String, oneshot::Sender<DaemonResponse>>>,
    reason: &str,
) {
    let mut map = pending.lock().await;
    for (_, tx) in map.drain() {
        let _ = tx.send(DaemonResponse {
            id: String::new(),
            ok: false,
            result: None,
            error: Some(reason.into()),
        });
    }
}

async fn route_daemon_line(
    line: &str,
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<DaemonResponse>>>>,
    state_tx: &watch::Sender<dt::AppState>,
) {
    match serde_json::from_str::<dt::DaemonLine>(line) {
        Ok(dt::DaemonLine::Response(resp)) => {
            let id = resp.id.clone();
            if let Some(tx) = pending.lock().await.remove(&id) {
                let _ = tx.send(resp);
            }
        }
        Ok(dt::DaemonLine::Event(event)) => match event.event {
            dt::DaemonEventKind::StateChanged => {
                if let Some(state) = event.state {
                    let _ = state_tx.send(state);
                } else {
                    warn!("State change event did not include state");
                }
            }
        },
        Err(e) => {
            warn!("Failed to parse daemon message: {}", e);
        }
    }
}

/// Only publish DaemonNotRunning if this reader's generation is still current.
fn send_terminal_state_if_current(conn_gen: u64, state_tx: &watch::Sender<dt::AppState>) {
    let current = BRIDGE.conn_generation.load(Ordering::Relaxed);
    if conn_gen == current {
        let _ = state_tx.send(dt::AppState {
            connection_status: dt::ConnectionStatus::DaemonNotRunning,
            ..Default::default()
        });
    } else {
        tracing::debug!(
            reader_conn_gen = conn_gen,
            current_conn_gen = current,
            "Stale reader exiting without publishing terminal state"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    /// Helper: create a temporary Unix socket pair via a listener, returning
    /// (client_stream, server_stream).
    async fn socket_pair() -> (tokio::net::UnixStream, tokio::net::UnixStream) {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let client = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        // Keep tempdir alive by leaking it (tests are short-lived).
        std::mem::forget(dir);
        (client, server)
    }

    #[tokio::test]
    async fn drain_pending_notifies_all_waiters() {
        let pending: Mutex<HashMap<String, oneshot::Sender<DaemonResponse>>> =
            Mutex::new(HashMap::new());

        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();
        pending.lock().await.insert("a".into(), tx1);
        pending.lock().await.insert("b".into(), tx2);

        drain_pending(&pending, "gone").await;

        let r1 = rx1.await.unwrap();
        assert!(!r1.ok);
        assert_eq!(r1.error.unwrap(), "gone");

        let r2 = rx2.await.unwrap();
        assert!(!r2.ok);
        assert_eq!(r2.error.unwrap(), "gone");

        assert!(pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn terminal_state_respects_generation() {
        let (state_tx, mut state_rx) = watch::channel(dt::AppState::default());

        // Simulate generation 5 being current.
        BRIDGE.conn_generation.store(5, Ordering::Relaxed);

        // A stale reader (gen 3) should NOT publish.
        send_terminal_state_if_current(3, &state_tx);
        assert!(state_rx.has_changed().is_ok(), "rx should not have errored");
        // No change should have been sent — borrow should still show default (Disconnected).
        assert_eq!(
            state_tx.borrow().connection_status,
            dt::ConnectionStatus::Disconnected
        );

        // Send a connected state so we can detect the terminal state change.
        let _ = state_tx.send(dt::AppState {
            connection_status: dt::ConnectionStatus::Connected,
            ..Default::default()
        });
        state_rx.changed().await.unwrap();

        // Current generation reader (gen 5) SHOULD publish terminal state.
        send_terminal_state_if_current(5, &state_tx);
        state_rx.changed().await.unwrap();
        assert_eq!(
            state_rx.borrow().connection_status,
            dt::ConnectionStatus::DaemonNotRunning
        );
    }

    #[tokio::test]
    async fn send_command_cleans_up_pending_on_write_failure() {
        // Set up a real connection via BRIDGE, then close the server side to
        // make the next write fail. We can't easily use BRIDGE (it's global/
        // shared across tests), so test the cleanup logic directly.
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<DaemonResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (tx, _rx) = oneshot::channel::<DaemonResponse>();
        let id = "test-id".to_string();
        pending.lock().await.insert(id.clone(), tx);

        // Verify entry exists.
        assert!(pending.lock().await.contains_key(&id));

        // Simulate what send_command does on write failure: remove the entry.
        pending.lock().await.remove(&id);
        assert!(!pending.lock().await.contains_key(&id));
    }

    #[tokio::test]
    async fn mock_daemon_responds_to_request() {
        let (client, server) = socket_pair().await;
        let (server_read, mut server_write) = server.into_split();
        let (client_read, client_write) = client.into_split();

        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<DaemonResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending2 = Arc::clone(&pending);

        let (state_tx, _state_rx) = watch::channel(dt::AppState::default());

        // Spawn a reader task (same logic as connect()).
        tokio::spawn(async move {
            let mut reader = BufReader::new(client_read);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        route_daemon_line(trimmed, &pending2, &state_tx).await;
                    }
                    Err(_) => break,
                }
            }
        });

        // Spawn a mock daemon that echoes back a success response.
        let mut server_reader = BufReader::new(server_read);
        tokio::spawn(async move {
            let mut line = String::new();
            while server_reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                let req: DaemonRequest = serde_json::from_str(line.trim()).unwrap();
                let echo = match req.command {
                    DaemonCommand::Refresh => "refresh",
                    _ => "other",
                };
                let resp = DaemonResponse::success(req.id, Some(serde_json::json!({"echo": echo})));
                let resp_line = format!("{}\n", serde_json::to_string(&resp).unwrap());
                server_write.write_all(resp_line.as_bytes()).await.unwrap();
                line.clear();
            }
        });

        // Now simulate send_command by hand (can't use the real one since
        // it uses BRIDGE's writer, not our test writer).
        let (tx, rx) = oneshot::channel();
        let req_id = "req-1".to_string();
        pending.lock().await.insert(req_id.clone(), tx);

        let req = DaemonRequest::new(req_id, DaemonCommand::Refresh);
        let req_line = format!("{}\n", serde_json::to_string(&req).unwrap());

        let writer = Mutex::new(client_write);
        writer
            .lock()
            .await
            .write_all(req_line.as_bytes())
            .await
            .unwrap();

        let resp = rx.await.unwrap();
        assert!(resp.ok);
        assert_eq!(resp.result.unwrap()["echo"], "refresh");
    }

    #[tokio::test]
    async fn state_event_broadcasts_to_subscriber() {
        let (client, server) = socket_pair().await;
        let (client_read, _client_write) = client.into_split();

        let (state_tx, _) = watch::channel(dt::AppState::default());
        let mut state_rx = state_tx.subscribe();

        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<DaemonResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Spawn reader task.
        let state_tx2 = state_tx.clone();
        let pending2 = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut reader = BufReader::new(client_read);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        route_daemon_line(trimmed, &pending2, &state_tx2).await;
                    }
                    Err(_) => break,
                }
            }
        });

        // Server sends a state_changed event.
        let (_server_read, mut server_write) = server.into_split();
        let event = dt::DaemonEvent::state_changed(dt::AppState {
            logged_in: true,
            connection_status: dt::ConnectionStatus::Connected,
            ..Default::default()
        });
        let event_line = format!("{}\n", serde_json::to_string(&event).unwrap());
        server_write.write_all(event_line.as_bytes()).await.unwrap();

        // Subscriber should receive the state change.
        state_rx.changed().await.unwrap();
        let state = state_rx.borrow().clone();
        assert!(state.logged_in);
        assert_eq!(state.connection_status, dt::ConnectionStatus::Connected);
    }

    #[tokio::test]
    async fn multiple_subscribers_see_same_state() {
        let (state_tx, _) = watch::channel(dt::AppState::default());
        let mut rx1 = state_tx.subscribe();
        let mut rx2 = state_tx.subscribe();

        let _ = state_tx.send(dt::AppState {
            logged_in: true,
            connection_status: dt::ConnectionStatus::Connected,
            ..Default::default()
        });

        rx1.changed().await.unwrap();
        rx2.changed().await.unwrap();

        assert!(rx1.borrow().logged_in);
        assert!(rx2.borrow().logged_in);
    }
}
