//! Multi-client broadcast manager.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{mpsc, RwLock};
use tracing::debug;

/// Manages connected app clients and broadcasts events to all of them.
pub struct ClientManager {
    clients: RwLock<HashMap<u64, mpsc::Sender<String>>>,
    next_id: AtomicU64,
}

impl ClientManager {
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Register a new client. Returns the client ID and a receiver for outbound event lines.
    pub async fn register(&self) -> (u64, mpsc::Receiver<String>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(64);
        self.clients.write().await.insert(id, tx);
        debug!(client_id = id, "Client registered");
        (id, rx)
    }

    /// Unregister a client.
    pub async fn unregister(&self, id: u64) {
        self.clients.write().await.remove(&id);
        debug!(client_id = id, "Client unregistered");
    }

    /// Broadcast a JSON line to all connected clients.
    pub async fn broadcast(&self, json_line: &str) {
        let clients = self.clients.read().await;
        let mut dead = Vec::new();
        for (&id, tx) in clients.iter() {
            if tx.try_send(json_line.to_string()).is_err() {
                dead.push(id);
            }
        }
        drop(clients);

        if !dead.is_empty() {
            let mut clients = self.clients.write().await;
            for id in dead {
                clients.remove(&id);
                debug!(client_id = id, "Removed dead client");
            }
        }
    }
}
