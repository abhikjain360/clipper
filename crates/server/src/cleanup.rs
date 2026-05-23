use chrono::{Duration, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::info;

use crate::entity::{clipboard_items, event_log, files};
use crate::state::AppState;

/// Run periodic cleanup tasks.
pub async fn run_cleanup_loop(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600)); // every hour

    loop {
        interval.tick().await;
        if let Err(e) = cleanup_expired_clipboard(&state).await {
            tracing::error!(error = %e, "Clipboard cleanup failed");
        }
        if let Err(e) = cleanup_old_events(&state).await {
            tracing::error!(error = %e, "Event log cleanup failed");
        }
        if let Err(e) = cleanup_orphan_uploads(&state).await {
            tracing::error!(error = %e, "Orphan upload cleanup failed");
        }
    }
}

async fn cleanup_expired_clipboard(state: &AppState) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();

    let expired = clipboard_items::Entity::find()
        .filter(clipboard_items::Column::ExpiresAt.lt(&now))
        .all(state.db())
        .await?;

    let count = expired.len();
    for item in &expired {
        let path = state.clipboard_dir().join(&item.ciphertext_path);
        let _ = tokio::fs::remove_file(&path).await;
    }

    if count > 0 {
        clipboard_items::Entity::delete_many()
            .filter(clipboard_items::Column::ExpiresAt.lt(&now))
            .exec(state.db())
            .await?;
        info!(count, "Cleaned up expired clipboard items");
    }

    Ok(())
}

async fn cleanup_old_events(state: &AppState) -> anyhow::Result<()> {
    let cutoff = (Utc::now() - Duration::days(3)).to_rfc3339();

    let result = event_log::Entity::delete_many()
        .filter(event_log::Column::CreatedAt.lt(&cutoff))
        .exec(state.db())
        .await?;

    if result.rows_affected > 0 {
        info!(
            count = result.rows_affected,
            "Cleaned up old event log entries"
        );
    }

    Ok(())
}

async fn cleanup_orphan_uploads(state: &AppState) -> anyhow::Result<()> {
    // Delete incomplete uploads stuck before completion for more than 1 hour.
    let cutoff = (Utc::now() - Duration::hours(1)).to_rfc3339();

    let orphans = files::Entity::find()
        .filter(files::Column::Status.ne("complete"))
        .filter(files::Column::CreatedAt.lt(&cutoff))
        .all(state.db())
        .await?;

    let count = orphans.len();
    for f in &orphans {
        let path = state.files_dir().join(&f.blob_path);
        let _ = tokio::fs::remove_file(&path).await;
    }

    if count > 0 {
        files::Entity::delete_many()
            .filter(files::Column::Status.ne("complete"))
            .filter(files::Column::CreatedAt.lt(&cutoff))
            .exec(state.db())
            .await?;
        info!(count, "Cleaned up orphan file uploads");
    }

    Ok(())
}
