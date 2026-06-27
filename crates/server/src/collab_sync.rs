//! Live collaborative document sync (Phase 3a).
//!
//! Collab docs are the one server-visible object kind: their content is a shared
//! Y-CRDT document that the server relays between connected clients and persists
//! to `collab_docs.yjs_state`. This module keeps an authoritative [`yrs::Doc`]
//! per room so the server can (a) answer a newly joined client's sync-step-1
//! with the current state and (b) write the latest state back to the database;
//! update and awareness frames are fanned out to the room's other connections.
//!
//! ## Wire protocol
//!
//! This speaks the y-websocket binary protocol (lib0 v1 framing), one message
//! per binary WebSocket frame:
//!
//! ```text
//! [varuint msgType, ...]
//!   msgType 0 = sync:      [varuint syncType, varUint8Array payload]
//!     syncType 0 = step1:  payload = state vector  → reply step2 with our diff
//!     syncType 1 = step2:  payload = update        → apply
//!     syncType 2 = update: payload = update        → apply
//!   msgType 1 = awareness: [varUint8Array update]  → relay verbatim to others
//! ```
//!
//! Other message types (auth = 2, queryAwareness = 3) are accepted and ignored.
//!
//! The server is a relay, not an awareness participant: it never authors cursor
//! state. Awareness frames are forwarded as-is; stale cursors expire on the
//! clients' own awareness timeout. When a client's update integrates new content
//! the server computes the resulting delta and fans that out, so no-ops (such as
//! a keepalive step2 with nothing new) produce no traffic.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use chrono::Utc;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, sea_query::Expr};
use tokio::sync::broadcast::{self, error::RecvError};
use tracing::{debug, info, warn};
use uuid::Uuid;
use yrs::{
    Doc, ReadTxn, StateVector, Transact, Update,
    encoding::{
        read::{Cursor, Read},
        write::Write,
    },
    updates::{decoder::Decode, encoder::Encode},
};

use crate::{entity::collab_docs, state::AppState};

// y-websocket message types.
const MSG_SYNC: u32 = 0;
const MSG_AWARENESS: u32 = 1;
// y-protocols sync sub-message types.
const SYNC_STEP1: u32 = 0;
const SYNC_STEP2: u32 = 1;
const SYNC_UPDATE: u32 = 2;

/// Ring buffer of relayed frames per room. A client that falls this far behind
/// is resynced from the server's authoritative doc rather than fed the backlog.
const RELAY_CAPACITY: usize = 256;

/// Per-room live-connection cap. Bounds fan-out work and memory for one document
/// regardless of how widely its share link is distributed.
pub(crate) const MAX_CONNS_PER_ROOM: usize = 64;

/// Inbound/outbound frame ceiling. A collab document's full state (sent as a
/// sync step2) can exceed the small JSON-protocol limit, so this is generous but
/// still bounds how much a single peer can make the server buffer.
pub(crate) const COLLAB_WS_MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// How often the server sends an application-level keepalive (a sync step1).
/// y-websocket force-reconnects a connection that receives no *application*
/// message for 30s — WebSocket ping/pong frames do not reset that timer — so the
/// keepalive must stay well under it. The step1 also elicits a tiny step2 reply,
/// which resets the server's own idle clock.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Close a connection that has produced no inbound frame within this window.
/// With a 15s keepalive eliciting a reply, a live peer is seen every ~15s, so
/// this is roughly three missed cycles before a zombie is reaped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound every server-to-client send so a peer that stops reading cannot wedge
/// the connection task (mirrors `ws::WS_SEND_TIMEOUT`).
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// Coalesce a burst of edits into a single database write this long after the
/// last change.
const PERSIST_DEBOUNCE: Duration = Duration::from_secs(2);

/// A frame to relay to a room's other connections, tagged with the connection
/// that produced it so that connection is not echoed its own change.
#[derive(Clone)]
struct RelayFrame {
    origin: u64,
    data: Arc<[u8]>,
}

