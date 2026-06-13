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
    entity::{event_log, objects},
    rate_limit::rate_limited_error,
    routes::{Postcard, RouteResult, error_response},
    state::AppState,
};

const WS_TICKET_PROTOCOL: &str = "clipper-ticket";

/// Clients only ever send a small JSON hello and control frames, so cap
/// inbound messages well below the transport default to bound per-connection
/// memory.
const WS_MAX_MESSAGE_BYTES: usize = 64 * 1024;

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
    ws.max_message_size(WS_MAX_MESSAGE_BYTES)
        .max_frame_size(WS_MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, auth))
}

pub async fn mint_ws_ticket(
    State(state): State<AppState>,
    axum::Extension(auth): axum::Extension<AuthInfo>,
) -> RouteResult<Postcard<WsTicketResponse>> {
    // Tickets live in one map shared by all users, so unlimited minting would
    // let one account evict other users' unconsumed tickets.
    if !state.rate_limiter().check_ws_ticket_user(auth.user_id) {
        return Err(rate_limited_error());
    }

    let issued = state.create_ws_ticket(auth);
    Ok(Postcard(WsTicketResponse {
        ticket: issued.ticket,
        expires_at: issued.expires_at.to_rfc3339(),
    }))
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
        .max_message_size(WS_MAX_MESSAGE_BYTES)
        .max_frame_size(WS_MAX_MESSAGE_BYTES)
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
    let mut rx = state.subscribe_ws_broadcasts(auth.user_id);

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
        drop(rx);
        state.prune_idle_ws_broadcast_channel(auth.user_id);
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
                        // Fixed snapshot boundary: events at or below the
                        // connect-time high-water are already covered by the HTTP
                        // snapshot. This must stay a constant for the connection's
                        // lifetime, not an advancing per-event cursor (live
                        // delivery is not seq-ordered; see broadcast_ws_event).
                        if evt.seq <= stream_start_seq {
                            continue;
                        }
                        if !should_forward_live_broadcast(&evt, auth.device_id) {
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

    drop(rx);
    state.prune_idle_ws_broadcast_channel(auth.user_id);
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

/// Snapshot high-water mark for a user: the boundary between what the HTTP
/// snapshot owns and what the live stream owns.
///
/// It must cover every object the snapshot can return, so it is the max of the
/// newest `event_log.seq` AND the newest `objects.created_seq`. Deriving it from
/// `event_log` alone is wrong: `cleanup_old_events` prunes the log after a
/// retention window, so an inactive user's events can age out entirely and
/// report 0 — even though never-expiring File objects still exist. A fresh
/// device would then take 0 as the boundary and live deltas would not reconcile
/// against the snapshot. `objects.created_seq` is durable for as long as the
/// object exists, so it is the correct floor.
pub(crate) async fn get_latest_seq(state: &AppState, user_id: Uuid) -> Result<i64, sea_orm::DbErr> {
    let latest_event_seq: Option<i64> = event_log::Entity::find()
        .filter(event_log::Column::UserId.eq(user_id))
        .order_by(event_log::Column::Seq, Order::Desc)
        .select_only()
        .column(event_log::Column::Seq)
        .into_tuple::<i64>()
        .one(state.db())
        .await?;
    let latest_object_seq: Option<i64> = objects::Entity::find()
        .filter(objects::Column::UserId.eq(user_id))
        .filter(objects::Column::Status.eq("complete"))
        .filter(objects::Column::CreatedSeq.is_not_null())
        .order_by(objects::Column::CreatedSeq, Order::Desc)
        .select_only()
        .column(objects::Column::CreatedSeq)
        .into_tuple::<Option<i64>>()
        .one(state.db())
        .await?
        .flatten();
    Ok(latest_event_seq
        .unwrap_or(0)
        .max(latest_object_seq.unwrap_or(0)))
}

fn should_forward_live_broadcast(evt: &WsBroadcast, device_id: Uuid) -> bool {
    evt.source_device_id != device_id
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
            device_id,
        ));
    }

    #[tokio::test]
    async fn ws_ticket_minting_is_rate_limited_per_user() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("db");
        let state = AppState::open_with_db(db, dir.path().to_path_buf())
            .await
            .expect("state");
        let auth = AuthInfo {
            session_id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            device_id: Uuid::now_v7(),
        };

        for _ in 0..state.config().rate_limit.ws_tickets_per_user_per_minute {
            mint_ws_ticket(State(state.clone()), axum::Extension(auth.clone()))
                .await
                .expect("mint ticket");
        }

        let err = mint_ws_ticket(State(state.clone()), axum::Extension(auth.clone()))
            .await
            .expect_err("over-quota mint must be rejected");
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);

        // Another user's budget is unaffected.
        let other_auth = AuthInfo {
            session_id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            device_id: Uuid::now_v7(),
        };
        mint_ws_ticket(State(state), axum::Extension(other_auth))
            .await
            .expect("other user mints fine");
    }
}
