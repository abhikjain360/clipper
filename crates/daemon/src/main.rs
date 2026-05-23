//! Clipper daemon: background clipboard sync service.
//!
//! Runs as a macOS LaunchAgent, exposes a Unix socket for app control.

mod clients;
mod handler;
mod keychain;
mod protocol;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::UnixListener;
use tracing::{error, info, warn};

use clipper_client::engine::SyncEngine;

use crate::clients::ClientManager;
use crate::protocol::DaemonEvent;

fn socket_path() -> PathBuf {
    let base = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Clipper");
    base.join("daemon.sock")
}

fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Clipper")
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
async fn main() -> anyhow::Result<()> {
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

    let sock_path = socket_path();
    let data_dir = data_dir();
    std::fs::create_dir_all(&data_dir)?;

    // Check for existing daemon
    if sock_path.exists() {
        match tokio::net::UnixStream::connect(&sock_path).await {
            Ok(_) => {
                eprintln!("Daemon already running (socket at {})", sock_path.display());
                std::process::exit(0);
            }
            Err(_) => {
                // Stale socket, remove it
                info!("Removing stale socket at {}", sock_path.display());
                std::fs::remove_file(&sock_path).ok();
            }
        }
    }

    // Determine server URL: prefer Keychain credentials, fall back to CLI arg
    let (server_url, auto_login_creds) = match keychain::load_credentials() {
        Ok(Some(creds)) => {
            info!("Found stored credentials in Keychain");
            let url = creds.server_url.clone();
            (url, Some(creds))
        }
        _ => (default_server_url, None),
    };

    let engine = SyncEngine::new(&server_url);

    // Attempt auto-login from Keychain
    if let Some(creds) = auto_login_creds {
        info!("Attempting auto-login from Keychain");
        match engine.login(&creds.passphrase, &creds.device_name).await {
            Ok(()) => {
                info!("Auto-login successful");
            }
            Err(e) => {
                warn!("Auto-login failed: {}. Waiting for app to login.", e);
                // If auth was rejected (not just network), clear the stored creds
                let err_str = e.to_string();
                if err_str.contains("401") || err_str.contains("403") {
                    warn!("Clearing invalid Keychain credentials");
                    keychain::clear_credentials().ok();
                }
            }
        }
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
    info!("Daemon listening on {}", sock_path.display());

    // Handle shutdown
    let sock_path_cleanup = sock_path.clone();
    let shutdown = async {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
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
