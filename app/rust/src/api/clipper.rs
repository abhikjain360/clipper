//! FRB API surface — Unix socket client to the clipper-daemon.
//! FRB codegen processes this file to generate Dart bindings.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use clipper_daemon_types as dt;
use clipper_daemon_types::{DaemonRequest, DaemonResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Mutex, RwLock, oneshot, watch};
use tracing::warn;

// ── FRB-facing types (thin wrappers for Dart codegen) ──

#[derive(Clone, Default)]
pub struct BridgeClipboardItem {
    pub id: String,
    pub text: String,
    pub created_at: String,
    pub source_device_id: String,
}

#[derive(Clone, Default)]
pub struct BridgeFileItem {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub blob_size: i64,
    pub created_at: String,
    pub source_device_id: String,
}

#[derive(Clone, Default)]
pub struct BridgeAppState {
    pub logged_in: bool,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub connection_status: String,
    pub clipboard_items: Vec<BridgeClipboardItem>,
    pub files: Vec<BridgeFileItem>,
    pub error: Option<String>,
}

impl From<dt::AppState> for BridgeAppState {
    fn from(s: dt::AppState) -> Self {
        Self {
            logged_in: s.logged_in,
            device_id: s.device_id,
            device_name: s.device_name,
            connection_status: match s.connection_status {
                dt::ConnectionStatus::Disconnected => "disconnected".into(),
                dt::ConnectionStatus::Connecting => "connecting".into(),
                dt::ConnectionStatus::Connected => "connected".into(),
                dt::ConnectionStatus::DaemonNotRunning => "daemon_not_running".into(),
            },
            clipboard_items: s.clipboard_items.into_iter().map(Into::into).collect(),
            files: s.files.into_iter().map(Into::into).collect(),
            error: s.error,
        }
    }
}

impl From<dt::DecryptedClipboardItem> for BridgeClipboardItem {
    fn from(i: dt::DecryptedClipboardItem) -> Self {
        Self {
            id: i.id,
            text: i.text,
            created_at: i.created_at,
            source_device_id: i.source_device_id,
        }
    }
}

impl From<dt::DecryptedFileItem> for BridgeFileItem {
    fn from(f: dt::DecryptedFileItem) -> Self {
        Self {
            id: f.id,
            filename: f.filename,
            mime_type: f.mime_type,
            blob_size: f.blob_size,
            created_at: f.created_at,
            source_device_id: f.source_device_id,
        }
    }
}

/// An event or response line from the daemon.
#[derive(serde::Deserialize)]
struct DaemonMessage {
    id: Option<String>,
    ok: Option<bool>,
    result: Option<serde_json::Value>,
    error: Option<String>,
    event: Option<String>,
    state: Option<serde_json::Value>,
}

// ── Connection ──

struct DaemonConnection {
    writer: Mutex<OwnedWriteHalf>,
    pending: Mutex<HashMap<String, oneshot::Sender<DaemonResponse>>>,
    state_tx: watch::Sender<dt::AppState>,
}

static CONN: OnceLock<DaemonConnection> = OnceLock::new();
static STATE_RX: OnceLock<RwLock<watch::Receiver<dt::AppState>>> = OnceLock::new();

fn socket_path() -> PathBuf {
    let base = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Clipper");
    base.join("daemon.sock")
}

fn conn() -> &'static DaemonConnection {
    CONN.get()
        .expect("Not connected to daemon. Call connect_to_daemon first.")
}

async fn send_request(
    cmd: &str,
    params: Option<serde_json::Value>,
) -> anyhow::Result<Option<serde_json::Value>> {
    let c = conn();
    let id = uuid::Uuid::new_v4().to_string();
    let req = DaemonRequest {
        id: id.clone(),
        cmd: cmd.into(),
        params: params.unwrap_or(serde_json::Value::Null),
    };

    let (tx, rx) = oneshot::channel();
    c.pending.lock().await.insert(id.clone(), tx);

    let json = serde_json::to_string(&req)?;
    let line = format!("{}\n", json);
    c.writer.lock().await.write_all(line.as_bytes()).await?;

    let resp = rx
        .await
        .map_err(|_| anyhow::anyhow!("Daemon connection lost"))?;
    if resp.ok {
        Ok(resp.result)
    } else {
        Err(anyhow::anyhow!(
            "{}",
            resp.error.unwrap_or_else(|| "Unknown error".into())
        ))
    }
}