/// One loaded collaborative document: the authoritative CRDT plus the machinery
/// to relay changes and persist them. Shared as `Arc<CollabRoom>` between the
/// room registry in [`AppState`] and each connection handler.
pub struct CollabRoom {
    pub collab_doc_id: Uuid,
    inner: std::sync::Mutex<RoomInner>,
    relay: broadcast::Sender<RelayFrame>,
    /// Live WebSocket connections. Mutated only under the room-registry lock in
    /// [`AppState`]; the room is unloaded when this returns to zero.
    conn_count: AtomicUsize,
    /// Monotonic source of per-connection ids within this room.
    next_conn_id: AtomicU64,
    /// Bumped on every applied change. A debounced persist task only writes when
    /// its captured generation still matches, collapsing a burst of edits into
    /// one write and letting a disconnect cancel stale pending writes.
    persist_gen: AtomicU64,
}

struct RoomInner {
    doc: Doc,
}

impl CollabRoom {
    /// Build a room, seeding the document from persisted `yjs_state` if present.
    pub(crate) fn new(collab_doc_id: Uuid, initial_state: Option<Vec<u8>>) -> Arc<Self> {
        let (relay, _) = broadcast::channel(RELAY_CAPACITY);
        let doc = Doc::new();

        if let Some(state) = initial_state {
            match Update::decode_v1(&state) {
                Ok(update) => {
                    if let Err(error) = doc.transact_mut().apply_update(update) {
                        warn!(%collab_doc_id, %error, "Discarding unusable persisted collab state");
                    }
                }
                Err(error) => {
                    warn!(%collab_doc_id, %error, "Persisted collab state is corrupt; starting empty");
                }
            }
        }

        Arc::new(Self {
            collab_doc_id,
            inner: std::sync::Mutex::new(RoomInner { doc }),
            relay,
            conn_count: AtomicUsize::new(0),
            next_conn_id: AtomicU64::new(0),
            persist_gen: AtomicU64::new(0),
        })
    }

    /// Number of live connections (read under the registry lock by [`AppState`]).
    pub(crate) fn conn_count(&self) -> &AtomicUsize {
        &self.conn_count
    }

    /// The sync step1 frame (our current state vector) sent to a client so it
    /// replies with whatever it holds that we are missing.
    fn sync_step1_frame(&self) -> Vec<u8> {
        let sv = {
            let inner = self.inner.lock().expect("collab room lock poisoned");
            inner.doc.transact().state_vector()
        };
        encode_sync(SYNC_STEP1, &sv.encode_v1())
    }

    /// The sync step2 frame answering a client's step1: our state encoded as an
    /// update relative to the client's state vector `sv`.
    fn sync_step2_frame(&self, sv: &StateVector) -> Vec<u8> {
        let update = {
            let inner = self.inner.lock().expect("collab room lock poisoned");
            inner.doc.transact().encode_state_as_update_v1(sv)
        };
        encode_sync(SYNC_STEP2, &update)
    }

    /// Integrate a remote update (a sync step2 or update payload) authored by
    /// `conn_id`, fanning the resulting delta out to the room's other
    /// connections. Returns true if anything changed (so a persist is worth
    /// scheduling).
    ///
    /// The delta is computed precisely: the document's state vector is captured
    /// before applying, and the post-apply diff against it is exactly the newly
    /// integrated content. A no-op apply (e.g. a keepalive step2 with nothing
    /// new) yields an empty delta and is neither relayed nor persisted.
    fn apply_remote_update(&self, conn_id: u64, payload: &[u8]) -> bool {
        if is_empty_update(payload) {
            return false;
        }
        let update = match Update::decode_v1(payload) {
            Ok(update) => update,
            Err(error) => {
                debug!(collab_doc_id = %self.collab_doc_id, %error, "Discarding malformed collab update");
                return false;
            }
        };

        let delta = {
            let inner = self.inner.lock().expect("collab room lock poisoned");
            let before = inner.doc.transact().state_vector();
            {
                let mut txn = inner.doc.transact_mut();
                if let Err(error) = txn.apply_update(update) {
                    debug!(collab_doc_id = %self.collab_doc_id, %error, "Failed to integrate collab update");
                    return false;
                }
            }
            inner.doc.transact().encode_state_as_update_v1(&before)
        };

        if is_empty_update(&delta) {
            return false;
        }
        let _ = self.relay.send(RelayFrame {
            origin: conn_id,
            data: encode_sync(SYNC_UPDATE, &delta).into(),
        });
        true
    }

