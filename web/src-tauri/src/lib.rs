use std::sync::{Arc, OnceLock};

use clipper_app_types::AppState;
use clipper_client::engine::{ClipboardPayload, SyncEngine, TEXT_CLIPBOARD_MIME_TYPE};
use serde::{Serialize, Serializer};
use tauri::{Manager, State};
use tracing_subscriber::EnvFilter;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8787";

struct DesktopBackend {
    engine: Arc<SyncEngine>,
}

#[derive(Debug, thiserror::Error)]
enum CommandError {
    #[error("{0}")]
    Client(String),
    #[error("server URL is fixed at client init: configured {configured}, requested {requested}")]
    ServerUrlMismatch {
        configured: String,
        requested: String,
    },
}

#[derive(Serialize)]
struct CommandErrorBody {
    code: &'static str,
    message: String,
}

impl Serialize for CommandError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        CommandErrorBody {
            code: match self {
                Self::Client(_) => "client",
                Self::ServerUrlMismatch { .. } => "server_url_mismatch",
            },
            message: self.to_string(),
        }
        .serialize(serializer)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopClipboardPayload {
    mime_type: String,
    bytes: Vec<u8>,
    text: Option<String>,
}

impl From<ClipboardPayload> for DesktopClipboardPayload {
    fn from(payload: ClipboardPayload) -> Self {
        Self {
            mime_type: payload.mime_type,
            bytes: payload.bytes,
            text: payload.text,
        }
    }
}

impl From<clipper_client::api_client::ClientError> for CommandError {
    fn from(error: clipper_client::api_client::ClientError) -> Self {
        Self::Client(error.to_string())
    }
}

type CommandResult<T> = Result<T, CommandError>;

pub fn run() {
    init_tracing();

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| Box::<dyn std::error::Error>::from(error.to_string()))?
                .join("client");
            app.manage(DesktopBackend {
                engine: SyncEngine::new_with_data_dir(DEFAULT_BASE_URL, data_dir),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect,
            default_server_url,
            login,
            register,
            logout,
            get_state,
            state_version,
            wait_for_state_change,
            refresh,
            send_clipboard_text,
            send_clipboard_payload,
            clipboard_payload,
            upload_file_path,
            upload_file_bytes,
            download_file_path,
            download_file_bytes,
            delete_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}

fn init_tracing() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("clipper_desktop=info,clipper_client=info"));
        tracing_subscriber::fmt().with_env_filter(filter).init();
    });
}

#[tauri::command]
async fn connect() -> CommandResult<()> {
    Ok(())
}

#[tauri::command]
fn default_server_url() -> String {
    DEFAULT_BASE_URL.to_string()
}

#[tauri::command]
async fn login(
    backend: State<'_, DesktopBackend>,
    passphrase: String,
    username: String,
    device_name: String,
    server_url: String,
) -> CommandResult<()> {
    let engine = engine(&backend);
    ensure_requested_base_url(&engine, &server_url)?;
    engine
        .login_with_platform(
            &passphrase,
            &username,
            non_empty_or_default(&device_name, default_device_name()),
            platform(),
        )
        .await?;
    Ok(())
}

#[tauri::command]
async fn register(
    backend: State<'_, DesktopBackend>,
    access_key: String,
    username: String,
    passphrase: String,
    device_name: String,
    server_url: String,
) -> CommandResult<String> {
    let engine = engine(&backend);
    ensure_requested_base_url(&engine, &server_url)?;
    Ok(engine
        .register_with_platform(
            &access_key,
            &username,
            &passphrase,
            non_empty_or_default(&device_name, default_device_name()),
            platform(),
        )
        .await?)
}

#[tauri::command]
async fn logout(backend: State<'_, DesktopBackend>) -> CommandResult<()> {
    backend.engine.logout().await?;
    Ok(())
}

#[tauri::command]
async fn get_state(backend: State<'_, DesktopBackend>) -> CommandResult<AppState> {
    Ok(backend.engine.get_state().await)
}

#[tauri::command]
fn state_version(backend: State<'_, DesktopBackend>) -> u64 {
    backend.engine.state_version()
}

#[tauri::command]
async fn wait_for_state_change(
    backend: State<'_, DesktopBackend>,
    seen_version: u64,
) -> CommandResult<u64> {
    Ok(backend
        .engine
        .wait_for_state_change_after(seen_version)
        .await?)
}

#[tauri::command]
async fn refresh(backend: State<'_, DesktopBackend>) -> CommandResult<()> {
    backend.engine.refresh().await?;
    Ok(())
}

#[tauri::command]
async fn send_clipboard_text(
    backend: State<'_, DesktopBackend>,
    text: String,
) -> CommandResult<String> {
    Ok(backend
        .engine
        .send_clipboard_payload(TEXT_CLIPBOARD_MIME_TYPE, text.as_bytes())
        .await?)
}

#[tauri::command]
async fn send_clipboard_payload(
    backend: State<'_, DesktopBackend>,
    mime_type: String,
    bytes: Vec<u8>,
) -> CommandResult<String> {
    Ok(backend
        .engine
        .send_clipboard_payload(&mime_type, &bytes)
        .await?)
}

#[tauri::command]
async fn clipboard_payload(
    backend: State<'_, DesktopBackend>,
    id: String,
) -> CommandResult<DesktopClipboardPayload> {
    Ok(backend.engine.clipboard_payload(&id).await?.into())
}

#[tauri::command]
async fn upload_file_path(
    backend: State<'_, DesktopBackend>,
    path: String,
) -> CommandResult<String> {
    Ok(backend.engine.upload_file(&path).await?)
}

#[tauri::command]
async fn upload_file_bytes(
    backend: State<'_, DesktopBackend>,
    filename: String,
    mime_type: String,
    bytes: Vec<u8>,
) -> CommandResult<String> {
    Ok(backend
        .engine
        .upload_file_bytes(&filename, Some(&mime_type), &bytes)
        .await?)
}

#[tauri::command]
async fn download_file_path(
    backend: State<'_, DesktopBackend>,
    file_id: String,
    path: String,
) -> CommandResult<()> {
    backend.engine.download_file(&file_id, &path).await?;
    Ok(())
}

#[tauri::command]
async fn download_file_bytes(
    backend: State<'_, DesktopBackend>,
    file_id: String,
) -> CommandResult<Vec<u8>> {
    Ok(backend.engine.download_file_bytes(&file_id).await?)
}

#[tauri::command]
async fn delete_file(backend: State<'_, DesktopBackend>, file_id: String) -> CommandResult<()> {
    backend.engine.delete_file(&file_id).await?;
    Ok(())
}

fn engine(backend: &DesktopBackend) -> Arc<SyncEngine> {
    Arc::clone(&backend.engine)
}

fn ensure_requested_base_url(engine: &Arc<SyncEngine>, requested: &str) -> CommandResult<()> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Ok(());
    }
    let configured = engine.base_url();
    if normalize_server_url(requested) == normalize_server_url(&configured) {
        return Ok(());
    }
    Err(CommandError::ServerUrlMismatch {
        configured,
        requested: requested.to_string(),
    })
}

fn normalize_server_url(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

fn non_empty_or_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.trim().is_empty() {
        default
    } else {
        value
    }
}

fn default_device_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macOS-Clipper"
    }
    #[cfg(target_os = "linux")]
    {
        "Linux-Clipper"
    }
    #[cfg(target_os = "windows")]
    {
        "Windows-Clipper"
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "Desktop-Clipper"
    }
}

fn platform() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "desktop"
    }
}