// ── Daemon lifecycle ──

const LAUNCHAGENT_LABEL: &str = "com.clipper.daemon";

/// Find the daemon binary path inside the app bundle.
/// The binary is at <app_bundle>/Contents/MacOS/clipper-daemon
fn daemon_binary_path() -> Option<PathBuf> {
    // current_exe gives us <bundle>/Contents/MacOS/clipper_app
    let exe = std::env::current_exe().ok()?;
    let macos_dir = exe.parent()?;
    let daemon = macos_dir.join("clipper-daemon");
    if daemon.exists() { Some(daemon) } else { None }
}

fn launchagent_plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents/com.clipper.daemon.plist")
}

fn generate_plist(daemon_path: &std::path::Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{daemon}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>/tmp/clipper-daemon.stdout.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/clipper-daemon.stderr.log</string>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>"#,
        label = LAUNCHAGENT_LABEL,
        daemon = daemon_path.display(),
    )
}

/// Ensure the daemon is running. Installs/updates the LaunchAgent if needed.
fn install_and_start_daemon() -> anyhow::Result<()> {
    let daemon_path = daemon_binary_path()
        .ok_or_else(|| anyhow::anyhow!("clipper-daemon not found in app bundle"))?;

    let plist_path = launchagent_plist_path();
    let new_plist = generate_plist(&daemon_path);

    // Check if plist needs updating
    let needs_install = if plist_path.exists() {
        let existing = std::fs::read_to_string(&plist_path).unwrap_or_default();
        existing != new_plist
    } else {
        true
    };

    if needs_install {
        // Unload old agent if it exists
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &plist_path.to_string_lossy()])
            .output();

        // Write new plist
        if let Some(parent) = plist_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&plist_path, &new_plist)?;
        tracing::info!("Installed LaunchAgent plist at {}", plist_path.display());

        // Load the agent
        let output = std::process::Command::new("launchctl")
            .args(["load", &plist_path.to_string_lossy()])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("launchctl load warning: {}", stderr);
        }
    } else {
        // Plist is current. Check if daemon is running by trying the socket.
        let sock = socket_path();
        if !sock.exists() {
            // Socket doesn't exist, try to start the agent
            let output = std::process::Command::new("launchctl")
                .args(["start", LAUNCHAGENT_LABEL])
                .output()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!("launchctl start warning: {}", stderr);
            }
        }
    }

    Ok(())
}

// ── Bridge Functions ──

#[flutter_rust_bridge::frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