    /// Schedule a debounced write of the current state. Only the latest schedule
    /// in a [`PERSIST_DEBOUNCE`] window actually writes (generation check).
    fn schedule_persist(self: &Arc<Self>, state: &AppState) {
        let generation = self.persist_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let room = self.clone();
        let state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(PERSIST_DEBOUNCE).await;
            // A later edit (or a disconnect flush) superseded this write.
            if room.persist_gen.load(Ordering::SeqCst) != generation {
                return;
            }
            room.persist(&state).await;
        });
    }

    /// Write the current document state to `collab_docs.yjs_state`.
    async fn persist(&self, state: &AppState) {
        let update = {
            let inner = self.inner.lock().expect("collab room lock poisoned");
            inner
                .doc
                .transact()
                .encode_state_as_update_v1(&StateVector::default())
        };
        let now = Utc::now().to_rfc3339();
        let result = collab_docs::Entity::update_many()
            .col_expr(collab_docs::Column::YjsState, Expr::value(update))
            .col_expr(collab_docs::Column::UpdatedAt, Expr::value(now))
            .filter(collab_docs::Column::Id.eq(self.collab_doc_id))
            .exec(state.db())
            .await;
        if let Err(error) = result {
            warn!(collab_doc_id = %self.collab_doc_id, %error, "Failed to persist collab doc state");
        }
    }
}

