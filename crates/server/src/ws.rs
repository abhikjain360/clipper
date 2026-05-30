use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use chrono::Utc;
use clipper_core::models::{WsClientMessage, WsServerMessage};
use sea_orm::{
    ColumnTrait, DerivePartialModel, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect,
};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{auth::AuthInfo, entity::event_log, state::AppState};

/// A broadcast message sent to all connected WebSocket clients.
#[derive(Clone, Debug)]
pub struct WsBroadcast {
    pub user_id: Uuid,
    pub seq: i64,
    pub event_type: String,
    pub object_kind: String,
    pub object_id: String,
    pub created_at: String,
}

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "event_log::Entity", from_query_result)]
struct WsEventRow {
    seq: i64,
    event_type: String,
    object_kind: String,
    object_id: Uuid,
    created_at: String,
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
    let Some(auth) = auth else {
        warn!("WebSocket connected without auth");
        return;
    };
    let device_id = auth.device_id.to_string();
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
    let latest_seq = get_latest_seq(&state, auth.user_id).await.unwrap_or(0);
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
        if let Ok(events) = get_events_since(&state, auth.user_id, last_seq).await {
            for evt in events {
                let msg = WsServerMessage::Event {
                    seq: evt.seq,
                    event_type: evt.event_type,
                    object_kind: evt.object_kind,
                    object_id: evt.object_id.to_string(),
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
        } else {
            // Gap too large or events pruned — send invalidate
            let inv = WsServerMessage::Invalidate {
                target: "all".to_string(),
            };
            _ = socket
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
                        _ = socket.send(Message::Pong(data)).await;
                    }
                    _ => {}
                }
            }
            broadcast = rx.recv() => {
                match broadcast {
                    Ok(evt) => {
                        if evt.user_id != auth.user_id {
                            continue;
                        }
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

async fn get_latest_seq(state: &AppState, user_id: Uuid) -> Result<i64, sea_orm::DbErr> {
    let row = event_log::Entity::find()
        .filter(event_log::Column::UserId.eq(user_id))
        .order_by(event_log::Column::Seq, Order::Desc)
        .select_only()
        .column(event_log::Column::Seq)
        .into_tuple::<i64>()
        .one(state.db())
        .await?;
    Ok(row.unwrap_or(0))
}

async fn get_events_since(
    state: &AppState,
    user_id: Uuid,
    last_seq: i64,
) -> Result<Vec<WsEventRow>, sea_orm::DbErr> {
    event_log::Entity::find()
        .filter(event_log::Column::UserId.eq(user_id))
        .filter(event_log::Column::Seq.gt(last_seq))
        .order_by_asc(event_log::Column::Seq)
        .into_partial_model::<WsEventRow>()
        .all(state.db())
        .await
}
