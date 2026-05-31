use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket, close_code},
    },
    http::StatusCode,
    response::Response,
};
use chrono::Utc;
use clipper_core::models::{
    ObjectEventType, ObjectId, ObjectKind, WsClientMessage, WsError, WsServerMessage,
    WsTicketResponse,
};
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info};
use uuid::Uuid;

use crate::{
    auth::AuthInfo,
    entity::event_log,
    routes::{Postcard, RouteResult, error_response},
    state::AppState,
};

const WS_TICKET_PROTOCOL: &str = "clipper-ticket";

/// A broadcast message sent to all connected WebSocket clients.
#[derive(Clone, Debug)]
pub struct WsBroadcast {
    pub user_id: Uuid,
    pub source_device_id: Uuid,
    pub seq: i64,
    pub event_type: ObjectEventType,
    pub object_kind: ObjectKind,
    pub object_id: ObjectId,
    pub created_at: String,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    axum::Extension(auth): axum::Extension<AuthInfo>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state, auth))
}

pub async fn mint_ws_ticket(
    State(state): State<AppState>,
    axum::Extension(auth): axum::Extension<AuthInfo>,
) -> Postcard<WsTicketResponse> {
    let issued = state.create_ws_ticket(auth);
    Postcard(WsTicketResponse {
        ticket: issued.ticket,
        expires_at: issued.expires_at.to_rfc3339(),
    })
}

pub async fn ws_ticket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> RouteResult<Response> {
    let mut saw_ticket_protocol = false;
    let mut ticket = None;
    for protocol in ws
        .requested_protocols()
        .filter_map(|protocol| protocol.to_str().ok())
        .map(str::trim)
    {
        if protocol == WS_TICKET_PROTOCOL {
            saw_ticket_protocol = true;
        } else if !protocol.is_empty() && ticket.is_none() {
            ticket = Some(protocol.to_owned());
        }
    }
    let ticket = saw_ticket_protocol
        .then_some(ticket)
        .flatten()
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Missing WebSocket ticket"))?;
    let auth = state.consume_ws_ticket(&ticket).ok_or_else(|| {
        error_response(
            StatusCode::UNAUTHORIZED,
            "Invalid or expired WebSocket ticket",
        )
    })?;
    Ok(ws
        .protocols([WS_TICKET_PROTOCOL])
        .on_upgrade(move |socket| handle_socket(socket, state, auth)))
}

async fn handle_socket(mut socket: WebSocket, state: AppState, auth: AuthInfo) {
    let device_id = auth.device_id.to_string();
    info!(device_id = %device_id, "WebSocket connected");

    // Wait for hello message.
    match socket.recv().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<WsClientMessage>(&text) {
            Ok(WsClientMessage::Hello) => {}
            Err(_) => return close_with_error(socket, WsError::InvalidHello).await,
        },
        _ => return close_with_error(socket, WsError::ExpectedHello).await,
    }

    // Subscribe before reading the high-water seq. HTTP snapshots own state
    // through stream_start_seq; this live stream owns events after it.
    let mut rx = state.ws_tx().subscribe();

    let stream_start_seq = get_latest_seq(&state, auth.user_id).await.unwrap_or(0);
    let ack = WsServerMessage::HelloAck {
        server_time: Utc::now().to_rfc3339(),
        stream_start_seq,
    };
    if socket
        .send(Message::Text(serde_json::to_string(&ack).unwrap().into()))
        .await
        .is_err()
    {
        return;
    }

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
                        if evt.seq <= stream_start_seq {
                            continue;
                        }
                        if !should_forward_live_broadcast(&evt, auth.user_id, auth.device_id) {
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
                    // This client fell behind the broadcast buffer and missed
                    // events. Tell it to drop cached state and re-sync, then
                    // close cleanly so it reconnects without flagging an error.
                    Err(RecvError::Lagged(skipped)) => {
                        debug!(device_id = %device_id, skipped, "WebSocket lagged; invalidating");
                        let inv = WsServerMessage::Invalidate {
                            target: "all".to_string(),
                        };
                        _ = socket
                            .send(Message::Text(serde_json::to_string(&inv).unwrap().into()))
                            .await;
                        _ = socket
                            .send(Message::Close(Some(CloseFrame {
                                code: close_code::AWAY,
                                reason: "lagged".into(),
                            })))
                            .await;
                        break;
                    }
                    // Broadcast channel closed: the server is going away. Close
                    // cleanly so the client treats it as a normal disconnect.
                    Err(RecvError::Closed) => {
                        _ = socket
                            .send(Message::Close(Some(CloseFrame {
                                code: close_code::AWAY,
                                reason: "server shutting down".into(),
                            })))
                            .await;
                        break;
                    }
                }
            }
        }
    }

    info!(device_id = %device_id, "WebSocket disconnected");
}

/// Report a typed protocol error to the client, then close the socket cleanly.
///
/// Dropping the socket alone would close the connection abruptly (no Close
/// frame), so we send the error message followed by a Close frame before the
/// socket is dropped at the end of this function.
async fn close_with_error(mut socket: WebSocket, error: WsError) {
    debug!(%error, "Closing WebSocket after client protocol error");
    let msg = WsServerMessage::Error { error };
    if let Ok(text) = serde_json::to_string(&msg) {
        _ = socket.send(Message::Text(text.into())).await;
    }
    _ = socket.send(Message::Close(None)).await;
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

fn should_forward_live_broadcast(evt: &WsBroadcast, user_id: Uuid, device_id: Uuid) -> bool {
    evt.user_id == user_id && evt.source_device_id != device_id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broadcast(user_id: Uuid, source_device_id: Uuid) -> WsBroadcast {
        WsBroadcast {
            user_id,
            source_device_id,
            seq: 1,
            event_type: ObjectEventType::Created,
            object_kind: ObjectKind::File,
            object_id: Uuid::now_v7().into(),
            created_at: Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn live_broadcast_filter_skips_self_originated_events() {
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();

        assert!(!should_forward_live_broadcast(
            &broadcast(user_id, device_id),
            user_id,
            device_id,
        ));
    }

    #[test]
    fn live_broadcast_filter_keeps_other_devices_for_same_user() {
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let other_device_id = Uuid::now_v7();

        assert!(should_forward_live_broadcast(
            &broadcast(user_id, other_device_id),
            user_id,
            device_id,
        ));
    }

    #[test]
    fn live_broadcast_filter_skips_other_users() {
        let user_id = Uuid::now_v7();
        let other_user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let other_device_id = Uuid::now_v7();

        assert!(!should_forward_live_broadcast(
            &broadcast(other_user_id, other_device_id),
            user_id,
            device_id,
        ));
    }
}