/// Drive one upgraded collab WebSocket: handshake, relay loop, and a final
/// persist on disconnect. `initial_state` seeds the room if this is the first
/// connection for the document.
pub async fn handle_collab_socket(
    mut socket: WebSocket,
    state: AppState,
    collab_doc_id: Uuid,
    initial_state: Option<Vec<u8>>,
) {
    let Some(room) = state.acquire_collab_room(collab_doc_id, initial_state) else {
        debug!(%collab_doc_id, "Collab WebSocket rejected: per-room connection cap reached");
        _ = send_bounded(
            &mut socket,
            Message::Close(Some(CloseFrame {
                code: close_code::AGAIN,
                reason: "room full".into(),
            })),
            SEND_TIMEOUT,
        )
        .await;
        return;
    };
    let conn_id = room.next_conn_id.fetch_add(1, Ordering::SeqCst);
    info!(%collab_doc_id, conn_id, "Collab WebSocket connected");

    let mut rx = room.relay.subscribe();

    // Send sync step1 so the client replies with anything we are missing. The
    // client independently sends its own step1, which we answer with step2 — that
    // is how it receives existing content.
    if !send_bounded(
        &mut socket,
        Message::Binary(room.sync_step1_frame().into()),
        SEND_TIMEOUT,
    )
    .await
    {
        finish_collab_socket(&state, &room).await;
        return;
    }

    let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
    // `interval` yields immediately; drop that tick so the first keepalive lands
    // one full interval after the handshake.
    keepalive.tick().await;
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_activity = Instant::now();

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        last_activity = Instant::now();
                        if !handle_inbound(&state, &room, conn_id, data.as_ref(), &mut socket).await {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(payload))) => {
                        last_activity = Instant::now();
                        if !send_bounded(&mut socket, Message::Pong(payload), SEND_TIMEOUT).await {
                            break;
                        }
                    }
                    // Any other inbound frame (Pong, Text) still proves liveness.
                    Some(Ok(_)) => {
                        last_activity = Instant::now();
                    }
                    Some(Err(_)) => break,
                }
            }
            _ = keepalive.tick() => {
                if last_activity.elapsed() > IDLE_TIMEOUT {
                    debug!(%collab_doc_id, conn_id, "Collab WebSocket idle past deadline; closing");
                    _ = send_bounded(
                        &mut socket,
                        Message::Close(Some(CloseFrame {
                            code: close_code::AWAY,
                            reason: "idle timeout".into(),
                        })),
                        SEND_TIMEOUT,
                    )
                    .await;
                    break;
                }
                // A WebSocket ping keeps NAT/proxy paths warm; the sync step1 is
                // the application-level keepalive y-websocket needs to not
                // force-reconnect (see KEEPALIVE_INTERVAL).
                if !send_bounded(&mut socket, Message::Ping(Vec::new().into()), SEND_TIMEOUT).await {
                    break;
                }
                if !send_bounded(
                    &mut socket,
                    Message::Binary(room.sync_step1_frame().into()),
                    SEND_TIMEOUT,
                )
                .await
                {
                    break;
                }
            }
            frame = rx.recv() => {
                match frame {
                    Ok(relayed) => {
                        // Do not echo a change back to the connection that authored it.
                        if relayed.origin == conn_id {
                            continue;
                        }
                        if !send_bounded(
                            &mut socket,
                            Message::Binary(relayed.data.as_ref().to_vec().into()),
                            SEND_TIMEOUT,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    // Fell behind the relay buffer: resync from our authoritative
                    // doc instead of trying to replay the lost frames.
                    Err(RecvError::Lagged(_)) => {
                        if !send_bounded(
                            &mut socket,
                            Message::Binary(room.sync_step1_frame().into()),
                            SEND_TIMEOUT,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }

    finish_collab_socket(&state, &room).await;
    info!(%collab_doc_id, conn_id, "Collab WebSocket disconnected");
}

/// Parse and act on one inbound binary frame. Returns false when the socket must
/// close (a send failed); malformed frames are ignored without closing.
async fn handle_inbound(
    state: &AppState,
    room: &Arc<CollabRoom>,
    conn_id: u64,
    data: &[u8],
    socket: &mut WebSocket,
) -> bool {
    let mut cursor = Cursor::new(data);
    let Ok(msg_type) = cursor.read_var::<u32>() else {
        return true;
    };
    match msg_type {
        MSG_SYNC => {
            let Ok(sync_type) = cursor.read_var::<u32>() else {
                return true;
            };
            let Ok(payload) = cursor.read_buf() else {
                return true;
            };
            match sync_type {
                SYNC_STEP1 => {
                    let Ok(sv) = StateVector::decode_v1(payload) else {
                        return true;
                    };
                    send_bounded(
                        socket,
                        Message::Binary(room.sync_step2_frame(&sv).into()),
                        SEND_TIMEOUT,
                    )
                    .await
                }
                SYNC_STEP2 | SYNC_UPDATE => {
                    if room.apply_remote_update(conn_id, payload) {
                        room.schedule_persist(state);
                    }
                    true
                }
                _ => true,
            }
        }
        MSG_AWARENESS => {
            // The server does not track awareness; forward the whole frame to the
            // room's other connections verbatim.
            let _ = room.relay.send(RelayFrame {
                origin: conn_id,
                data: Arc::from(data),
            });
            true
        }
        // auth / queryAwareness / unknown — accepted and ignored.
        _ => true,
    }
}

/// Flush the latest state and release the room slot. Persisting before release
/// guarantees a room reloaded after the last disconnect reads current content;
/// bumping the persist generation cancels any pending debounced writes that
/// could otherwise clobber it with stale state.
async fn finish_collab_socket(state: &AppState, room: &Arc<CollabRoom>) {
    room.persist_gen.fetch_add(1, Ordering::SeqCst);
    room.persist(state).await;
    state.release_collab_room(room);
}

/// Send a frame with a bounded deadline. Returns false if the socket errors or
/// the deadline elapses (mirrors `ws::send_bounded`).
async fn send_bounded(socket: &mut WebSocket, msg: Message, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, socket.send(msg)).await,
        Ok(Ok(()))
    )
}

/// Encode a sync message: `[varuint MSG_SYNC, varuint sync_type, varUint8Array payload]`.
fn encode_sync(sync_type: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(payload.len() + 8);
    buf.write_var(MSG_SYNC);
    buf.write_var(sync_type);
    buf.write_buf(payload);
    buf
}

/// Whether a v1 update integrated nothing: no structs and an empty delete set
/// encode as the two-byte sequence `[0, 0]`.
fn is_empty_update(update: &[u8]) -> bool {
    update.len() <= 2
}

#[cfg(test)]
mod tests {
    use yrs::{GetString, Text};

    use super::*;

    /// A v1 update that inserts `content` into the `content` text — the shape a
    /// client sends to the room.
    fn text_update(content: &str) -> Vec<u8> {
        let doc = Doc::new();
        let text = doc.get_or_insert_text("content");
        {
            let mut txn = doc.transact_mut();
            text.insert(&mut txn, 0, content);
        }
        doc.transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    fn decode_sync(frame: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut cursor = Cursor::new(frame);
        let msg_type = cursor.read_var::<u32>().unwrap();
        let sync_type = cursor.read_var::<u32>().unwrap();
        let payload = cursor.read_buf().unwrap().to_vec();
        (msg_type, sync_type, payload)
    }

    /// Reconstruct the `content` text from a v1 update, as a peer would.
    fn apply_to_fresh(update: &[u8]) -> String {
        let doc = Doc::new();
        let text = doc.get_or_insert_text("content");
        doc.transact_mut()
            .apply_update(Update::decode_v1(update).unwrap())
            .unwrap();
        text.get_string(&doc.transact())
    }

    #[test]
    fn encode_sync_roundtrips() {
        let frame = encode_sync(SYNC_UPDATE, b"payload-bytes");
        assert_eq!(
            decode_sync(&frame),
            (MSG_SYNC, SYNC_UPDATE, b"payload-bytes".to_vec())
        );
    }

    #[test]
    fn is_empty_update_detects_noops() {
        let empty = Doc::new()
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        assert!(is_empty_update(&empty));
        assert!(!is_empty_update(&text_update("hi")));
    }

    #[test]
    fn apply_remote_update_relays_delta_and_skips_noops() {
        let room = CollabRoom::new(Uuid::now_v7(), None);
        let mut rx = room.relay.subscribe();

        // A real update relays a sync-update frame tagged with its author, and
        // the relayed payload reconstructs the same text on a fresh peer.
        let update = text_update("hello");
        assert!(room.apply_remote_update(7, &update));
        let relayed = rx.try_recv().expect("a frame should be relayed");
        assert_eq!(relayed.origin, 7);
        let (msg_type, sync_type, payload) = decode_sync(&relayed.data);
        assert_eq!((msg_type, sync_type), (MSG_SYNC, SYNC_UPDATE));
        assert_eq!(apply_to_fresh(&payload), "hello");

        // Re-applying the same update integrates nothing: no relay, no persist.
        assert!(!room.apply_remote_update(7, &update));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn room_seeds_from_persisted_state() {
        let room = CollabRoom::new(Uuid::now_v7(), Some(text_update("seeded")));
        // The step2 answer to an empty state vector carries the seeded content.
        let frame = room.sync_step2_frame(&StateVector::default());
        let (_, sync_type, payload) = decode_sync(&frame);
        assert_eq!(sync_type, SYNC_STEP2);
        assert_eq!(apply_to_fresh(&payload), "seeded");
    }
}
