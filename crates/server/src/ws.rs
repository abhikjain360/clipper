use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use chrono::Utc;
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};
use tracing::{info, warn};

use crate::auth::AuthInfo;
use crate::entity::event_log;
use crate::state::AppState;
use clipper_core::models::{WsClientMessage, WsServerMessage};

/// A broadcast message sent to all connected WebSocket clients.
#[derive(Clone, Debug)]
pub struct WsBroadcast {
    pub seq: i64,
    pub event_type: String,
    pub object_kind: String,
    pub object_id: String,
    pub created_at: String,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    auth: Option<axum::Extension<AuthInfo>>,
) -> Response {
    let auth_info = auth.map(|a| a.0);
    ws.on_upgrade(move |socket| handle_socket(socket, state, auth_info))
}

async fn handle_socket(mut socket: WebSocket, state: AppState, auth: Option<AuthInfo>) {
    let device_id = auth
        .as_ref()
        .map(|a| a.device_id.clone())
        .unwrap_or_else(|| "unknown".to_string());
    info!(device_id = %device_id, "WebSocket connected");

    // Wait for hello message
    let last_seq = match socket.recv().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<WsClientMessage>(&text) {
            Ok(WsClientMessage::Hello { last_seq }) => last_seq,
            Err(_) => {
                warn!("Invalid hello message");
                return;
            }
        },
        _ => {
            warn!("Expected hello message");
            return;
        }
    };

    // Send hello_ack
    let latest_seq = get_latest_seq(&state).await.unwrap_or(0);
    let ack = WsServerMessage::HelloAck {
        server_time: Utc::now().to_rfc3339(),
        latest_seq,
    };
    if socket
        .send(Message::Text(serde_json::to_string(&ack).unwrap().into()))
        .await
        .is_err()
    {
        return;
    }

    // Replay missed events
    if last_seq < latest_seq {
        if let Ok(events) = get_events_since(&state, last_seq).await {
            if !replay_is_contiguous(last_seq, &events) {
                // Gap too large or events pruned — send invalidate
                let inv = WsServerMessage::Invalidate {
                    target: "all".to_string(),
                };
                let _ = socket
                    .send(Message::Text(serde_json::to_string(&inv).unwrap().into()))
                    .await;
            } else {
                for evt in events {
                    let msg = WsServerMessage::Event {
                        seq: evt.seq,
                        event_type: evt.event_type,
                        object_kind: evt.object_kind,
                        object_id: evt.object_id,
                        created_at: evt.created_at,
                    };
                    if socket
                        .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        } else {
            // Gap too large or events pruned — send invalidate
            let inv = WsServerMessage::Invalidate {
                target: "all".to_string(),
            };
            let _ = socket
                .send(Message::Text(serde_json::to_string(&inv).unwrap().into()))
                .await;
        }
    }

    // Subscribe to broadcast
    let mut rx = state.ws_tx().subscribe();

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    _ => {}
                }
            }
            broadcast = rx.recv() => {
                match broadcast {
                    Ok(evt) => {
                        let msg = WsServerMessage::Event {
                            seq: evt.seq,
                            event_type: evt.event_type,
                            object_kind: evt.object_kind,
                            object_id: evt.object_id,
                            created_at: evt.created_at,
                        };
                        if socket
                            .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    info!(device_id = %device_id, "WebSocket disconnected");
}

async fn get_latest_seq(state: &AppState) -> Result<i64, sea_orm::DbErr> {
    let row = event_log::Entity::find()
        .order_by(event_log::Column::Seq, Order::Desc)
        .one(state.db())
        .await?;
    Ok(row.map(|r| r.seq).unwrap_or(0))
}

async fn get_events_since(
    state: &AppState,
    last_seq: i64,
) -> Result<Vec<event_log::Model>, sea_orm::DbErr> {
    event_log::Entity::find()
        .filter(event_log::Column::Seq.gt(last_seq))
        .order_by_asc(event_log::Column::Seq)
        .all(state.db())
        .await
}

fn replay_is_contiguous(last_seq: i64, events: &[event_log::Model]) -> bool {
    events
        .first()
        .is_some_and(|event| event.seq == last_seq.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(seq: i64) -> event_log::Model {
        event_log::Model {
            seq,
            event_type: "file.created".into(),
            object_kind: "file".into(),
            object_id: "00000000-0000-0000-0000-000000000000".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn replay_detects_pruned_gap_when_newer_events_exist() {
        assert!(!replay_is_contiguous(10, &[event(50), event(51)]));
    }

    #[test]
    fn replay_accepts_contiguous_history() {
        assert!(replay_is_contiguous(10, &[event(11), event(12)]));
    }
}
