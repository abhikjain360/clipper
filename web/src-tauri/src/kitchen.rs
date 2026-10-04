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
    change: KitchenSessionChange,
) -> CommandResult<()> {
    backend
        .daemon
        .send_ok(DaemonCommand::KitchenChangeSession(
            KitchenChangeSessionParams { recipe_id, change },
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
static DISPLAY_AWAKE: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

#[tauri::command]
pub fn keep_display_awake(on: bool) {
    #[cfg(target_os = "macos")]
    {
        let mut held = DISPLAY_AWAKE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if on && held.is_none() {
            match std::process::Command::new("/usr/bin/caffeinate")
                .args(["-d", "-w", &std::process::id().to_string()])
                .spawn()
            {
                Ok(child) => *held = Some(child),
                Err(error) => tracing::warn!(%error, "Could not keep the display awake"),
            }
        } else if !on && let Some(mut child) = held.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = on;
}
