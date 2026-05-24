//! Per-connection handler: reads commands, dispatches to SyncEngine, writes responses.

use std::sync::Arc;

use clipper_client::engine::SyncEngine;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::unix::{OwnedReadHalf, OwnedWriteHalf},
    sync::Mutex,
};
use tracing::{debug, warn};

use crate::{
    clients::ClientManager,
    keychain::{self, Credentials},
    protocol::{
        CopyToLocalResult, DaemonCommand, DaemonEvent, DaemonRequest, DaemonResponse, LoginParams,
        RegisterParams, RegisterResult, UploadFileResult,
    },
};

const MAX_IPC_REQUEST_LINE_BYTES: usize = 4 * 1024 * 1024;

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
            let mut line = Vec::new();
            loop {
                match read_limited_line(&mut reader, &mut line).await {
                    Ok(None) => break,
                    Ok(Some(trimmed)) => {
                        if trimmed.is_empty() {
                            continue;
                        }
                        let response = match serde_json::from_str::<DaemonRequest>(&trimmed) {
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
                    Err(RequestLineError::TooLong) => {
                        warn!(client_id, max_bytes = MAX_IPC_REQUEST_LINE_BYTES, "IPC request line too large");
                        let response = DaemonResponse::error(
                            String::new(),
                            "Request line too large".into(),
                        );
                        if let Ok(json) = serde_json::to_string(&response) {
                            let mut w = writer.lock().await;
                            let resp_line = format!("{}\n", json);
                            let _ = w.write_all(resp_line.as_bytes()).await;
                        }
                        break;
                    }
                    Err(RequestLineError::Utf8) => {
                        let response = DaemonResponse::error(
                            String::new(),
                            "Invalid request: request line is not UTF-8".into(),
                        );
                        if let Ok(json) = serde_json::to_string(&response) {
                            let mut w = writer.lock().await;
                            let resp_line = format!("{}\n", json);
                            if w.write_all(resp_line.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(RequestLineError::Io(e)) => {
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

#[derive(Debug, thiserror::Error)]
enum RequestLineError {
    #[error("request line exceeds maximum size")]
    TooLong,
    #[error("request line is not UTF-8")]
    Utf8,
    #[error("request line read failed: {0}")]
    Io(#[from] std::io::Error),
}

async fn read_limited_line<R>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> Result<Option<String>, RequestLineError>
where
    R: AsyncBufRead + Unpin,
{
    line.clear();

    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            break;
        }

        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |pos| pos + 1);
        if line.len() + take > MAX_IPC_REQUEST_LINE_BYTES {
            return Err(RequestLineError::TooLong);
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);

        if line.ends_with(b"\n") {
            break;
        }
    }

    let text = std::str::from_utf8(line).map_err(|_| RequestLineError::Utf8)?;
    Ok(Some(text.trim().to_string()))
}

async fn dispatch_command(req: DaemonRequest, engine: &Arc<SyncEngine>) -> DaemonResponse {
    let id = req.id.clone();
    match req.command {
        DaemonCommand::Login(params) => cmd_login(id, params, engine).await,
        DaemonCommand::Register(params) => cmd_register(id, params, engine).await,
        DaemonCommand::Logout => cmd_logout(id, engine).await,
        DaemonCommand::GetState => cmd_get_state(id, engine).await,
        DaemonCommand::SendClipboard(params) => cmd_send_clipboard(id, params.text, engine).await,
        DaemonCommand::CopyToLocal(params) => cmd_copy_to_local(id, params.item_id, engine).await,
        DaemonCommand::UploadFile(params) => cmd_upload_file(id, params.file_path, engine).await,
        DaemonCommand::DownloadFile(params) => {
            cmd_download_file(id, params.file_id, params.target_path, engine).await
        }
        DaemonCommand::DeleteFile(params) => cmd_delete_file(id, params.file_id, engine).await,
        DaemonCommand::Refresh => cmd_refresh(id, engine).await,
    }
}

async fn cmd_login(id: String, params: LoginParams, engine: &Arc<SyncEngine>) -> DaemonResponse {
    let device_name = params.device_name.as_deref().unwrap_or("macOS");

    // If server_url is provided, reconfigure the engine
    if let Some(url) = params.server_url.as_deref() {
        engine.set_base_url(url).await;
    }

    match engine
        .login_with_platform_and_user(
            &params.passphrase,
            params.user_id.as_deref(),
            device_name,
            "macos",
        )
        .await
    {
        Ok(()) => {
            // Store credentials in Keychain
            let url = engine.base_url().await;
            let state = engine.get_state().await;
            let creds = Credentials {
                device_name: device_name.to_string(),
                server_url: url,
                user_id: state.user_id,
            };
            if let Err(e) = keychain::store_credentials(&creds) {
                warn!("Failed to store server profile in Keychain: {}", e);
            }
            DaemonResponse::success(id, None)
        }
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_register(
    id: String,
    params: RegisterParams,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    let device_name = params.device_name.as_deref().unwrap_or("macOS");

    if let Some(url) = params.server_url.as_deref() {
        engine.set_base_url(url).await;
    }

    match engine
        .register(&params.access_key, &params.passphrase, device_name)
        .await
    {
        Ok(user_id) => {
            let url = engine.base_url().await;
            let creds = Credentials {
                device_name: device_name.to_string(),
                server_url: url,
                user_id: Some(user_id.clone()),
            };
            if let Err(e) = keychain::store_credentials(&creds) {
                warn!("Failed to store server profile in Keychain: {}", e);
            }
            json_success(id, RegisterResult { user_id })
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
    json_success(id, state)
}

async fn cmd_send_clipboard(id: String, text: String, engine: &Arc<SyncEngine>) -> DaemonResponse {
    match engine.send_clipboard(&text).await {
        Ok(()) => DaemonResponse::success(id, None),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_copy_to_local(
    id: String,
    item_id: String,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    match engine.copy_to_local(&item_id).await {
        Ok(text) => json_success(id, CopyToLocalResult { text }),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_upload_file(
    id: String,
    file_path: String,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    match engine.upload_file(&file_path).await {
        Ok(file_id) => json_success(id, UploadFileResult { file_id }),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_download_file(
    id: String,
    file_id: String,
    target_path: String,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    match engine.download_file(&file_id, &target_path).await {
        Ok(()) => DaemonResponse::success(id, None),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

async fn cmd_delete_file(id: String, file_id: String, engine: &Arc<SyncEngine>) -> DaemonResponse {
    match engine.delete_file(&file_id).await {
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

fn json_success<T: serde::Serialize>(id: String, value: T) -> DaemonResponse {
    match serde_json::to_value(value) {
        Ok(value) => DaemonResponse::success(id, Some(value)),
        Err(e) => DaemonResponse::error(id, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_limited_line_accepts_normal_line() {
        let mut reader = BufReader::new(
            br#"{"id":"1","cmd":"refresh"}
"# as &[u8],
        );
        let mut line = Vec::new();

        let line = read_limited_line(&mut reader, &mut line)
            .await
            .expect("read line")
            .expect("line");

        assert_eq!(line, r#"{"id":"1","cmd":"refresh"}"#);
    }

    #[tokio::test]
    async fn read_limited_line_rejects_oversized_line() {
        let mut input = vec![b'a'; MAX_IPC_REQUEST_LINE_BYTES + 1];
        input.push(b'\n');
        let mut reader = BufReader::new(input.as_slice());
        let mut line = Vec::new();

        let error = read_limited_line(&mut reader, &mut line)
            .await
            .expect_err("oversized line should fail");

        assert!(matches!(error, RequestLineError::TooLong));
    }
}
