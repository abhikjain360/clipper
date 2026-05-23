//! Per-connection handler: reads commands, dispatches to SyncEngine, writes responses.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use clipper_client::engine::SyncEngine;

use crate::clients::ClientManager;
use crate::keychain::{self, Credentials};
use crate::protocol::{DaemonEvent, DaemonRequest, DaemonResponse};

/// Handle a single client connection.
pub async fn handle_connection(
    read_half: OwnedReadHalf,
    write_half: OwnedWriteHalf,
    engine: Arc<SyncEngine>,
    client_mgr: Arc<ClientManager>,
) {
    let (client_id, mut broadcast_rx) = client_mgr.register().await;
    let writer = Arc::new(Mutex::new(write_half));

    // Send initial state
    let state = engine.get_state().await;
    let event = DaemonEvent::state_changed(state);
    if let Ok(json) = serde_json::to_string(&event) {
        let mut w = writer.lock().await;
        let line = format!("{}\n", json);
        if w.write_all(line.as_bytes()).await.is_err() {
            client_mgr.unregister(client_id).await;
            return;
        }
    }

    let mut reader = BufReader::new(read_half);

    let writer_for_broadcast = Arc::clone(&writer);

    // Run read loop and broadcast loop concurrently
    tokio::select! {
        _ = async {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let response = match serde_json::from_str::<DaemonRequest>(trimmed) {
                            Ok(req) => dispatch_command(req, &engine).await,
                            Err(e) => DaemonResponse::error(
                                String::new(),
                                format!("Invalid request: {}", e),
                            ),
                        };
                        if let Ok(json) = serde_json::to_string(&response) {
                            let mut w = writer.lock().await;
                            let resp_line = format!("{}\n", json);
                            if w.write_all(resp_line.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(client_id, "Read error: {}", e);
                        break;
                    }
                }
            }
        } => {}
        _ = async {
            while let Some(event_line) = broadcast_rx.recv().await {
                let mut w = writer_for_broadcast.lock().await;
                let line = format!("{}\n", event_line);
                if w.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        } => {}
    }

    client_mgr.unregister(client_id).await;
    debug!(client_id, "Client disconnected");
}

async fn dispatch_command(req: DaemonRequest, engine: &Arc<SyncEngine>) -> DaemonResponse {
    let id = req.id.clone();
    match req.cmd.as_str() {
        "login" => cmd_login(id, &req.params, engine).await,
        "logout" => cmd_logout(id, engine).await,
        "get_state" => cmd_get_state(id, engine).await,
        "send_clipboard" => cmd_send_clipboard(id, &req.params, engine).await,
        "copy_to_local" => cmd_copy_to_local(id, &req.params, engine).await,
        "upload_file" => cmd_upload_file(id, &req.params, engine).await,
        "download_file" => cmd_download_file(id, &req.params, engine).await,
        "delete_file" => cmd_delete_file(id, &req.params, engine).await,
        "refresh" => cmd_refresh(id, engine).await,
        _ => DaemonResponse::error(id, format!("Unknown command: {}", req.cmd)),
    }
}

async fn cmd_login(
    id: String,
    params: &serde_json::Value,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    let passphrase = match params.get("passphrase").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return DaemonResponse::error(id, "Missing passphrase".into()),
    };
    let device_name = params
        .get("device_name")
        .and_then(|v| v.as_str())
        .unwrap_or("macOS");
    let server_url = params.get("server_url").and_then(|v| v.as_str());

    // If server_url is provided, reconfigure the engine
    if let Some(url) = server_url {
        engine.set_base_url(url).await;
    }

    match engine.login(passphrase, device_name).await {
        Ok(()) => {
            // Store credentials in Keychain
            let url = engine.base_url().await;
            let creds = Credentials {
                device_name: device_name.to_string(),
                server_url: url,
            };
            if let Err(e) = keychain::store_credentials(&creds) {
                warn!("Failed to store server profile in Keychain: {}", e);
            }
            DaemonResponse::success(id, None)
        }
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_logout(id: String, engine: &Arc<SyncEngine>) -> DaemonResponse {
    match engine.logout().await {
        Ok(()) => {
            if let Err(e) = keychain::clear_credentials() {
                warn!("Failed to clear Keychain: {}", e);
            }
            DaemonResponse::success(id, None)
        }
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_get_state(id: String, engine: &Arc<SyncEngine>) -> DaemonResponse {
    let state = engine.get_state().await;
    match serde_json::to_value(state) {
        Ok(v) => DaemonResponse::success(id, Some(v)),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_send_clipboard(
    id: String,
    params: &serde_json::Value,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    let text = match params.get("text").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return DaemonResponse::error(id, "Missing text".into()),
    };
    match engine.send_clipboard(text).await {
        Ok(()) => DaemonResponse::success(id, None),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_copy_to_local(
    id: String,
    params: &serde_json::Value,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    let item_id = match params.get("item_id").and_then(|v| v.as_str()) {
        Some(i) => i,
        None => return DaemonResponse::error(id, "Missing item_id".into()),
    };
    match engine.copy_to_local(item_id).await {
        Ok(text) => DaemonResponse::success(id, Some(serde_json::json!({ "text": text }))),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_upload_file(
    id: String,
    params: &serde_json::Value,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    let file_path = match params.get("file_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return DaemonResponse::error(id, "Missing file_path".into()),
    };
    match engine.upload_file(file_path).await {
        Ok(file_id) => DaemonResponse::success(id, Some(serde_json::json!({ "file_id": file_id }))),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_download_file(
    id: String,
    params: &serde_json::Value,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    let file_id = match params.get("file_id").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => return DaemonResponse::error(id, "Missing file_id".into()),
    };
    let target_path = match params.get("target_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return DaemonResponse::error(id, "Missing target_path".into()),
    };
    match engine.download_file(file_id, target_path).await {
        Ok(()) => DaemonResponse::success(id, None),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_delete_file(
    id: String,
    params: &serde_json::Value,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    let file_id = match params.get("file_id").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => return DaemonResponse::error(id, "Missing file_id".into()),
    };
    match engine.delete_file(file_id).await {
        Ok(()) => DaemonResponse::success(id, None),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_refresh(id: String, engine: &Arc<SyncEngine>) -> DaemonResponse {
    match engine.refresh().await {
        Ok(()) => DaemonResponse::success(id, None),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}