pub async fn connect_to_daemon() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .ok();

    // Ensure daemon is running before connecting
    if let Err(e) = install_and_start_daemon() {
        tracing::warn!("Failed to start daemon: {}", e);
        // Continue anyway — maybe daemon is already running externally
    }

    // Give the daemon a moment to start and create the socket
    let sock = socket_path();
    for _ in 0..10 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    let stream = tokio::net::UnixStream::connect(&sock)
        .await
        .map_err(|e| anyhow::anyhow!("Cannot connect to daemon at {}: {}", sock.display(), e))?;

    let (read_half, write_half) = stream.into_split();

    let default_state = dt::AppState::default();
    let (state_tx, state_rx) = watch::channel(default_state);

    let connection = DaemonConnection {
        writer: Mutex::new(write_half),
        pending: Mutex::new(HashMap::new()),
        state_tx,
    };

    if CONN.set(connection).is_err() {
        // Already connected
        return Ok(());
    }
    STATE_RX.set(RwLock::new(state_rx)).ok();

    // Spawn reader task
    let conn_ref = conn();
    let state_tx = conn_ref.state_tx.clone();
    // We need to move the reader into a spawned task.
    // Since CONN is 'static via OnceLock, we can access it from the task.
    tokio::spawn(async move {
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    // EOF — daemon disconnected
                    warn!("Daemon connection lost (EOF)");
                    let _ = state_tx.send(dt::AppState {
                        connection_status: dt::ConnectionStatus::DaemonNotRunning,
                        ..Default::default()
                    });
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<DaemonMessage>(trimmed) {
                        Ok(msg) => {
                            if let Some(id) = msg.id {
                                // It's a response
                                let resp = DaemonResponse {
                                    id: id.clone(),
                                    ok: msg.ok.unwrap_or(false),
                                    result: msg.result,
                                    error: msg.error,
                                };
                                let c = CONN.get().unwrap();
                                if let Some(tx) = c.pending.lock().await.remove(&id) {
                                    let _ = tx.send(resp);
                                }
                            } else if msg.event.as_deref() == Some("state_changed") {
                                // It's a state event
                                if let Some(state_val) = msg.state {
                                    match serde_json::from_value::<dt::AppState>(state_val) {
                                        Ok(state) => { let _ = state_tx.send(state); }
                                        Err(e) => { warn!("Failed to parse state event: {}", e); }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to parse daemon message: {}", e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Daemon read error: {}", e);
                    let _ = state_tx.send(dt::AppState {
                        connection_status: dt::ConnectionStatus::DaemonNotRunning,
                        ..Default::default()
                    });
                    break;
                }
            }
        }
    });

    Ok(())
}

pub async fn login(
    passphrase: String,
    device_name: String,
    server_url: String,
) -> anyhow::Result<()> {
    send_request(
        "login",
        Some(serde_json::json!({
            "passphrase": passphrase,
            "device_name": device_name,
            "server_url": server_url,
        })),
    )
    .await?;
    Ok(())
}

pub async fn logout() -> anyhow::Result<()> {
    send_request("logout", None).await?;
    Ok(())
}

pub async fn get_state() -> BridgeAppState {
    match STATE_RX.get() {
        Some(rx) => rx.read().await.borrow().clone().into(),
        None => dt::AppState {
            connection_status: dt::ConnectionStatus::DaemonNotRunning,
            ..Default::default()
        }
        .into(),
    }
}

pub async fn send_clipboard(text: String) -> anyhow::Result<()> {
    send_request("send_clipboard", Some(serde_json::json!({ "text": text }))).await?;
    Ok(())
}

pub async fn copy_to_local(id: String) -> anyhow::Result<String> {
    let result = send_request("copy_to_local", Some(serde_json::json!({ "item_id": id }))).await?;
    let text = result
        .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(String::from))
        .ok_or_else(|| anyhow::anyhow!("No text in response"))?;
    Ok(text)
}

pub async fn upload_file(file_path: String) -> anyhow::Result<String> {
    let result = send_request(
        "upload_file",
        Some(serde_json::json!({ "file_path": file_path })),
    )
    .await?;
    let file_id = result
        .and_then(|v| v.get("file_id").and_then(|f| f.as_str()).map(String::from))
        .ok_or_else(|| anyhow::anyhow!("No file_id in response"))?;
    Ok(file_id)
}

pub async fn download_file(file_id: String, target_path: String) -> anyhow::Result<()> {
    send_request(
        "download_file",
        Some(serde_json::json!({ "file_id": file_id, "target_path": target_path })),
    )
    .await?;
    Ok(())
}

pub async fn delete_file(file_id: String) -> anyhow::Result<()> {
    send_request(
        "delete_file",
        Some(serde_json::json!({ "file_id": file_id })),
    )
    .await?;
    Ok(())
}

pub async fn refresh() -> anyhow::Result<()> {
    send_request("refresh", None).await?;
    Ok(())
}

pub async fn wait_for_state_change() {
    if let Some(rx_lock) = STATE_RX.get() {
        let mut rx = rx_lock.write().await;
        let _ = rx.changed().await;
    }
}
