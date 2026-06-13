//! Clipper daemon: background clipboard sync service.
//!
//! Runs as a per-user background service and exposes a Unix socket for app
//! control.

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() {
    std::process::exit(1);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod clients;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod error;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod handler;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod keychain;
#[cfg(target_os = "linux")]
mod platform_clipboard;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod protocol;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::{
    io,
    os::fd::AsRawFd,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use clipper_client::engine::SyncEngine;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use clipper_daemon_types::ipc_path;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use tokio::net::{UnixListener, UnixStream};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use tracing::{error, info, warn};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::{
    clients::ClientManager,
    error::{DaemonError, DaemonResult},
    protocol::DaemonEvent,
};

#[cfg(any(target_os = "macos", target_os = "linux"))]
const PRIVATE_DIR_MODE: u32 = 0o700;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const PRIVATE_FILE_MODE: u32 = 0o600;
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn app_data_dir() -> DaemonResult<PathBuf> {
    dirs::data_dir()
        .map(|base| base.join("Clipper"))
        .ok_or(DaemonError::DataDirUnavailable)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn data_dir() -> DaemonResult<PathBuf> {
    app_data_dir()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a directory", path.display()),
        ));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn log_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/Logs/Clipper")
}

#[cfg(target_os = "linux")]
fn log_dir() -> PathBuf {
    dirs::cache_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("clipper")
        .join("logs")
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn parse_args() -> String {
    let args: Vec<String> = std::env::args().collect();
    let mut server_url = "http://127.0.0.1:8787".to_string();

    let mut i = 1;
    while i < args.len() {
        if args[i] == "--server-url" && i + 1 < args.len() {
            server_url = args[i + 1].clone();
            i += 2;
        } else {
            i += 1;
        }
    }
    server_url
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn current_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() as u32 }
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut credentials_len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: credentials points to writable memory, credentials_len is the
    // correct buffer size, and stream.as_raw_fd() is a live Unix socket fd.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut credentials_len,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if credentials_len < std::mem::size_of::<libc::ucred>() as libc::socklen_t {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short SO_PEERCRED response",
        ));
    }

    // SAFETY: getsockopt succeeded and reported a full ucred structure.
    let credentials = unsafe { credentials.assume_init() };
    Ok(credentials.uid)
}

#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut uid = 0 as libc::uid_t;
    let mut gid = 0 as libc::gid_t;
    // SAFETY: getpeereid writes the peer uid/gid for a live connected Unix
    // stream socket and does not retain the pointers.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn peer_uid_matches_current_user(stream: &UnixStream) -> io::Result<bool> {
    let peer_uid = peer_uid(stream)?;
    let expected_uid = current_euid();
    if peer_uid != expected_uid {
        warn!(
            peer_uid,
            expected_uid, "Rejected IPC client with unexpected uid"
        );
        return Ok(false);
    }
    Ok(true)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        error!(%error, "daemon failed");
        std::process::exit(error.exit_code());
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn run() -> DaemonResult<()> {
    let default_server_url = parse_args();

    let log_dir = log_dir();
    ensure_private_dir(&log_dir)?;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(PRIVATE_FILE_MODE)
        .open(log_dir.join("daemon.log"))?;
    log_file.set_permissions(std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::sync::Mutex::new(log_file))
        .try_init()
        .ok();

    let sock_path = ipc_path::socket_path();
    let data_dir = data_dir()?;
    ensure_private_dir(&data_dir)?;
    if let Some(sock_dir) = sock_path.parent() {
        ipc_path::ensure_private_socket_dir(sock_dir)?;
    }

    // Check for existing daemon
    if sock_path.exists() {
        ipc_path::validate_socket_file(&sock_path)?;
        match tokio::net::UnixStream::connect(&sock_path).await {
            Ok(_) => {
                info!("Daemon already running (socket at {})", sock_path.display());
                std::process::exit(0);
            }
            Err(_) => {
                // Stale socket, remove it
                info!("Removing stale socket at {}", sock_path.display());
                std::fs::remove_file(&sock_path).ok();
            }
        }
    }

    // Determine server URL: prefer stored profile, fall back to CLI arg.
    // The passphrase is intentionally not persisted, so the daemon waits for
    // the app to provide it after startup.
    let loaded_creds = match keychain::load_credentials() {
        Ok(Some(creds)) => {
            info!("Found stored server profile");
            Some(creds)
        }
        Ok(None) => None,
        Err(e) => {
            warn!("Failed to load stored server profile: {}", e);
            None
        }
    };
    let server_url = loaded_creds
        .as_ref()
        .map(|creds| creds.server_url.as_str())
        .unwrap_or(default_server_url.as_str());

    let engine = SyncEngine::try_new_with_data_dir(server_url, data_dir.join("client"))?;
    if let Some(creds) = loaded_creds {
        engine
            .set_saved_profile(Some(creds.username), Some(creds.device_name))
            .await;
    }

    let client_mgr = Arc::new(ClientManager::new());

    // Spawn state watcher that broadcasts to all clients
    {
        let engine = Arc::clone(&engine);
        let client_mgr = Arc::clone(&client_mgr);
        let mut rx = engine.subscribe();
        tokio::spawn(async move {
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let state = engine.get_state().await;
                let event = DaemonEvent::state_changed(state);
                if let Ok(json) = serde_json::to_string(&event) {
                    client_mgr.broadcast(&json).await;
                }
            }
        });
    }

    // Bind Unix socket
    let listener = UnixListener::bind(&sock_path)?;
    std::fs::set_permissions(
        &sock_path,
        std::fs::Permissions::from_mode(ipc_path::SOCKET_FILE_MODE),
    )?;
    info!("Daemon listening on {}", sock_path.display());

    // Handle shutdown
    let sock_path_cleanup = sock_path.clone();
    let shutdown = async {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to register SIGTERM handler");
        let sigint = tokio::signal::ctrl_c();

        tokio::select! {
            _ = sigterm.recv() => info!("Received SIGTERM"),
            _ = sigint => info!("Received SIGINT"),
        }
    };

    // Accept loop with graceful shutdown
    tokio::select! {
        _ = async {
            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        match peer_uid_matches_current_user(&stream) {
                            Ok(true) => {}
                            Ok(false) => continue,
                            Err(error) => {
                                warn!(%error, "Rejected IPC client with unavailable peer uid");
                                continue;
                            }
                        }
                        let engine = Arc::clone(&engine);
                        let client_mgr = Arc::clone(&client_mgr);
                        let data_dir = data_dir.clone();
                        let (read_half, write_half) = stream.into_split();
                        tokio::spawn(handler::handle_connection(
                            read_half, write_half, engine, client_mgr, data_dir,
                        ));
                    }
                    Err(e) => {
                        error!("Accept error: {}", e);
                    }
                }
            }
        } => {}
        _ = shutdown => {
            info!("Shutting down daemon");
        }
    }

    // Cleanup
    std::fs::remove_file(&sock_path_cleanup).ok();
    info!("Daemon stopped");

    Ok(())
}
