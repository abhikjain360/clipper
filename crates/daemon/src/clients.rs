//! Multi-client broadcast manager.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{RwLock, mpsc};
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_returns_unique_ids() {
        let mgr = ClientManager::new();
        let (id1, _rx1) = mgr.register().await;
        let (id2, _rx2) = mgr.register().await;
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn broadcast_reaches_all_clients() {
        let mgr = ClientManager::new();
        let (_id1, mut rx1) = mgr.register().await;
        let (_id2, mut rx2) = mgr.register().await;

        mgr.broadcast(r#"{"event":"test"}"#).await;

        let msg1 = rx1.recv().await.unwrap();
        let msg2 = rx2.recv().await.unwrap();
        assert_eq!(msg1, r#"{"event":"test"}"#);
        assert_eq!(msg2, r#"{"event":"test"}"#);
    }

    #[tokio::test]
    async fn unregister_stops_delivery() {
        let mgr = ClientManager::new();
        let (id1, mut rx1) = mgr.register().await;
        let (_id2, mut rx2) = mgr.register().await;

        mgr.unregister(id1).await;
        mgr.broadcast(r#"{"event":"after"}"#).await;

        // rx1 should get nothing (channel closed)
        assert!(rx1.try_recv().is_err());

        // rx2 should still get it
        let msg = rx2.recv().await.unwrap();
        assert_eq!(msg, r#"{"event":"after"}"#);
    }

    #[tokio::test]
    async fn broadcast_removes_dead_clients() {
        let mgr = ClientManager::new();
        let (_id1, rx1) = mgr.register().await;
        let (_id2, mut rx2) = mgr.register().await;

        // Drop rx1 to simulate a dead client
        drop(rx1);

        mgr.broadcast(r#"{"event":"ping"}"#).await;

        // Dead client should be cleaned up
        let clients = mgr.clients.read().await;
        assert_eq!(clients.len(), 1);
        drop(clients);

        // Live client still receives
        let msg = rx2.recv().await.unwrap();
        assert_eq!(msg, r#"{"event":"ping"}"#);
    }

    #[tokio::test]
    async fn broadcast_to_empty_is_noop() {
        let mgr = ClientManager::new();
        mgr.broadcast(r#"{"event":"nobody_home"}"#).await;
        // Should not panic
    }
}
