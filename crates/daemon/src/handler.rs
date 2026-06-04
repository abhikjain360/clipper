//! Per-connection handler: reads commands, dispatches to SyncEngine, writes responses.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use clipper_client::{
    api_client::ClientError,
    engine::{SyncEngine, TEXT_CLIPBOARD_MIME_TYPE},
};
use hmac::{Hmac, Mac};
use rand::RngExt;
use sha2::Sha256;
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
        AuthChallenge, ClipboardPayloadResult, CopyToLocalResult, DaemonCommand, DaemonEvent,
        DaemonRequest, DaemonResponse, IPC_AUTH_NONCE_BYTES, IPC_AUTH_TAG_BYTES, IPC_AUTH_VERSION,
        LoginParams, RegisterParams, RegisterResult, UploadFileResult, ipc_auth_message,
    },
};

const MAX_IPC_REQUEST_LINE_BYTES: usize = 32 * 1024 * 1024;

type HmacSha256 = Hmac<Sha256>;

/// Handle a single client connection.
pub async fn handle_connection(
    read_half: OwnedReadHalf,
    write_half: OwnedWriteHalf,
    engine: Arc<SyncEngine>,
    client_mgr: Arc<ClientManager>,
    data_dir: PathBuf,
) {
    let writer = Arc::new(Mutex::new(write_half));
    let mut reader = BufReader::new(read_half);

    if !authenticate_connection(&mut reader, &writer, &data_dir).await {
        return;
    }

    let (client_id, mut broadcast_rx) = client_mgr.register().await;

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
                            Err(e) => DaemonResponse::error_message(
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
                        let response = DaemonResponse::error_message(
                            String::new(),
                            "Request line too large",
                        );
                        if let Ok(json) = serde_json::to_string(&response) {
                            let mut w = writer.lock().await;
                            let resp_line = format!("{}\n", json);
                            _ = w.write_all(resp_line.as_bytes()).await;
                        }
                        break;
                    }
                    Err(RequestLineError::Utf8) => {
                        let response = DaemonResponse::error_message(
                            String::new(),
                            "Invalid request: request line is not UTF-8",
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

async fn authenticate_connection(
    reader: &mut BufReader<OwnedReadHalf>,
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    data_dir: &Path,
) -> bool {
    let secret = match keychain::load_or_create_ipc_secret(data_dir) {
        Ok(secret) => secret,
        Err(e) => {
            warn!("Failed to load IPC secret: {}", e);
            return false;
        }
    };

    let daemon_nonce = random_bytes::<IPC_AUTH_NONCE_BYTES>().to_vec();
    let challenge = DaemonEvent::auth_challenge(AuthChallenge {
        protocol_version: IPC_AUTH_VERSION,
        daemon_nonce: daemon_nonce.clone(),
    });
    if let Ok(json) = serde_json::to_string(&challenge) {
        let mut w = writer.lock().await;
        let line = format!("{}\n", json);
        if w.write_all(line.as_bytes()).await.is_err() {
            return false;
        }
    } else {
        return false;
    }

    let mut line = Vec::new();
    loop {
        let trimmed = match read_limited_line(reader, &mut line).await {
            Ok(Some(trimmed)) => trimmed,
            Ok(None) => return false,
            Err(RequestLineError::TooLong) => {
                let response =
                    DaemonResponse::error_message(String::new(), "Request line too large");
                _ = write_response(writer, response).await;
                return false;
            }
            Err(RequestLineError::Utf8) => {
                let response = DaemonResponse::error_message(
                    String::new(),
                    "Invalid request: request line is not UTF-8",
                );
                _ = write_response(writer, response).await;
                return false;
            }
            Err(RequestLineError::Io(e)) => {
                warn!("IPC auth read error: {}", e);
                return false;
            }
        };
        if trimmed.is_empty() {
            continue;
        }

        match verify_auth_request(&trimmed, &secret, &daemon_nonce) {
            Ok(id) => {
                let response = DaemonResponse::success(id, None);
                return write_response(writer, response).await;
            }
            Err(AuthRequestError { id, message }) => {
                let response = DaemonResponse::error_message(id.unwrap_or_default(), message);
                _ = write_response(writer, response).await;
                return false;
            }
        }
    }
}

struct AuthRequestError {
    id: Option<String>,
    message: String,
}

fn verify_auth_request(
    line: &str,
    secret: &[u8],
    daemon_nonce: &[u8],
) -> Result<String, AuthRequestError> {
    let req = serde_json::from_str::<DaemonRequest>(line).map_err(|e| AuthRequestError {
        id: None,
        message: format!("Invalid auth request: {}", e),
    })?;
    let id = req.id.clone();

    let DaemonCommand::Authenticate(params) = req.command else {
        return Err(AuthRequestError {
            id: Some(id),
            message: "IPC authentication required".into(),
        });
    };

    if params.protocol_version != IPC_AUTH_VERSION {
        return Err(AuthRequestError {
            id: Some(id),
            message: "Unsupported IPC authentication version".into(),
        });
    }
    if params.client_nonce.len() != IPC_AUTH_NONCE_BYTES {
        return Err(AuthRequestError {
            id: Some(id),
            message: "Invalid IPC authentication nonce".into(),
        });
    }
    if params.tag.len() != IPC_AUTH_TAG_BYTES {
        return Err(AuthRequestError {
            id: Some(id),
            message: "Invalid IPC authentication tag".into(),
        });
    }

    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| AuthRequestError {
        id: Some(id.clone()),
        message: "Invalid IPC secret".into(),
    })?;
    mac.update(&ipc_auth_message(daemon_nonce, &params.client_nonce));
    mac.verify_slice(&params.tag)
        .map_err(|_| AuthRequestError {
            id: Some(id.clone()),
            message: "IPC authentication failed".into(),
        })?;

    Ok(id)
}

async fn write_response(writer: &Arc<Mutex<OwnedWriteHalf>>, response: DaemonResponse) -> bool {
    let Ok(json) = serde_json::to_string(&response) else {
        return false;
    };
    let mut w = writer.lock().await;
    let line = format!("{}\n", json);
    w.write_all(line.as_bytes()).await.is_ok()
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
        DaemonCommand::Authenticate(_) => {
            DaemonResponse::error_message(id, "Already authenticated")
        }
        DaemonCommand::Login(params) => cmd_login(id, params, engine).await,
        DaemonCommand::Register(params) => cmd_register(id, params, engine).await,
        DaemonCommand::Logout => cmd_logout(id, engine).await,
        DaemonCommand::GetState => cmd_get_state(id, engine).await,
        DaemonCommand::SendClipboard(params) => cmd_send_clipboard(id, params.text, engine).await,
        DaemonCommand::SendClipboardPayload(params) => {
            cmd_send_clipboard_payload(id, params.mime_type, params.bytes, engine).await
        }
        DaemonCommand::CopyToLocal(params) => cmd_copy_to_local(id, params.item_id, engine).await,
        DaemonCommand::ClipboardPayload(params) => {
            cmd_clipboard_payload(id, params.item_id, engine).await
        }
        DaemonCommand::UploadFile(params) => cmd_upload_file(id, params.file_path, engine).await,
        DaemonCommand::DownloadFile(params) => {
            cmd_download_file(id, params.file_id, params.target_path, engine).await
        }
        DaemonCommand::DeleteFile(params) => cmd_delete_file(id, params.file_id, engine).await,
        DaemonCommand::Refresh => cmd_refresh(id, engine).await,
    }
}

fn client_error(id: String, error: ClientError) -> DaemonResponse {
    DaemonResponse::error(id, error.error_response())
}

fn message_error(id: String, error: impl ToString) -> DaemonResponse {
    DaemonResponse::error_message(id, error.to_string())
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    rand::rng().fill(&mut bytes);
    bytes
}

async fn ensure_requested_base_url(
    engine: &Arc<SyncEngine>,
    requested: &str,
) -> Result<(), ClientError> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Ok(());
    }
    let configured = engine.base_url();
    if normalize_server_url(requested) == normalize_server_url(&configured) {
        return Ok(());
    }
    Err(ClientError::InvalidServerUrl(format!(
        "Server URL is fixed at client init: configured {configured}, requested {requested}"
    )))
}

