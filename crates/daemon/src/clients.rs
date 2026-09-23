//! Multi-client broadcast manager.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use tokio::sync::{RwLock, watch};
use tracing::debug;

/// Manages connected app clients and broadcasts events to all of them.
pub struct ClientManager {
    clients: RwLock<HashMap<u64, watch::Sender<String>>>,
    next_id: AtomicU64,
}

impl ClientManager {
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Register a new client. Returns the client ID and a receiver that holds
    /// the latest event line.
    pub async fn register(&self) -> (u64, watch::Receiver<String>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = watch::channel(String::new());
        self.clients.write().await.insert(id, tx);
        debug!(client_id = id, "Client registered");
        (id, rx)
    }

    /// Unregister a client.
    pub async fn unregister(&self, id: u64) {
        self.clients.write().await.remove(&id);
        debug!(client_id = id, "Client unregistered");
    }

    /// Broadcast a JSON line to all connected clients. Each client keeps only
    /// the newest line, so a slow client skips states it has not written yet.
    pub async fn broadcast(&self, json_line: &str) {
        let clients = self.clients.read().await;
        let mut dead = Vec::new();
        for (&id, tx) in clients.iter() {
            if tx.send(json_line.to_string()).is_err() {
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

    async fn next_line(rx: &mut watch::Receiver<String>) -> String {
        rx.changed().await.unwrap();
        rx.borrow_and_update().clone()
    }

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

        assert_eq!(next_line(&mut rx1).await, r#"{"event":"test"}"#);
        assert_eq!(next_line(&mut rx2).await, r#"{"event":"test"}"#);
    }

    #[tokio::test]
    async fn unregister_stops_delivery() {
        let mgr = ClientManager::new();
        let (id1, mut rx1) = mgr.register().await;
        let (_id2, mut rx2) = mgr.register().await;

        mgr.unregister(id1).await;
        mgr.broadcast(r#"{"event":"after"}"#).await;

        assert!(rx1.changed().await.is_err());
        assert_eq!(next_line(&mut rx2).await, r#"{"event":"after"}"#);
    }

    #[tokio::test]
    async fn broadcast_removes_dead_clients() {
        let mgr = ClientManager::new();
        let (_id1, rx1) = mgr.register().await;
        let (_id2, mut rx2) = mgr.register().await;

        drop(rx1);

        mgr.broadcast(r#"{"event":"ping"}"#).await;

        assert_eq!(mgr.clients.read().await.len(), 1);
        assert_eq!(next_line(&mut rx2).await, r#"{"event":"ping"}"#);
    }

    #[tokio::test]
    async fn a_slow_client_stays_connected_and_receives_the_newest_state() {
        let mgr = ClientManager::new();
        let (_id, mut rx) = mgr.register().await;

        for index in 0..100 {
            mgr.broadcast(&format!(r#"{{"state":{index}}}"#)).await;
        }

        assert_eq!(mgr.clients.read().await.len(), 1);
        assert_eq!(next_line(&mut rx).await, r#"{"state":99}"#);
    }
}
