//! Collab document routes.
//!
//! A collab doc is the one server-visible object kind: its content (a Y.Doc) is
//! stored plaintext in `collab_docs.yjs_state` so the server can relay CRDT
//! updates in Phase 3. Each collab doc has a matching `objects` row with
//! `kind = 'collab'`, `status = 'complete'`, the ciphertext columns null, and
//! `collab_doc_id` pointing at the `collab_docs` row (the objects XOR check
//! enforces that split). These handlers cover Phase 2 CRUD only — no Y-sync, no
//! `yjs_state` read/write.
//!
//! The auth, event-log, and broadcast patterns mirror `routes::objects`.

use axum::{
    Json,
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use clipper_core::{
    crypto,
    models::{
        ApiErrorCode, CollabDocMeta, CreateCollabDocResponse, ObjectEventType, ObjectKind,
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DerivePartialModel, EntityTrait, QueryFilter, Set,
};
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::{
    auth::AuthInfo,
    entity::{collab_docs, event_log, objects},
    routes::{ApiError, with_txn},
    state::AppState,
    ws::WsBroadcast,
};

/// Bytes of randomness in a `share_token`. The token is the sole credential for
/// unauthenticated share-link access (Phase 3), so it must be unguessable; 32
/// bytes (256 bits) matches the WebSocket ticket secret.
const SHARE_TOKEN_BYTES: usize = 32;

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "objects::Entity", from_query_result)]
struct CollabObjectRow {
    user_id: Uuid,
    collab_doc_id: Option<Uuid>,
}

