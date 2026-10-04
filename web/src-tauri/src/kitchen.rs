use clipper_app_types::{
    AppDocumentRevision, KitchenPantry, KitchenPantryChange, KitchenPlan, KitchenRecipe,
    KitchenRecipeList, KitchenSessionChange,
};
use clipper_daemon_types::{
    AppDocumentHistoryParams, DaemonCommand, KitchenChangeSessionParams, KitchenRecipeParams,
    KitchenRecipeRevisionParams, KitchenRecipesParams,
};
use tauri::State;

use crate::{CommandResult, DesktopBackend};

#[tauri::command]
pub async fn kitchen_recipes(
    backend: State<'_, DesktopBackend>,
    search: String,
    zone: String,
) -> CommandResult<KitchenRecipeList> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::KitchenRecipes(KitchenRecipesParams {
            search,
            zone,
        }))
        .await?)
}

#[tauri::command]
pub async fn kitchen_recipe(
    backend: State<'_, DesktopBackend>,
    id: String,
    servings: Option<u32>,
    zone: String,
) -> CommandResult<KitchenRecipe> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::KitchenRecipe(KitchenRecipeParams {
            id,
            servings,
            zone,
        }))
        .await?)
}

#[tauri::command]
pub async fn kitchen_recipe_history(
    backend: State<'_, DesktopBackend>,
    id: String,
) -> CommandResult<Vec<AppDocumentRevision>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::AppDocumentHistory(
            AppDocumentHistoryParams {
                collection: "kitchen.recipes".into(),
                id,
            },
        ))
        .await?)
}

#[tauri::command]
pub async fn kitchen_recipe_revision(
    backend: State<'_, DesktopBackend>,
    id: String,
    revision: u64,
    servings: Option<u32>,
) -> CommandResult<KitchenRecipe> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::KitchenRecipeRevision(
            KitchenRecipeRevisionParams {
                id,
                revision,
                servings,
            },
        ))
        .await?)
}

#[tauri::command]
pub async fn kitchen_change_session(
    backend: State<'_, DesktopBackend>,
    recipe_id: String,
    revision: u64,
    servings: u32,
    change: KitchenSessionChange,
) -> CommandResult<()> {
    backend
        .daemon
        .send_ok(DaemonCommand::KitchenChangeSession(
            KitchenChangeSessionParams {
                recipe_id,
                revision,
                servings,
                change,
            },
        ))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn kitchen_pantry(backend: State<'_, DesktopBackend>) -> CommandResult<KitchenPantry> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::KitchenPantry)
        .await?)
}

#[tauri::command]
pub async fn kitchen_change_pantry(
    backend: State<'_, DesktopBackend>,
    change: KitchenPantryChange,
) -> CommandResult<()> {
    backend
        .daemon
        .send_ok(DaemonCommand::KitchenChangePantry(change))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn kitchen_plans(backend: State<'_, DesktopBackend>) -> CommandResult<Vec<KitchenPlan>> {
    Ok(backend
        .daemon
        .send_result(DaemonCommand::KitchenPlans)
        .await?)
}

#[cfg(target_os = "macos")]
struct DisplayAwake {
    wanted: bool,
    window_hidden: bool,
    held: Option<std::process::Child>,
}

#[cfg(target_os = "macos")]
static DISPLAY_AWAKE: std::sync::Mutex<DisplayAwake> = std::sync::Mutex::new(DisplayAwake {
    wanted: false,
    window_hidden: false,
    held: None,
});

#[tauri::command]
pub fn keep_display_awake(on: bool) {
    #[cfg(target_os = "macos")]
    change_display_awake(|state| state.wanted = on);
    #[cfg(not(target_os = "macos"))]
    let _ = on;
}

#[cfg(target_os = "macos")]
pub fn window_hidden(hidden: bool) {
    change_display_awake(|state| state.window_hidden = hidden);
}

#[cfg(target_os = "macos")]
fn change_display_awake(change: impl FnOnce(&mut DisplayAwake)) {
    let mut state = DISPLAY_AWAKE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    change(&mut state);
    let hold = state.wanted && !state.window_hidden;
    if hold && state.held.is_none() {
        match std::process::Command::new("/usr/bin/caffeinate")
            .args(["-d", "-w", &std::process::id().to_string()])
            .spawn()
        {
            Ok(child) => state.held = Some(child),
            Err(error) => tracing::warn!(%error, "Could not keep the display awake"),
        }
    } else if !hold && let Some(mut child) = state.held.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}
