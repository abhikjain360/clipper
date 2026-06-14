mod daemon_client;
mod daemon_spawn;
mod ipc_secret;

use std::path::PathBuf;
use std::sync::OnceLock;

use clipper_app_types::{AppState, DeviceInfo};
use clipper_daemon_types::{
    ClipboardPayloadResult, DaemonCommand, DeleteFileParams, DownloadFileParams,
    LoginParams, RegisterParams, RemoveDeviceParams, SendClipboardPayloadParams,
    UploadFileParams, ClipboardPayloadParams, CopyToLocalParams, RegisterResult,
    UploadFileResult, DeviceListResult,
};
use daemon_client::{DaemonClient, DaemonClientError};
use serde::{Deserialize, Serialize, Serializer};
use tauri::{Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;
use tracing_subscriber::EnvFilter;
use zeroize::Zeroizing;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8787";
const TEXT_CLIPBOARD_MIME_TYPE: &str = "text/plain";

struct DesktopBackend {
    daemon: DaemonClient,
}

#[derive(Debug, thiserror::Error)]
enum CommandError {
    #[error("{0}")]
    Client(String),
    #[error("native file dialog failed: {0}")]
    NativeFileDialog(String),
    #[error("native clipboard failed: {0}")]
    NativeClipboard(String),
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
                Self::NativeFileDialog(_) => "native_file_dialog",
                Self::NativeClipboard(_) => "native_clipboard",
            },
            message: self.to_string(),
        }
        .serialize(serializer)
    }
}

