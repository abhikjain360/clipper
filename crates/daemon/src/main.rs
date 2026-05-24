//! Clipper daemon: background clipboard sync service.
//!
//! Runs as a macOS LaunchAgent, exposes a Unix socket for app control.

mod clients;
mod error;
mod handler;
mod keychain;
mod protocol;

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use clipper_client::engine::SyncEngine;
use tokio::net::UnixListener;
use tracing::{error, info, warn};

use crate::{
    clients::ClientManager,
    error::{DaemonError, DaemonResult},
    protocol::DaemonEvent,
};

const PRIVATE_DIR_MODE: u32 = 0o700;
const SOCKET_FILE_MODE: u32 = 0o600;

fn app_data_dir() -> DaemonResult<PathBuf> {
    dirs::data_dir()
        .map(|base| base.join("Clipper"))
        .ok_or(DaemonError::DataDirUnavailable)
}

fn socket_path() -> DaemonResult<PathBuf> {
    Ok(app_data_dir()?.join("daemon.sock"))
}

fn data_dir() -> DaemonResult<PathBuf> {
    app_data_dir()
}

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

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        error!(%error, "daemon failed");
        std::process::exit(error.exit_code());
    }
}

async fn run() -> DaemonResult<()> {
    let default_server_url = parse_args();

    // Init logging
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/Logs/Clipper");
    std::fs::create_dir_all(&log_dir).ok();

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("daemon.log"))?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::sync::Mutex::new(log_file))
        .try_init()
        .ok();

    let sock_path = socket_path()?;
    let data_dir = data_dir()?;
    ensure_private_dir(&data_dir)?;

    // Check for existing daemon
    if sock_path.exists() {
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
            info!("Found stored server profile in Keychain");
            Some(creds)
        }
        Ok(None) => None,
        Err(e) => {
            warn!("Failed to load server profile from Keychain: {}", e);
            None
        }
    };
    let server_url = loaded_creds
        .as_ref()
        .map(|creds| creds.server_url.as_str())
        .unwrap_or(default_server_url.as_str());

    let engine = SyncEngine::new_with_data_dir(server_url, data_dir.join("client"));
    if let Some(creds) = loaded_creds {
        engine
            .set_saved_profile(creds.user_id, Some(creds.device_name))
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
        std::fs::Permissions::from_mode(SOCKET_FILE_MODE),
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
                        let engine = Arc::clone(&engine);
                        let client_mgr = Arc::clone(&client_mgr);
                        let (read_half, write_half) = stream.into_split();
                        tokio::spawn(handler::handle_connection(
                            read_half, write_half, engine, client_mgr,
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
