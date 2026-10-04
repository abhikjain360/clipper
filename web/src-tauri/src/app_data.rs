use clipper_app_types::AppDataWrite;
use clipper_daemon_types::{
    DaemonCommand, QueryAppDataParams, WriteAppDataParams, WriteAppDataResult,
};
use serde_json::{Map, Value};
use tauri::State;

use crate::{CommandResult, DesktopBackend};

#[tauri::command]
pub async fn query_app_data(
    backend: State<'_, DesktopBackend>,
    sql: String,
) -> CommandResult<Vec<Map<String, Value>>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::QueryAppData(QueryAppDataParams { sql }))
        .await?)
}

#[tauri::command]
pub async fn write_app_data(
    backend: State<'_, DesktopBackend>,
    collection: String,
    row_id: Option<String>,
    write: AppDataWrite,
) -> CommandResult<String> {
    let result: WriteAppDataResult = backend
        .daemon
        .send_result(DaemonCommand::WriteAppData(WriteAppDataParams {
            collection,
            row_id,
            revision: None,
            write,
        }))
        .await?;
    Ok(result.id)
}