impl From<DaemonClientError> for CommandError {
    fn from(e: DaemonClientError) -> Self {
        Self::Client(e.to_string())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopClipboardPayload {
    mime_type: String,
    bytes: Vec<u8>,
    text: Option<String>,
}

impl From<ClipboardPayloadResult> for DesktopClipboardPayload {
    fn from(r: ClipboardPayloadResult) -> Self {
        Self { mime_type: r.mime_type, bytes: r.bytes, text: r.text }
    }
}

#[derive(Deserialize)]
struct SendItemResult {
    id: String,
}

type CommandResult<T> = Result<T, CommandError>;

pub fn run() {
    init_tracing();

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let daemon_data_dir = dirs::data_dir()
                .ok_or("could not determine data directory")?
                .join("Clipper");

            daemon_spawn::spawn_daemon(DEFAULT_BASE_URL);

            let (daemon, daemon_fut) = DaemonClient::new_with_future(daemon_data_dir);
            tauri::async_runtime::spawn(daemon_fut);

            app.manage(DesktopBackend { daemon });
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
            send_current_clipboard_text,
            send_clipboard_payload,
            clipboard_payload,
            write_clipboard_item_text,
            upload_file_from_dialog,
            upload_file_bytes,
            download_file_to_dialog,
            download_file_bytes,
            delete_file,
            list_devices,
            remove_device,
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
    backend
        .daemon
        .send_ok(DaemonCommand::Login(LoginParams {
            passphrase: Zeroizing::new(passphrase),
            username,
            device_name: non_empty_string(device_name),
            server_url: non_empty_string(server_url),
        }))
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
    let result = backend
        .daemon
        .send_result::<RegisterResult>(DaemonCommand::Register(RegisterParams {
            access_key: Zeroizing::new(access_key),
            username,
            passphrase: Zeroizing::new(passphrase),
            device_name: non_empty_string(device_name),
            server_url: non_empty_string(server_url),
        }))
        .await?;
    Ok(result.username)
}

#[tauri::command]
async fn logout(backend: State<'_, DesktopBackend>) -> CommandResult<()> {
    backend.daemon.send_ok(DaemonCommand::Logout).await?;
    Ok(())
}

#[tauri::command]
async fn get_state(backend: State<'_, DesktopBackend>) -> CommandResult<AppState> {
    Ok(backend.daemon.get_state().await)
}

#[tauri::command]
fn state_version(backend: State<'_, DesktopBackend>) -> u64 {
    backend.daemon.state_version()
}

#[tauri::command]
async fn wait_for_state_change(
    backend: State<'_, DesktopBackend>,
    seen_version: u64,
) -> CommandResult<u64> {
    Ok(backend.daemon.wait_for_state_change_after(seen_version).await)
}

#[tauri::command]
async fn refresh(backend: State<'_, DesktopBackend>) -> CommandResult<()> {
    backend.daemon.send_ok(DaemonCommand::Refresh).await?;
    Ok(())
}

#[tauri::command]
async fn send_clipboard_text(
    backend: State<'_, DesktopBackend>,
    text: String,
) -> CommandResult<String> {
    let result = backend
        .daemon
        .send_result::<SendItemResult>(DaemonCommand::SendClipboardPayload(
            SendClipboardPayloadParams {
                mime_type: TEXT_CLIPBOARD_MIME_TYPE.to_string(),
                bytes: text.into_bytes(),
            },
        ))
        .await?;
    Ok(result.id)
}

#[tauri::command]
async fn send_current_clipboard_text(
    window: tauri::Window,
    backend: State<'_, DesktopBackend>,
) -> CommandResult<Option<String>> {
    let Some(text) = read_current_clipboard_text(&window)? else {
        return Ok(None);
    };
    if text.is_empty() {
        return Ok(None);
    }
    let result = backend
        .daemon
        .send_result::<SendItemResult>(DaemonCommand::SendClipboardPayload(
            SendClipboardPayloadParams {
                mime_type: TEXT_CLIPBOARD_MIME_TYPE.to_string(),
                bytes: text.into_bytes(),
            },
        ))
        .await?;
    Ok(Some(result.id))
}

#[tauri::command]
async fn send_clipboard_payload(
    backend: State<'_, DesktopBackend>,
    mime_type: String,
    bytes: Vec<u8>,
) -> CommandResult<String> {
    let result = backend
        .daemon
        .send_result::<SendItemResult>(DaemonCommand::SendClipboardPayload(
            SendClipboardPayloadParams { mime_type, bytes },
        ))
        .await?;
    Ok(result.id)
}

#[tauri::command]
async fn clipboard_payload(
    backend: State<'_, DesktopBackend>,
    id: String,
) -> CommandResult<DesktopClipboardPayload> {
    let result = backend
        .daemon
        .send_result::<ClipboardPayloadResult>(DaemonCommand::ClipboardPayload(
            ClipboardPayloadParams { item_id: id },
        ))
        .await?;
    Ok(result.into())
}

#[tauri::command]
async fn write_clipboard_item_text(
    window: tauri::Window,
    backend: State<'_, DesktopBackend>,
    id: String,
) -> CommandResult<()> {
    let result = backend
        .daemon
        .send_result::<ClipboardPayloadResult>(DaemonCommand::ClipboardPayload(
            ClipboardPayloadParams { item_id: id },
        ))
        .await?;
    let text = result
        .text
        .unwrap_or_else(|| String::from_utf8_lossy(&result.bytes).into_owned());
    window
        .clipboard()
        .write_text(text)
        .map_err(|e| CommandError::NativeClipboard(e.to_string()))?;
    Ok(())
}

#[tauri::command]
async fn upload_file_from_dialog(
    window: tauri::Window,
    backend: State<'_, DesktopBackend>,
) -> CommandResult<Option<String>> {
    let Some(path) = window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("Upload File")
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = dialog_path_to_path(path)?;
    let result = backend
        .daemon
        .send_result::<UploadFileResult>(DaemonCommand::UploadFile(UploadFileParams {
            file_path: path.to_string_lossy().into_owned(),
        }))
        .await?;
    Ok(Some(result.file_id))
}

#[tauri::command]
async fn upload_file_bytes(
    backend: State<'_, DesktopBackend>,
    filename: String,
    _mime_type: String,
    bytes: Vec<u8>,
) -> CommandResult<String> {
    let tmp = temp_path(&format!("upload-{filename}"));
    tokio::fs::write(&tmp, &bytes)
        .await
        .map_err(|e| CommandError::Client(format!("temp write: {e}")))?;
    let result = backend
        .daemon
        .send_result::<UploadFileResult>(DaemonCommand::UploadFile(UploadFileParams {
            file_path: tmp.to_string_lossy().into_owned(),
        }))
        .await;
    tokio::fs::remove_file(&tmp).await.ok();
    Ok(result?.file_id)
}

#[tauri::command]
async fn download_file_to_dialog(
    window: tauri::Window,
    backend: State<'_, DesktopBackend>,
    file_id: String,
    default_filename: String,
) -> CommandResult<bool> {
    let Some(path) = window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("Save File")
        .set_file_name(safe_dialog_filename(&default_filename))
        .blocking_save_file()
    else {
        return Ok(false);
    };
    let path = dialog_path_to_path(path)?;
    backend
        .daemon
        .send_ok(DaemonCommand::DownloadFile(DownloadFileParams {
            file_id,
            target_path: path.to_string_lossy().into_owned(),
        }))
        .await?;
    Ok(true)
}

#[tauri::command]
async fn download_file_bytes(
    backend: State<'_, DesktopBackend>,
    file_id: String,
) -> CommandResult<Vec<u8>> {
    let tmp = temp_path(&format!("download-{file_id}"));
    let result = backend
        .daemon
        .send_ok(DaemonCommand::DownloadFile(DownloadFileParams {
            file_id,
            target_path: tmp.to_string_lossy().into_owned(),
        }))
        .await;
    if result.is_err() {
        tokio::fs::remove_file(&tmp).await.ok();
        return Err(result.unwrap_err().into());
    }
    let bytes = tokio::fs::read(&tmp)
        .await
        .map_err(|e| CommandError::Client(format!("temp read: {e}")))?;
    tokio::fs::remove_file(&tmp).await.ok();
    Ok(bytes)
}

#[tauri::command]
async fn delete_file(backend: State<'_, DesktopBackend>, file_id: String) -> CommandResult<()> {
    backend
        .daemon
        .send_ok(DaemonCommand::DeleteFile(DeleteFileParams { file_id }))
        .await?;
    Ok(())
}

#[tauri::command]
async fn list_devices(backend: State<'_, DesktopBackend>) -> CommandResult<Vec<DeviceInfo>> {
    let result = backend
        .daemon
        .send_result::<DeviceListResult>(DaemonCommand::ListDevices)
        .await?;
    Ok(result.devices)
}

#[tauri::command]
async fn remove_device(
    backend: State<'_, DesktopBackend>,
    device_id: String,
) -> CommandResult<()> {
    backend
        .daemon
        .send_ok(DaemonCommand::RemoveDevice(RemoveDeviceParams { device_id }))
        .await?;
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn read_current_clipboard_text(_window: &tauri::Window) -> CommandResult<Option<String>> {
    Ok(clipper_client::clipboard_watcher::read_current_unconcealed_clipboard_text())
}

#[cfg(target_os = "linux")]
fn read_current_clipboard_text(_window: &tauri::Window) -> CommandResult<Option<String>> {
    clipper_client::clipboard_watcher::read_current_unconcealed_clipboard_text()
        .map_err(CommandError::NativeClipboard)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn read_current_clipboard_text(window: &tauri::Window) -> CommandResult<Option<String>> {
    Ok(Some(window.clipboard().read_text().map_err(|e| {
        CommandError::NativeClipboard(e.to_string())
    })?))
}

fn dialog_path_to_path(path: tauri_plugin_dialog::FilePath) -> CommandResult<PathBuf> {
    path.into_path()
        .map_err(|e| CommandError::NativeFileDialog(e.to_string()))
}

fn safe_dialog_filename(filename: &str) -> String {
    let cleaned = filename
        .trim()
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect::<String>();
    if cleaned.is_empty() { "clipper-download".to_string() } else { cleaned }
}

fn temp_path(suffix: &str) -> PathBuf {
    let safe: String = suffix
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    std::env::temp_dir().join(format!("clipper-{}-{}", std::process::id(), safe))
}

fn non_empty_string(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}