/// `POST /api/collab-docs` — create a collab doc for the authenticated user.
///
/// The request carries no required fields (the title lives inside the Y.Doc), so
/// any body is ignored. Inserts a `collab_docs` row and its `kind = 'collab'`
/// `objects` row in one transaction, emits a `created` event, and returns the
/// new object id plus the random share token.
pub async fn create_collab_doc(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
) -> Result<impl IntoResponse, ApiError> {
    let collab_doc_id = Uuid::now_v7();
    let object_id = Uuid::now_v7();
    let share_token = URL_SAFE_NO_PAD.encode(crypto::generate_random_bytes(SHARE_TOKEN_BYTES));
    let now = Utc::now().to_rfc3339();

    let user_id = auth.user_id;
    let device_id = auth.device_id;
    let share_token_for_row = share_token.clone();
    let now_ref = now.as_str();
    let state_ref = &state;

    let seq = with_txn(state.db(), "create_collab_doc", async move |txn| {
        // The collab_docs insert is the first write, so it takes the SQLite write
        // lock. Allocate the seq only after that, matching the event_log.seq
        // boundary rule (seq order must match commit order).
        collab_docs::ActiveModel {
            id: Set(collab_doc_id),
            owner_user_id: Set(user_id),
            share_token: Set(share_token_for_row),
            yjs_state: Set(None),
            created_at: Set(now_ref.to_owned()),
            updated_at: Set(now_ref.to_owned()),
        }
        .insert(txn)
        .await
        .map_err(|e| {
            error!(
                collab_doc_id = %collab_doc_id,
                user_id = %user_id,
                error = %e,
                "Failed to insert collab_docs row",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

        let seq = state_ref.next_event_seq();

        // A collab object: ciphertext columns null, collab_doc_id set, status
        // complete from the start (no upload phase). created_seq is set here so
        // the `status = 'complete' => created_seq NOT NULL` check holds.
        objects::ActiveModel {
            id: Set(object_id),
            user_id: Set(user_id),
            kind: Set(ObjectKind::Collab.to_string()),
            meta_ciphertext: Set(None),
            meta_nonce: Set(None),
            created_at: Set(now_ref.to_owned()),
            updated_at: Set(now_ref.to_owned()),
            expires_at: Set(None),
            source_device_id: Set(Some(device_id)),
            envelope: Set(None),
            status: Set("complete".into()),
            created_seq: Set(Some(seq)),
            collab_doc_id: Set(Some(collab_doc_id)),
        }
        .insert(txn)
        .await
        .map_err(|e| {
            error!(
                object_id = %object_id,
                user_id = %user_id,
                error = %e,
                "Failed to insert collab object row",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

        event_log::ActiveModel {
            seq: Set(seq),
            user_id: Set(user_id),
            event_type: Set(ObjectEventType::Created.to_string()),
            object_kind: Set(ObjectKind::Collab.to_string()),
            object_id: Set(object_id),
            created_at: Set(now_ref.to_owned()),
        }
        .insert(txn)
        .await
        .map_err(|e| {
            error!(
                object_id = %object_id,
                user_id = %user_id,
                error = %e,
                "Failed to insert collab created event",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

        Ok(seq)
    })
    .await?;

    state.broadcast_ws_event(WsBroadcast {
        user_id,
        source_device_id: device_id,
        seq,
        event_type: ObjectEventType::Created,
        object_kind: ObjectKind::Collab,
        object_id: object_id.into(),
        created_at: now,
    });

    info!(device_id = %device_id, object_id = %object_id, "Collab doc created");

    Ok((
        StatusCode::CREATED,
        Json(CreateCollabDocResponse {
            object_id: object_id.into(),
            share_token,
        }),
    ))
}

/// `GET /api/collab-docs/:id/meta` — return the share token and `updated_at` for
/// the authenticated owner's collab doc. Does not return `yjs_state` (that flows
/// over the Y-sync WebSocket in Phase 3).
pub async fn get_collab_doc_meta(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path(object_id): Path<String>,
) -> Result<Json<CollabDocMeta>, ApiError> {
    let object_uuid =
        Uuid::parse_str(&object_id).map_err(|_| ApiError::from_code(ApiErrorCode::InvalidId))?;

    let collab_doc_id = load_owned_collab_doc_id(&state, auth.user_id, object_uuid).await?;

    let doc = collab_docs::Entity::find_by_id(collab_doc_id)
        .into_partial_model::<CollabDocMetaRow>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                collab_doc_id = %collab_doc_id,
                error = %e,
                "Failed to load collab_docs row for meta",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .ok_or_else(|| {
            // The objects row pointed at a collab_docs row that no longer exists;
            // the FK is ON DELETE CASCADE, so this is a data inconsistency.
            error!(
                object_id = %object_uuid,
                collab_doc_id = %collab_doc_id,
                "Collab object references a missing collab_docs row",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

    Ok(Json(CollabDocMeta {
        object_id: object_uuid.into(),
        share_token: doc.share_token,
        updated_at: doc.updated_at,
    }))
}

/// `DELETE /api/collab-docs/:id` — delete the authenticated owner's collab doc.
///
/// Deletes the `objects` row, which cascades to `collab_docs` via the
/// `collab_doc_id` FK. Emits a `deleted` event and broadcasts it. Returns 204.
pub async fn delete_collab_doc(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path(object_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let object_uuid =
        Uuid::parse_str(&object_id).map_err(|_| ApiError::from_code(ApiErrorCode::InvalidId))?;

    // 404 if missing, 403 if owned by another user.
    load_owned_collab_doc_id(&state, auth.user_id, object_uuid).await?;

    let user_id = auth.user_id;
    let now = Utc::now().to_rfc3339();
    let now_ref = now.as_str();
    let state_ref = &state;

    let seq = with_txn(state.db(), "delete_collab_doc", async move |txn| {
        let deleted = objects::Entity::delete_by_id(object_uuid)
            .exec(txn)
            .await
            .map_err(|e| {
                error!(
                    object_id = %object_uuid,
                    user_id = %user_id,
                    error = %e,
                    "Failed to delete collab object row",
                );
                ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
            })?;
        if deleted.rows_affected != 1 {
            // The pre-check above found the row; a concurrent delete lost the
            // race. Treat it as not found rather than emitting a phantom event.
            debug!(
                object_id = %object_uuid,
                user_id = %user_id,
                rows_affected = deleted.rows_affected,
                "Collab delete affected no rows (concurrent delete)",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::ObjectNotFound,
                "Object not found",
            ));
        }

        // Allocated after the delete above has taken the write lock.
        let seq = state_ref.next_event_seq();
        event_log::ActiveModel {
            seq: Set(seq),
            user_id: Set(user_id),
            event_type: Set(ObjectEventType::Deleted.to_string()),
            object_kind: Set(ObjectKind::Collab.to_string()),
            object_id: Set(object_uuid),
            created_at: Set(now_ref.to_owned()),
        }
        .insert(txn)
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                user_id = %user_id,
                error = %e,
                "Failed to insert collab deleted event",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

        Ok(seq)
    })
    .await?;

    state.broadcast_ws_event(WsBroadcast {
        user_id,
        source_device_id: auth.device_id,
        seq,
        event_type: ObjectEventType::Deleted,
        object_kind: ObjectKind::Collab,
        object_id: object_uuid.into(),
        created_at: now,
    });

    info!(device_id = %auth.device_id, object_id = %object_uuid, "Collab doc deleted");

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "collab_docs::Entity", from_query_result)]
struct CollabDocMetaRow {
    share_token: String,
    updated_at: String,
}

/// Look up a collab object by id and return its `collab_doc_id`, enforcing
/// ownership: 404 if no such collab object exists, 403 if it belongs to another
/// user. Scoping the existence check by id alone (not user) keeps a cross-user
/// id from acting as an existence oracle only at the cost of a 403 vs 404
/// distinction the owner already knows; the spec asks for 403 on mismatch.
async fn load_owned_collab_doc_id(
    state: &AppState,
    user_id: Uuid,
    object_id: Uuid,
) -> Result<Uuid, ApiError> {
    let object = objects::Entity::find_by_id(object_id)
        .filter(objects::Column::Kind.eq(ObjectKind::Collab.as_ref()))
        .into_partial_model::<CollabObjectRow>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_id,
                user_id = %user_id,
                error = %e,
                "Failed to look up collab object",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .ok_or_else(|| {
            debug!(
                object_id = %object_id,
                user_id = %user_id,
                "Collab object not found",
            );
            ApiError::from_code_with_message(ApiErrorCode::ObjectNotFound, "Object not found")
        })?;

    if object.user_id != user_id {
        debug!(
            object_id = %object_id,
            user_id = %user_id,
            owner_user_id = %object.user_id,
            "Rejected collab doc access from non-owner",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectForbidden,
            "Forbidden",
        ));
    }

    object.collab_doc_id.ok_or_else(|| {
        // A `kind = 'collab'` object must have collab_doc_id set (objects XOR
        // check), so a null here is a data inconsistency.
        error!(
            object_id = %object_id,
            user_id = %user_id,
            "Collab object row is missing collab_doc_id",
        );
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })
}
