mod daemon_client;
mod daemon_spawn;
mod ipc_secret;

use std::{path::PathBuf, sync::OnceLock};

use rand::RngExt;

use clipper_app_types::{
    ActualView, AppState, CollabItem, DeviceInfo, IngestReport, OccurrenceView,
};
use clipper_daemon_types::{
    ActualsBetweenParams, AddCalendarSourceParams, ClipboardPayloadParams, ClipboardPayloadResult,
    CreateScheduleItemParams, DaemonCommand, DeleteCollabDocParams, DeleteFileParams,
    DeleteScheduleObjectParams, DeviceListResult, DownloadFileParams, ExpandScheduleParams,
    GetCollabDocMetaParams, LoginParams, RegisterParams, RegisterResult, RemoveDeviceParams,
    RenameCollabDocParams, SendClipboardPayloadParams, StartActualParams, StopActualParams,
    SyncCalendarSourceParams, UpdateScheduleItemParams, UploadFileParams, UploadFileResult,
};
use clipper_schedule::ScheduleItem;
use daemon_client::{DaemonClient, DaemonClientError};
use serde::{Deserialize, Serialize, Serializer};
use tauri::{Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;
use tracing_subscriber::EnvFilter;
use zeroize::Zeroizing;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8787";
const TEXT_CLIPBOARD_MIME_TYPE: &str = "text/plain";
#[cfg_attr(not(unix), allow(dead_code))]
const PRIVATE_DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;

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
        Self {
            mime_type: r.mime_type,
            bytes: r.bytes,
            text: r.text,
        }
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
            create_collab_doc,
            delete_collab_doc,
            create_schedule_item,
            update_schedule_item,
            delete_schedule_object,
            expand_schedule,
            start_actual,
            stop_actual,
            actuals_between,
            add_calendar_source,
            sync_calendar_source,
            rename_collab_doc,
            get_collab_doc_meta,
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
    Ok(backend
        .daemon
        .wait_for_state_change_after(seen_version)
        .await)
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
    let tmp = create_private_temp_file("upload", &filename).await?;
    if let Err(e) = tokio::fs::write(&tmp, &bytes).await {
        tokio::fs::remove_file(&tmp).await.ok();
        return Err(CommandError::Client(format!("temp write: {e}")));
    }
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
    // Reserve a 0600 path first so the daemon's write keeps a private mode.
    let tmp = create_private_temp_file("download", &file_id).await?;
    let result = backend
        .daemon
        .send_ok(DaemonCommand::DownloadFile(DownloadFileParams {
            file_id,
            target_path: tmp.to_string_lossy().into_owned(),
        }))
        .await;
    if let Err(err) = result {
        tokio::fs::remove_file(&tmp).await.ok();
        return Err(err.into());
    }
    let bytes = match tokio::fs::read(&tmp).await {
        Ok(bytes) => bytes,
        Err(e) => {
            tokio::fs::remove_file(&tmp).await.ok();
            return Err(CommandError::Client(format!("temp read: {e}")));
        }
    };
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

/// Create a schedule series.
///
/// The item arrives as the domain type rather than as flattened fields: a
/// recurrence rule does not survive being reduced to strings, and serde keeps
/// the TypeScript shape honest.
#[tauri::command]
async fn create_schedule_item(
    backend: State<'_, DesktopBackend>,
    item: ScheduleItem,
) -> CommandResult<String> {
    Ok(backend
        .daemon
        .send_result::<String>(DaemonCommand::CreateScheduleItem(
            CreateScheduleItemParams { item },
        ))
        .await?)
}

#[tauri::command]
async fn update_schedule_item(
    backend: State<'_, DesktopBackend>,
    object_id: String,
    item: ScheduleItem,
    expected_revision: u64,
) -> CommandResult<String> {
    Ok(backend
        .daemon
        .send_result::<String>(DaemonCommand::UpdateScheduleItem(
            UpdateScheduleItemParams {
                object_id,
                item,
                expected_revision,
            },
        ))
        .await?)
}

#[tauri::command]
async fn delete_schedule_object(
    backend: State<'_, DesktopBackend>,
    object_id: String,
) -> CommandResult<()> {
    backend
        .daemon
        .send_ok(DaemonCommand::DeleteScheduleObject(
            DeleteScheduleObjectParams { object_id },
        ))
        .await?;
    Ok(())
}

/// Expand every series into the occurrences falling in `[from, to)`.
#[tauri::command]
async fn expand_schedule(
    backend: State<'_, DesktopBackend>,
    from: String,
    to: String,
    observer_zone: String,
) -> CommandResult<Vec<OccurrenceView>> {
    Ok(backend
        .daemon
        .send_result::<Vec<OccurrenceView>>(DaemonCommand::ExpandSchedule(ExpandScheduleParams {
            from,
            to,
            observer_zone,
        }))
        .await?)
}

#[tauri::command]
async fn start_actual(
    backend: State<'_, DesktopBackend>,
    plan_context: Option<String>,
) -> CommandResult<String> {
    Ok(backend
        .daemon
        .send_result::<String>(DaemonCommand::StartActual(StartActualParams {
            plan_context,
        }))
        .await?)
}

#[tauri::command]
async fn stop_actual(
    backend: State<'_, DesktopBackend>,
    object_id: String,
) -> CommandResult<String> {
    Ok(backend
        .daemon
        .send_result::<String>(DaemonCommand::StopActual(StopActualParams { object_id }))
        .await?)
}

#[tauri::command]
async fn actuals_between(
    backend: State<'_, DesktopBackend>,
    from: String,
    to: String,
) -> CommandResult<Vec<ActualView>> {
    Ok(backend
        .daemon
        .send_result::<Vec<ActualView>>(DaemonCommand::ActualsBetween(ActualsBetweenParams {
            from,
            to,
        }))
        .await?)
}

#[tauri::command]
async fn add_calendar_source(
    backend: State<'_, DesktopBackend>,
    name: String,
    url: String,
) -> CommandResult<String> {
    Ok(backend
        .daemon
        .send_result::<String>(DaemonCommand::AddCalendarSource(AddCalendarSourceParams {
            name,
            url,
        }))
        .await?)
}

#[tauri::command]
async fn sync_calendar_source(
    backend: State<'_, DesktopBackend>,
    object_id: String,
) -> CommandResult<IngestReport> {
    Ok(backend
        .daemon
        .send_result::<IngestReport>(DaemonCommand::SyncCalendarSource(
            SyncCalendarSourceParams { object_id },
        ))
        .await?)
}

#[tauri::command]
async fn create_collab_doc(backend: State<'_, DesktopBackend>) -> CommandResult<CollabItem> {
    Ok(backend
        .daemon
        .send_result::<CollabItem>(DaemonCommand::CreateCollabDoc)
        .await?)
}

#[tauri::command]
async fn delete_collab_doc(
    backend: State<'_, DesktopBackend>,
    object_id: String,
) -> CommandResult<()> {
    backend
        .daemon
        .send_ok(DaemonCommand::DeleteCollabDoc(DeleteCollabDocParams {
            object_id,
        }))
        .await?;
    Ok(())
}

#[tauri::command]
async fn rename_collab_doc(
    backend: State<'_, DesktopBackend>,
    object_id: String,
    title: String,
) -> CommandResult<CollabItem> {
    Ok(backend
        .daemon
        .send_result::<CollabItem>(DaemonCommand::RenameCollabDoc(RenameCollabDocParams {
            object_id,
            title,
        }))
        .await?)
}

#[tauri::command]
async fn get_collab_doc_meta(
    backend: State<'_, DesktopBackend>,
    object_id: String,
) -> CommandResult<CollabItem> {
    Ok(backend
        .daemon
        .send_result::<CollabItem>(DaemonCommand::GetCollabDocMeta(GetCollabDocMetaParams {
            object_id,
        }))
        .await?)
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
async fn remove_device(backend: State<'_, DesktopBackend>, device_id: String) -> CommandResult<()> {
    backend
        .daemon
        .send_ok(DaemonCommand::RemoveDevice(RemoveDeviceParams {
            device_id,
        }))
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
    if cleaned.is_empty() {
        "clipper-download".to_string()
    } else {
        cleaned
    }
}

fn staging_dir() -> PathBuf {
    dirs::cache_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("Clipper")
        .join("staging")
}

fn ensure_private_staging_dir() -> Result<PathBuf, CommandError> {
    let dir = staging_dir();
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CommandError::Client(format!("staging dir: {e}")))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        match std::fs::DirBuilder::new()
            .mode(PRIVATE_DIR_MODE)
            .create(&dir)
        {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(CommandError::Client(format!("staging dir: {e}"))),
        }
        let meta = std::fs::symlink_metadata(&dir)
            .map_err(|e| CommandError::Client(format!("staging dir: {e}")))?;
        if !meta.is_dir() {
            return Err(CommandError::Client("staging path is not a directory".into()));
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE))
            .map_err(|e| CommandError::Client(format!("staging dir: {e}")))?;
    }
    #[cfg(not(unix))]
    {
        match std::fs::DirBuilder::new().create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(CommandError::Client(format!("staging dir: {e}"))),
        }
        let meta = std::fs::metadata(&dir)
            .map_err(|e| CommandError::Client(format!("staging dir: {e}")))?;
        if !meta.is_dir() {
            return Err(CommandError::Client("staging path is not a directory".into()));
        }
    }
    Ok(dir)
}

fn sanitize_temp_prefix(value: &str) -> String {
    let safe: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let short: String = safe.chars().take(20).collect();
    if short.is_empty() {
        "file".to_string()
    } else {
        short
    }
}

fn random_hex_suffix() -> String {
    let mut bytes = [0u8; 8];
    rand::rng().fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn create_private_temp_file(kind: &str, hint: &str) -> Result<PathBuf, CommandError> {
    let dir = ensure_private_staging_dir()?;
    let prefix = sanitize_temp_prefix(hint);
    for _ in 0..10 {
        let name = format!(
            "clipper-{}-{}-{}-{}",
            kind,
            prefix,
            std::process::id(),
            random_hex_suffix()
        );
        let path = dir.join(name);
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(PRIVATE_FILE_MODE);
        match options.open(&path).await {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(CommandError::Client(format!("temp create: {e}"))),
        }
    }
    Err(CommandError::Client("temp create: too many collisions".into()))
}

fn non_empty_string(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}
