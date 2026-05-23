use sea_orm::DatabaseConnection;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::ws::WsBroadcast;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub db: DatabaseConnection,
    pub data_dir: PathBuf,
    pub ws_tx: broadcast::Sender<WsBroadcast>,
}

impl AppState {
    pub fn new(db: DatabaseConnection, data_dir: PathBuf) -> Self {
        let (ws_tx, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(AppStateInner {
                db,
                data_dir,
                ws_tx,
            }),
        }
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.inner.db
    }

    pub fn clipboard_dir(&self) -> PathBuf {
        self.inner.data_dir.join("clipboard")
    }

    pub fn files_dir(&self) -> PathBuf {
        self.inner.data_dir.join("files")
    }

    pub fn ws_tx(&self) -> &broadcast::Sender<WsBroadcast> {
        &self.inner.ws_tx
    }
}