fn normalize_server_url(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

async fn cmd_login(id: String, params: LoginParams, engine: &Arc<SyncEngine>) -> DaemonResponse {
    let device_name = params
        .device_name
        .as_deref()
        .unwrap_or(default_device_name());

    if let Some(url) = params.server_url.as_deref()
        && let Err(error) = ensure_requested_base_url(engine, url).await
    {
        return client_error(id, error);
    }

    match engine
        .login_with_platform(
            &params.passphrase,
            &params.username,
            device_name,
            platform_name(),
        )
        .await
    {
        Ok(()) => {
            // Store credentials in Keychain
            let url = engine.base_url();
            let state = engine.get_state().await;
            let creds = Credentials {
                device_name: device_name.to_string(),
                server_url: url,
                username: state.username,
            };
            if let Err(e) = keychain::store_credentials(&creds) {
                warn!("Failed to store server profile: {}", e);
            }
            DaemonResponse::success(id, None)
        }
        Err(e) => client_error(id, e),
    }
}

async fn cmd_register(
    id: String,
    params: RegisterParams,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    let device_name = params
        .device_name
        .as_deref()
        .unwrap_or(default_device_name());

    if let Some(url) = params.server_url.as_deref()
        && let Err(error) = ensure_requested_base_url(engine, url).await
    {
        return client_error(id, error);
    }

    match engine
        .register_with_platform(
            &params.access_key,
            &params.username,
            &params.passphrase,
            device_name,
            platform_name(),
        )
        .await
    {
        Ok(username) => {
            let url = engine.base_url();
            let creds = Credentials {
                device_name: device_name.to_string(),
                server_url: url,
                username: Some(username.clone()),
            };
            if let Err(e) = keychain::store_credentials(&creds) {
                warn!("Failed to store server profile: {}", e);
            }
            json_success(id, RegisterResult { username })
        }
        Err(e) => client_error(id, e),
    }
}

async fn cmd_logout(id: String, engine: &Arc<SyncEngine>) -> DaemonResponse {
    match engine.logout().await {
        Ok(()) => {
            if let Err(e) = keychain::clear_credentials() {
                warn!("Failed to clear stored server profile: {}", e);
            }
            DaemonResponse::success(id, None)
        }
        Err(e) => client_error(id, e),
    }
}

async fn cmd_get_state(id: String, engine: &Arc<SyncEngine>) -> DaemonResponse {
    let state = engine.get_state().await;
    json_success(id, state)
}

async fn cmd_send_clipboard(id: String, text: String, engine: &Arc<SyncEngine>) -> DaemonResponse {
    match engine
        .send_clipboard_payload(TEXT_CLIPBOARD_MIME_TYPE, text.as_bytes())
        .await
    {
        Ok(_) => DaemonResponse::success(id, None),
        Err(e) => client_error(id, e),
    }
}

async fn cmd_send_clipboard_payload(
    id: String,
    mime_type: String,
    bytes: Vec<u8>,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    match engine.send_clipboard_payload(&mime_type, &bytes).await {
        Ok(item_id) => json_success(id, serde_json::json!({ "id": item_id })),
        Err(e) => client_error(id, e),
    }
}

async fn cmd_copy_to_local(
    id: String,
    item_id: String,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    match engine.copy_to_local(&item_id).await {
        Ok(text) => {
            #[cfg(target_os = "linux")]
            if let Err(error) = crate::platform_clipboard::set_text(&text) {
                return message_error(id, error);
            }
            json_success(id, CopyToLocalResult { text })
        }
        Err(e) => client_error(id, e),
    }
}

async fn cmd_clipboard_payload(
    id: String,
    item_id: String,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    match engine.clipboard_payload(&item_id).await {
        Ok(payload) => json_success(
            id,
            ClipboardPayloadResult {
                mime_type: payload.mime_type,
                bytes: payload.bytes,
                text: payload.text,
            },
        ),
        Err(e) => client_error(id, e),
    }
}

async fn cmd_upload_file(
    id: String,
    file_path: String,
    engine: &Arc<SyncEngine>,
) -> DaemonResponse {
    match engine.upload_file(&file_path).await {
        Ok(file_id) => json_success(id, UploadFileResult { file_id }),
        Err(e) => client_error(id, e),
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
        Err(e) => client_error(id, e),
    }
}

async fn cmd_delete_file(id: String, file_id: String, engine: &Arc<SyncEngine>) -> DaemonResponse {
    match engine.delete_file(&file_id).await {
        Ok(()) => DaemonResponse::success(id, None),
        Err(e) => client_error(id, e),
    }
}

async fn cmd_refresh(id: String, engine: &Arc<SyncEngine>) -> DaemonResponse {
    match engine.refresh().await {
        Ok(()) => DaemonResponse::success(id, None),
        Err(e) => client_error(id, e),
    }
}

fn json_success<T: serde::Serialize>(id: String, value: T) -> DaemonResponse {
    match serde_json::to_value(value) {
        Ok(value) => DaemonResponse::success(id, Some(value)),
        Err(e) => message_error(id, e),
    }
}

fn default_device_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macOS"
    }
    #[cfg(target_os = "linux")]
    {
        "Linux"
    }
}

fn platform_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "linux")]
    {
        "linux"
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
