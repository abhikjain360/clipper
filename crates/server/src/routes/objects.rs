//! Generic encrypted object routes.

use std::collections::HashMap;

use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
};
use chrono::{Duration, Utc};
use clipper_core::{
    crypto::{self, SHA256_BYTES},
    models::{
        ApiErrorCode, ObjectCompleteRequest, ObjectCompleteResponse, ObjectDeleteResponse,
        ObjectEnvelopeOperation, ObjectEventType, ObjectId, ObjectInitRequest, ObjectInitResponse,
        ObjectKind, ObjectListCursor, ObjectListItem, ObjectListResponse, ObjectPayloadDescriptor,
        ObjectPayloadInit, ObjectPayloadUpload, OkResponse,
    },
};
use clipper_fs_txn::FsTransaction;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DbErr, DerivePartialModel, EntityTrait, Order,
    QueryFilter, QueryOrder, QuerySelect, Set, SqlErr, TransactionTrait,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    auth::AuthInfo,
    entity::{devices, event_log, object_payloads, objects},
    routes::{ApiError, Postcard, with_txn},
    state::AppState,
    ws::WsBroadcast,
};

pub async fn init_object(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Postcard(req): Postcard<ObjectInitRequest>,
) -> Result<Postcard<ObjectInitResponse>, ApiError> {
    let object_id = req.id.into_uuid();
    let object_id_text = req.id.to_string();
    if req.meta_ciphertext.len() > state.config().limits.max_object_meta_ciphertext_bytes {
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::PayloadTooLarge,
            "Object metadata ciphertext exceeds maximum size",
        ));
    }

    // Bound each payload's declared ciphertext size. This is the only place the
    // size is gated: `upload_payload` streams up to the stored `ciphertext_size`,
    // so rejecting an oversized declaration here keeps every downstream write
    // (inline and streamed) under the configured ceiling.
    let max_blob_bytes = state.config().limits.max_file_blob_bytes;
    for payload in &req.payloads {
        // `ciphertext_size` is garde-validated `>= 0`, so the cast is lossless.
        if payload.ciphertext_size as u64 > max_blob_bytes {
            debug!(
                object_id = %object_id,
                payload_id = %payload.id,
                declared_size = payload.ciphertext_size,
                max_blob_bytes,
                "Rejected object init with oversized payload",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::PayloadTooLarge,
                "Object payload exceeds maximum size",
            ));
        }
    }

    validate_object_init_envelope(&state, auth.user_id, auth.device_id, &req).await?;
    let envelope_bytes = postcard::to_allocvec(&req.envelope).map_err(|e| {
        error!(object_id = %object_id, error = %e, "Failed to encode object envelope");
        ApiError::from_code_with_message(
            ApiErrorCode::InvalidObjectEnvelope,
            "Invalid object envelope",
        )
    })?;

    if let Some(existing) = objects::Entity::find_by_id(object_id)
        .one(state.db())
        .await
        .map_err(|e| {
            error!(object_id = %object_id, error = %e, "Failed to look up object in init_object");
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
    {
        let resp = idempotent_init_response(
            &state,
            auth.user_id,
            auth.device_id,
            &req,
            &envelope_bytes,
            existing,
        )
        .await?;
        return Ok(Postcard(resp));
    }

    let mut all_inline = true;
    // Inline payload files are written before the transaction. `staged` removes
    // them on drop, so any early return below (including a failed write or a
    // rolled-back transaction) cleans them up without explicit bookkeeping.
    let mut staged = FsTransaction::new();
    for payload in &req.payloads {
        let Some(inline_ciphertext) = &payload.inline_ciphertext else {
            all_inline = false;
            continue;
        };

        let payload_id = payload.id.to_string();
        let path = state
            .objects_dir()
            .join(object_payload_filename(&object_id_text, &payload_id));
        staged
            .write_new(&path, inline_ciphertext)
            .await
            .map_err(|e| {
                error!(
                    object_id = %object_id,
                    payload_id = %payload_id,
                    path = %path.display(),
                    error = %e,
                    "Failed to write inline payload to disk",
                );
                ApiError::from_code_with_message(
                    ApiErrorCode::Storage,
                    "Object payload storage error",
                )
            })?;
    }
    let all_inline = all_inline;

    let created_at = req.envelope.body.created_at.clone();
    chrono::DateTime::parse_from_rfc3339(&created_at).map_err(|e| {
        debug!(
            object_id = %object_id,
            created_at = %created_at,
            error = %e,
            "Rejected object envelope with invalid created_at",
        );
        ApiError::from_code_with_message(
            ApiErrorCode::InvalidObjectEnvelope,
            "Invalid object envelope created_at",
        )
    })?;
    let updated_at = Utc::now().to_rfc3339();
    let expires_at = match req.kind {
        ObjectKind::Clipboard => {
            let created = chrono::DateTime::parse_from_rfc3339(&created_at).ok();
            created.map(|created| {
                (created + Duration::days(state.config().clipboard.ttl_days)).to_rfc3339()
            })
        }
        ObjectKind::File => None,
    };
    // Response data derived from the request, computed before the request is
    // moved into the transaction closure.
    let kind = req.kind;
    let user_id = auth.user_id;
    let device_id = auth.device_id;
    let upload_urls: Vec<ObjectPayloadUpload> = req
        .payloads
        .iter()
        .filter(|p| p.inline_ciphertext.is_none())
        .map(|p| ObjectPayloadUpload {
            id: p.id,
            upload_url: format!("/api/objects/{}/payloads/{}", object_id_text, p.id),
        })
        .collect();
    let payload_count = req.payloads.len();
    let payload_models: Vec<_> = req
        .payloads
        .iter()
        .map(|payload| {
            let payload_id = payload.id.to_string();
            object_payloads::ActiveModel {
                object_id: Set(object_id),
                payload_id: Set(payload.id.into_uuid()),
                ciphertext_path: Set(object_payload_filename(&object_id_text, &payload_id)),
                nonce: Set(payload.nonce.clone()),
                ciphertext_size: Set(payload.ciphertext_size),
                sha256_ciphertext: Set(payload.sha256_ciphertext.clone()),
                created_at: Set(created_at.clone()),
                updated_at: Set(updated_at.clone()),
                status: Set(if payload.inline_ciphertext.is_some() {
                    "complete"
                } else {
                    "pending"
                }
                .into()),
            }
        })
        .collect();

    let created_at_str = created_at.as_str();
    let updated_at_str = updated_at.as_str();
    let state_ref = &state;
    let inserted_event = with_txn(state.db(), "init_object", async move |txn| {
        let object = objects::ActiveModel {
            id: Set(object_id),
            user_id: Set(user_id),
            kind: Set(kind.to_string()),
            meta_ciphertext: Set(req.meta_ciphertext),
            meta_nonce: Set(req.meta_nonce),
            created_at: Set(created_at_str.to_owned()),
            updated_at: Set(updated_at_str.to_owned()),
            expires_at: Set(expires_at),
            source_device_id: Set(device_id),
            envelope: Set(envelope_bytes),
            status: Set("pending".into()),
            created_seq: Set(None),
        };
        object.insert(txn).await.map_err(|e| match e.sql_err() {
            Some(SqlErr::UniqueConstraintViolation(constraint)) => {
                warn!(
                    object_id = %object_id,
                    user_id = %user_id,
                    constraint = %constraint,
                    "Concurrent init_object lost a race on object id uniqueness",
                );
                ApiError::from_code_with_message(
                    ApiErrorCode::ObjectAlreadyExists,
                    "Object already exists",
                )
            }
            _ => {
                error!(object_id = %object_id, error = %e, "Failed to insert object row");
                ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
            }
        })?;

        let inserted_payloads = object_payloads::Entity::insert_many(payload_models)
            .exec_without_returning(txn)
            .await
            .map_err(|e| map_payload_batch_insert_error(e, object_id))?;
        if inserted_payloads != payload_count as u64 {
            error!(
                object_id = %object_id,
                expected_payloads = payload_count,
                inserted_payloads,
                "Object payload batch insert affected an unexpected row count",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::Database,
                "Database error",
            ));
        }

        // Allocated here, after the object/payload inserts above have taken the
        // write lock, so seq order matches commit order.
        if all_inline {
            let seq = state_ref.next_event_seq();
            let inserted =
                insert_created_event(txn, user_id, kind, object_id, created_at_str, seq).await?;
            set_object_created_seq(txn, user_id, object_id, seq).await?;
            Ok(Some(inserted))
        } else {
            Ok(None)
        }
    })
    .await?;

    // The transaction committed; keep the inline payload files on disk.
    staged.commit();

    let response = if let Some(inserted) = inserted_event.as_ref() {
        ObjectInitResponse::Complete {
            created_seq: inserted.seq,
        }
    } else {
        ObjectInitResponse::Pending { upload_urls }
    };
    if let Some(inserted) = inserted_event {
        broadcast_created(
            &state,
            user_id,
            device_id,
            inserted.seq,
            kind,
            &object_id_text,
            &created_at,
        );
        if kind == ObjectKind::Clipboard {
            spawn_clipboard_trim(state.clone(), user_id);
        }
    }

    info!(device_id = %device_id, object_id = %object_id_text, kind = kind.as_ref(), "Object initialized");

    Ok(Postcard(response))
}

pub async fn upload_payload(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path((object_id, payload_id)): Path<(String, String)>,
    body: Body,
) -> Result<Postcard<OkResponse>, ApiError> {
    let object_uuid =
        Uuid::parse_str(&object_id).map_err(|_| ApiError::from_code(ApiErrorCode::InvalidId))?;
    let payload_uuid =
        Uuid::parse_str(&payload_id).map_err(|_| ApiError::from_code(ApiErrorCode::InvalidId))?;
    let object = object_for_upload(&state, auth.user_id, auth.device_id, object_uuid).await?;

    let payload = object_payloads::Entity::find_by_id((object_uuid, payload_uuid))
        .into_partial_model::<PayloadUploadRow>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                payload_id = %payload_uuid,
                error = %e,
                "Failed to look up object payload for upload",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .ok_or_else(|| {
            debug!(
                object_id = %object_uuid,
                payload_id = %payload_uuid,
                "Rejected upload for missing object payload",
            );
            ApiError::from_code_with_message(
                ApiErrorCode::ObjectPayloadNotFound,
                "Object payload not found",
            )
        })?;

    if payload.status == "uploaded" || payload.status == "complete" {
        debug!(
            object_id = %object_uuid,
            payload_id = %payload_uuid,
            status = %payload.status,
            "Accepted idempotent upload for already uploaded payload",
        );
        return Ok(Postcard(OkResponse {}));
    }

    if payload.status != "pending" {
        debug!(
            object_id = %object_uuid,
            payload_id = %payload_uuid,
            status = %payload.status,
            "Rejected upload for payload that is not pending",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectPayloadAlreadyUploaded,
            "Object payload already uploaded",
        ));
    }

    if payload.ciphertext_size < 0 {
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::InvalidPayloadSize,
            "Invalid payload size",
        ));
    }
    let expected_size = payload.ciphertext_size as u64;
    let now = Utc::now().to_rfc3339();
    let claimed = object_payloads::Entity::update_many()
        .col_expr(
            object_payloads::Column::Status,
            sea_orm::sea_query::Expr::value("uploading"),
        )
        .col_expr(
            object_payloads::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(object_payloads::Column::ObjectId.eq(object_uuid))
        .filter(object_payloads::Column::PayloadId.eq(payload_uuid))
        .filter(object_payloads::Column::Status.eq("pending"))
        .exec(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                payload_id = %payload_uuid,
                error = %e,
                "Failed to claim payload for upload",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

    if claimed.rows_affected != 1 {
        warn!(
            object_id = %object_uuid,
            payload_id = %payload_uuid,
            "Payload upload claim failed because status was no longer pending",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectPayloadUploadInProgress,
            "Object payload upload in progress",
        ));
    }

    let final_path = state.objects_dir().join(&payload.ciphertext_path);
    let tmp_path = state.objects_dir().join(format!(
        "{}.{}.tmp",
        payload.ciphertext_path,
        uuid::Uuid::now_v7()
    ));

    if let Err(response) = stream_body_to_payload_file(body, expected_size, &tmp_path).await {
        _ = tokio::fs::remove_file(&tmp_path).await;
        reset_payload_status(&state, object_uuid, payload_uuid, "uploading", "pending").await;
        return Err(response);
    }

    _ = tokio::fs::remove_file(&final_path).await;
    if let Err(e) = tokio::fs::rename(&tmp_path, &final_path).await {
        error!(
            object_id = %object_uuid,
            payload_id = %payload_uuid,
            tmp = %tmp_path.display(),
            dest = %final_path.display(),
            error = %e,
            "Failed to rename tmp payload to final path",
        );
        _ = tokio::fs::remove_file(&tmp_path).await;
        reset_payload_status(&state, object_uuid, payload_uuid, "uploading", "pending").await;
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::Storage,
            "Object payload storage error",
        ));
    }

    let now = Utc::now().to_rfc3339();
    let uploaded = object_payloads::Entity::update_many()
        .col_expr(
            object_payloads::Column::Status,
            sea_orm::sea_query::Expr::value("uploaded"),
        )
        .col_expr(
            object_payloads::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(object_payloads::Column::ObjectId.eq(object_uuid))
        .filter(object_payloads::Column::PayloadId.eq(payload_uuid))
        .filter(object_payloads::Column::Status.eq("uploading"))
        .exec(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                payload_id = %payload_uuid,
                error = %e,
                "Failed to mark payload uploaded",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

    if uploaded.rows_affected != 1 {
        _ = tokio::fs::remove_file(&final_path).await;
        warn!(
            object_id = %object_uuid,
            payload_id = %payload_uuid,
            "Payload upload finalization failed because status was no longer uploading",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectPayloadUploadInProgress,
            "Object payload upload no longer in progress",
        ));
    }

    info!(device_id = %auth.device_id, object_id = %object.id, payload_id = %payload_id, "Object payload uploaded");
    Ok(Postcard(OkResponse {}))
}

pub async fn complete_object(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path(object_id): Path<String>,
    Postcard(req): Postcard<ObjectCompleteRequest>,
) -> Result<Postcard<ObjectCompleteResponse>, ApiError> {
    let object_uuid =
        Uuid::parse_str(&object_id).map_err(|_| ApiError::from_code(ApiErrorCode::InvalidId))?;
    let object = object_for_upload(&state, auth.user_id, auth.device_id, object_uuid).await?;
    let kind = parse_object_kind(object_uuid, &object.kind)?;

    if object.status == "complete" {
        let created_seq = object.created_seq.ok_or_else(|| {
            error!(
                object_id = %object_uuid,
                user_id = %auth.user_id,
                "Complete object is missing created_seq",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;
        debug!(
            object_id = %object_uuid,
            user_id = %auth.user_id,
            device_id = %auth.device_id,
            "Accepted idempotent complete_object for already complete object",
        );
        return Ok(Postcard(ObjectCompleteResponse { created_seq }));
    }

    let payloads = object_payloads::Entity::find()
        .filter(object_payloads::Column::ObjectId.eq(object_uuid))
        .into_partial_model::<PayloadCompletionRow>()
        .all(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                error = %e,
                "Failed to list payloads in complete_object",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

    if payloads.is_empty() {
        debug!(
            object_id = %object_uuid,
            "Rejected complete_object for object with no payload rows",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::MissingObjectPayloads,
            "Missing object payloads",
        ));
    }

    let mut completion_by_id = HashMap::new();
    for payload in &req.payloads {
        if completion_by_id
            .insert(payload.id.into_uuid(), payload)
            .is_some()
        {
            warn!(
                object_id = %object_uuid,
                payload_id = %payload.id,
                "Duplicate payload completion in complete_object request",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::DuplicateObjectPayloadId,
                "Duplicate payload id",
            ));
        }
    }

    if completion_by_id.len() != payloads.len() {
        debug!(
            object_id = %object_uuid,
            expected_payloads = payloads.len(),
            completed_payloads = completion_by_id.len(),
            "Rejected incomplete complete_object request",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::IncompletePayloadCompletion,
            "Complete request does not cover all object payloads",
        ));
    }

    for payload in &payloads {
        let complete = completion_by_id.get(&payload.payload_id).ok_or_else(|| {
            debug!(
                object_id = %object_uuid,
                payload_id = %payload.payload_id,
                "Rejected complete_object request missing payload completion",
            );
            ApiError::from_code_with_message(
                ApiErrorCode::MissingPayloadCompletion,
                "Missing payload completion",
            )
        })?;
        if payload.status != "uploaded" && payload.status != "complete" {
            debug!(
                object_id = %object_uuid,
                payload_id = %payload.payload_id,
                status = %payload.status,
                "Rejected complete_object before payload upload finished",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::ObjectPayloadNotUploaded,
                "Object payload has not been uploaded",
            ));
        }
        if complete.ciphertext_size != payload.ciphertext_size
            || complete.sha256_ciphertext.as_slice() != payload.sha256_ciphertext.as_slice()
        {
            debug!(
                object_id = %object_uuid,
                payload_id = %payload.payload_id,
                expected_size = payload.ciphertext_size,
                completed_size = complete.ciphertext_size,
                "Rejected complete_object with mismatched payload metadata",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::ObjectPayloadMetadataMismatch,
                "Payload metadata does not match initialized values",
            ));
        }

        let path = state.objects_dir().join(&payload.ciphertext_path);
        let (computed_hash, actual_size) = sha256_file(&path).await.map_err(|e| {
            error!(
                object_id = %object_uuid,
                payload_id = %payload.payload_id,
                path = %path.display(),
                error = %e,
                error_kind = ?e.kind(),
                "Failed to hash uploaded payload (file missing or unreadable)",
            );
            ApiError::from_code_with_message(ApiErrorCode::PayloadRead, "Payload read error")
        })?;
        if actual_size != payload.ciphertext_size as u64
            || computed_hash.as_slice() != payload.sha256_ciphertext.as_slice()
        {
            warn!(
                object_id = %object_uuid,
                payload_id = %payload.payload_id,
                expected_size = payload.ciphertext_size,
                actual_size,
                "Uploaded payload failed completion integrity check",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::ObjectPayloadIntegrityMismatch,
                "Payload size or SHA-256 mismatch",
            ));
        }
    }

    let now = Utc::now().to_rfc3339();
    let txn = state.db().begin().await.map_err(|e| {
        error!(error = %e, "Failed to begin complete_object transaction");
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })?;
    let created_seq = state.next_event_seq();

    object_payloads::Entity::update_many()
        .col_expr(
            object_payloads::Column::Status,
            sea_orm::sea_query::Expr::value("complete"),
        )
        .col_expr(
            object_payloads::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now.clone()),
        )
        .filter(object_payloads::Column::ObjectId.eq(object_uuid))
        .exec(&txn)
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                error = %e,
                "Failed to mark payloads complete",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

    let updated = objects::Entity::update_many()
        .col_expr(
            objects::Column::Status,
            sea_orm::sea_query::Expr::value("complete"),
        )
        .col_expr(
            objects::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now.clone()),
        )
        .col_expr(
            objects::Column::CreatedSeq,
            sea_orm::sea_query::Expr::value(created_seq),
        )
        .filter(objects::Column::Id.eq(object_uuid))
        .filter(objects::Column::UserId.eq(auth.user_id))
        .filter(objects::Column::Status.eq("pending"))
        .filter(objects::Column::SourceDeviceId.eq(auth.device_id))
        .exec(&txn)
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                error = %e,
                "Failed to mark object complete",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

    if updated.rows_affected != 1 {
        _ = txn.rollback().await;
        warn!(
            object_id = %object_uuid,
            user_id = %auth.user_id,
            device_id = %auth.device_id,
            "Object completion update affected no rows",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectNotReadyToComplete,
            "Object is no longer ready to complete",
        ));
    }

    let inserted =
        insert_created_event(&txn, auth.user_id, kind, object_uuid, &now, created_seq).await?;

    txn.commit().await.map_err(|e| {
        error!(
            object_id = %object_uuid,
            error = %e,
            "Failed to commit complete_object transaction",
        );
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })?;

    broadcast_created(
        &state,
        auth.user_id,
        auth.device_id,
        inserted.seq,
        kind,
        &object_id,
        &now,
    );
    if kind == ObjectKind::Clipboard {
        spawn_clipboard_trim(state.clone(), auth.user_id);
    }

    info!(device_id = %auth.device_id, object_id = %object_id, kind = kind.as_ref(), "Object completed");
    Ok(Postcard(ObjectCompleteResponse { created_seq }))
}

#[derive(Debug)]
pub struct ObjectListQuery {
    pub kind: Option<String>,
    pub limit: Option<u64>,
    pub created_seq_lte: Option<i64>,
    pub after: Option<ObjectListCursor>,
}

#[derive(Debug, serde::Deserialize)]
struct ObjectListQueryWire {
    kind: Option<String>,
    limit: Option<u64>,
    created_seq_lte: Option<i64>,
    after_created_seq: Option<i64>,
    after_id: Option<ObjectId>,
}

impl<'de> serde::Deserialize<'de> for ObjectListQuery {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ObjectListQueryWire::deserialize(deserializer)?;
        let after = match (wire.after_created_seq, wire.after_id) {
            (Some(created_seq), Some(id)) => Some(ObjectListCursor { created_seq, id }),
            (None, None) => None,
            _ => {
                return Err(serde::de::Error::custom(
                    "after_created_seq and after_id must be provided together",
                ));
            }
        };
        Ok(Self {
            kind: wire.kind,
            limit: wire.limit,
            created_seq_lte: wire.created_seq_lte,
            after,
        })
    }
}

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "objects::Entity", from_query_result)]
struct ListedObjectRow {
    id: Uuid,
    kind: String,
    created_seq: Option<i64>,
    meta_ciphertext: Vec<u8>,
    meta_nonce: Vec<u8>,
    created_at: String,
    source_device_id: Uuid,
    envelope: Vec<u8>,
}

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "object_payloads::Entity", from_query_result)]
struct ListedPayloadRow {
    object_id: Uuid,
    payload_id: Uuid,
    nonce: Vec<u8>,
    ciphertext_size: i64,
    sha256_ciphertext: Vec<u8>,
}

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "objects::Entity", from_query_result)]
struct ObjectUploadRow {
    id: Uuid,
    kind: String,
    source_device_id: Uuid,
    status: String,
    created_seq: Option<i64>,
}

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "object_payloads::Entity", from_query_result)]
struct PayloadUploadRow {
    ciphertext_path: String,
    ciphertext_size: i64,
    status: String,
}

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "object_payloads::Entity", from_query_result)]
struct PayloadCompletionRow {
    payload_id: Uuid,
    ciphertext_path: String,
    ciphertext_size: i64,
    sha256_ciphertext: Vec<u8>,
    status: String,
}

pub async fn list_objects(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Query(query): Query<ObjectListQuery>,
) -> Result<Postcard<ObjectListResponse>, ApiError> {
    let limit = query
        .limit
        .unwrap_or(state.config().list.default_limit)
        .min(state.config().list.max_limit);
    let kind = query
        .kind
        .as_deref()
        .map(|kind| {
            kind.parse::<ObjectKind>().map_err(|_| {
                debug!(kind, "Rejected unknown object kind in list query");
                ApiError::from_code(ApiErrorCode::InvalidObjectKind)
            })
        })
        .transpose()?;
    let after = query.after;

    let mut q = objects::Entity::find()
        .filter(objects::Column::UserId.eq(auth.user_id))
        .filter(objects::Column::Status.eq("complete"))
        .filter(objects::Column::CreatedSeq.is_not_null());

    if let Some(kind) = kind {
        q = q.filter(objects::Column::Kind.eq(kind.as_ref()));
        if kind == ObjectKind::Clipboard {
            let retained_ids = retained_clipboard_object_ids(&state, auth.user_id).await?;
            if retained_ids.is_empty() {
                return Ok(Postcard(ObjectListResponse {
                    items: Vec::new(),
                    next_after: None,
                }));
            }
            q = q.filter(objects::Column::Id.is_in(retained_ids));
        }
    }

    if let Some(created_seq_lte) = query.created_seq_lte {
        q = q.filter(objects::Column::CreatedSeq.lte(created_seq_lte));
    }

    if let Some(after) = after {
        q = q.filter(
            Condition::any()
                .add(objects::Column::CreatedSeq.gt(after.created_seq))
                .add(
                    Condition::all()
                        .add(objects::Column::CreatedSeq.eq(after.created_seq))
                        .add(objects::Column::Id.gt(after.id.into_uuid())),
                ),
        );
    }

    let uses_forward_cursor = query.created_seq_lte.is_some() || query.after.is_some();
    q = if uses_forward_cursor {
        q.order_by(objects::Column::CreatedSeq, Order::Asc)
            .order_by(objects::Column::Id, Order::Asc)
    } else {
        q.order_by(objects::Column::CreatedSeq, Order::Desc)
            .order_by(objects::Column::Id, Order::Desc)
    };

    let objects = q
        .limit(limit + 1)
        .into_partial_model::<ListedObjectRow>()
        .all(state.db())
        .await
        .map_err(|e| {
            error!(
                user_id = %auth.user_id,
                error = %e,
                "Failed to list objects",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

    let has_more = objects.len() as u64 > limit;
    let objects: Vec<ListedObjectRow> = objects.into_iter().take(limit as usize).collect();
    let items = object_list_items(&state, auth.user_id, &objects).await?;
    let next_after = if has_more && uses_forward_cursor {
        let last = objects.last().ok_or_else(|| {
            error!(
                user_id = %auth.user_id,
                "Object list had more rows than requested but no cursor row",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;
        Some(ObjectListCursor {
            created_seq: last.created_seq.ok_or_else(|| {
                error!(
                    object_id = %last.id,
                    user_id = %auth.user_id,
                    "Listed object is missing created_seq",
                );
                ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
            })?,
            id: ObjectId::from(last.id),
        })
    } else {
        None
    };

    debug!(
        user_id = %auth.user_id,
        device_id = %auth.device_id,
        kind = query.kind.as_deref().unwrap_or("<all>"),
        limit,
        items = items.len(),
        has_more,
        "Listed objects",
    );

    Ok(Postcard(ObjectListResponse { items, next_after }))
}

pub async fn get_object(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path(object_id): Path<String>,
) -> Result<Postcard<ObjectListItem>, ApiError> {
    let object_uuid =
        Uuid::parse_str(&object_id).map_err(|_| ApiError::from_code(ApiErrorCode::InvalidId))?;
    let object = objects::Entity::find_by_id(object_uuid)
        .filter(objects::Column::UserId.eq(auth.user_id))
        .filter(objects::Column::Status.eq("complete"))
        .filter(objects::Column::CreatedSeq.is_not_null())
        .into_partial_model::<ListedObjectRow>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                user_id = %auth.user_id,
                error = %e,
                "Failed to load object by id",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .ok_or_else(|| {
            debug!(
                object_id = %object_uuid,
                user_id = %auth.user_id,
                "Object by id not found",
            );
            ApiError::from_code_with_message(ApiErrorCode::ObjectNotFound, "Object not found")
        })?;
    ensure_object_read_retained(&state, auth.user_id, object_uuid, &object.kind).await?;
    let mut items = object_list_items(&state, auth.user_id, &[object]).await?;
    let item = items.pop().ok_or_else(|| {
        error!(
            object_id = %object_uuid,
            user_id = %auth.user_id,
            "Object list item helper returned no rows for targeted get",
        );
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })?;
    Ok(Postcard(item))
}

async fn retained_clipboard_object_ids(
    state: &AppState,
    user_id: Uuid,
) -> Result<Vec<Uuid>, ApiError> {
    let now = Utc::now().to_rfc3339();
    objects::Entity::find()
        .filter(objects::Column::UserId.eq(user_id))
        .filter(objects::Column::Kind.eq(ObjectKind::Clipboard.as_ref()))
        .filter(objects::Column::Status.eq("complete"))
        .filter(objects::Column::CreatedSeq.is_not_null())
        .filter(
            Condition::any()
                .add(objects::Column::ExpiresAt.is_null())
                .add(objects::Column::ExpiresAt.gt(now)),
        )
        .order_by(objects::Column::CreatedSeq, Order::Desc)
        .order_by(objects::Column::Id, Order::Desc)
        .limit(state.config().clipboard.max_items)
        .select_only()
        .column(objects::Column::Id)
        .into_tuple()
        .all(state.db())
        .await
        .map_err(|e| {
            error!(
                user_id = %user_id,
                error = %e,
                "Failed to load retained clipboard object ids",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })
}

async fn ensure_object_read_retained(
    state: &AppState,
    user_id: Uuid,
    object_id: Uuid,
    kind: &str,
) -> Result<(), ApiError> {
    if kind != ObjectKind::Clipboard.as_ref() {
        return Ok(());
    }

    if retained_clipboard_object_ids(state, user_id)
        .await?
        .contains(&object_id)
    {
        Ok(())
    } else {
        Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectNotFound,
            "Object not found",
        ))
    }
}

async fn object_list_items(
    state: &AppState,
    user_id: Uuid,
    objects: &[ListedObjectRow],
) -> Result<Vec<ObjectListItem>, ApiError> {
    let object_ids: Vec<Uuid> = objects.iter().map(|object| object.id).collect();
    let payloads = if object_ids.is_empty() {
        Vec::new()
    } else {
        object_payloads::Entity::find()
            .filter(object_payloads::Column::ObjectId.is_in(object_ids))
            .filter(object_payloads::Column::Status.eq("complete"))
            .order_by(object_payloads::Column::ObjectId, Order::Asc)
            .order_by(object_payloads::Column::PayloadId, Order::Asc)
            .into_partial_model::<ListedPayloadRow>()
            .all(state.db())
            .await
            .map_err(|e| {
                error!(
                    user_id = %user_id,
                    error = %e,
                    "Failed to load payloads while listing objects",
                );
                ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
            })?
    };

    let mut payloads_by_object = HashMap::<Uuid, Vec<ListedPayloadRow>>::new();
    for payload in payloads {
        payloads_by_object
            .entry(payload.object_id)
            .or_default()
            .push(payload);
    }

    let source_device_ids: Vec<Uuid> = objects
        .iter()
        .map(|object| object.source_device_id)
        .collect();
    let device_public_keys = if source_device_ids.is_empty() {
        HashMap::new()
    } else {
        devices::Entity::find()
            .filter(devices::Column::Id.is_in(source_device_ids))
            .filter(devices::Column::UserId.eq(user_id))
            .select_only()
            .column(devices::Column::Id)
            .column(devices::Column::SigningPublicKey)
            .into_tuple::<(Uuid, Vec<u8>)>()
            .all(state.db())
            .await
            .map_err(|e| {
                error!(
                    user_id = %user_id,
                    error = %e,
                    "Failed to load source device signing keys while listing objects",
                );
                ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
            })?
            .into_iter()
            .collect::<HashMap<_, _>>()
    };

    let mut items = Vec::with_capacity(objects.len());
    for object in objects {
        let created_seq = object.created_seq.ok_or_else(|| {
            error!(
                object_id = %object.id,
                user_id = %user_id,
                "Complete object row is missing created_seq",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;
        let envelope = postcard::from_bytes(&object.envelope).map_err(|e| {
            error!(
                object_id = %object.id,
                error = %e,
                "Stored object envelope could not be decoded",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;
        let source_device_signing_public_key = device_public_keys
            .get(&object.source_device_id)
            .cloned()
            .ok_or_else(|| {
                error!(
                    object_id = %object.id,
                    source_device_id = %object.source_device_id,
                    "Object source device row is missing while listing objects",
                );
                ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
            })?;
        items.push(ObjectListItem {
            id: object.id.into(),
            kind: object.kind.parse().map_err(|_| {
                error!(
                    object_id = %object.id,
                    kind = %object.kind,
                    "Object row has unknown kind value in database",
                );
                ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
            })?,
            created_seq,
            meta_nonce: object.meta_nonce.clone(),
            meta_ciphertext: object.meta_ciphertext.clone(),
            payloads: payloads_by_object
                .remove(&object.id)
                .unwrap_or_default()
                .into_iter()
                .map(|p| ObjectPayloadDescriptor {
                    id: p.payload_id.into(),
                    nonce: p.nonce,
                    ciphertext_size: p.ciphertext_size,
                    sha256_ciphertext: p.sha256_ciphertext,
                })
                .collect(),
            created_at: object.created_at.clone(),
            source_device_id: object.source_device_id.into(),
            source_device_signing_public_key,
            envelope,
        });
    }
    Ok(items)
}

pub async fn download_payload(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path((object_id, payload_id)): Path<(String, String)>,
) -> Result<Body, ApiError> {
    let object_uuid =
        Uuid::parse_str(&object_id).map_err(|_| ApiError::from_code(ApiErrorCode::InvalidId))?;
    let payload_uuid =
        Uuid::parse_str(&payload_id).map_err(|_| ApiError::from_code(ApiErrorCode::InvalidId))?;

    let object = objects::Entity::find_by_id(object_uuid)
        .filter(objects::Column::UserId.eq(auth.user_id))
        .filter(objects::Column::Status.eq("complete"))
        .filter(objects::Column::CreatedSeq.is_not_null())
        .select_only()
        .column(objects::Column::Id)
        .column(objects::Column::Kind)
        .into_tuple::<(Uuid, String)>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                error = %e,
                "Failed to look up object for download",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;
    let Some((_, kind)) = object else {
        debug!(
            object_id = %object_uuid,
            user_id = %auth.user_id,
            "Rejected payload download for missing or incomplete object",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectNotFound,
            "Object not found",
        ));
    };
    ensure_object_read_retained(&state, auth.user_id, object_uuid, &kind).await?;

    let payload = object_payloads::Entity::find_by_id((object_uuid, payload_uuid))
        .filter(object_payloads::Column::Status.eq("complete"))
        .select_only()
        .column(object_payloads::Column::CiphertextPath)
        .into_tuple::<String>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                payload_id = %payload_uuid,
                error = %e,
                "Failed to look up payload for download",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .ok_or_else(|| {
            debug!(
                object_id = %object_uuid,
                payload_id = %payload_uuid,
                "Rejected download for missing or incomplete object payload",
            );
            ApiError::from_code_with_message(
                ApiErrorCode::ObjectPayloadNotFound,
                "Object payload not found",
            )
        })?;

    let path = state.objects_dir().join(&payload);
    let file = tokio::fs::File::open(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            error!(
                object_id = %object_uuid,
                payload_id = %payload_uuid,
                path = %path.display(),
                "Payload file missing for complete payload (data inconsistency)",
            );
        } else {
            error!(
                object_id = %object_uuid,
                payload_id = %payload_uuid,
                path = %path.display(),
                error = %e,
                error_kind = ?e.kind(),
                "Failed to open payload file for download",
            );
        }
        ApiError::from_code_with_message(ApiErrorCode::Storage, "Object payload storage error")
    })?;

    Ok(Body::from_stream(ReaderStream::new(file)))
}

pub async fn delete_object(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path(object_id): Path<String>,
) -> Result<Postcard<ObjectDeleteResponse>, ApiError> {
    let object_uuid =
        Uuid::parse_str(&object_id).map_err(|_| ApiError::from_code(ApiErrorCode::InvalidId))?;
    let kind = objects::Entity::find_by_id(object_uuid)
        .filter(objects::Column::UserId.eq(auth.user_id))
        .select_only()
        .column(objects::Column::Kind)
        .into_tuple::<String>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                error = %e,
                "Failed to look up object for delete",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .ok_or_else(|| {
            debug!(
                object_id = %object_uuid,
                user_id = %auth.user_id,
                "Rejected delete for missing object",
            );
            ApiError::from_code_with_message(ApiErrorCode::ObjectNotFound, "Object not found")
        })?;
    let kind: ObjectKind = kind.parse().map_err(|_| {
        error!(
            object_id = %object_uuid,
            kind = %kind,
            "Object row has unknown kind value in database",
        );
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })?;

    if kind != ObjectKind::File {
        debug!(
            object_id = %object_uuid,
            kind = kind.as_ref(),
            "Rejected delete_object for non-file object",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectDeleteUnsupported,
            "Only file objects can be deleted",
        ));
    }

    let payload_paths: Vec<String> = object_payloads::Entity::find()
        .filter(object_payloads::Column::ObjectId.eq(object_uuid))
        .select_only()
        .column(object_payloads::Column::CiphertextPath)
        .into_tuple()
        .all(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                error = %e,
                "Failed to list payloads for delete",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;
    let paths: Vec<_> = payload_paths
        .iter()
        .map(|payload_path| state.objects_dir().join(payload_path))
        .collect();

    let txn = state.db().begin().await.map_err(|e| {
        error!(error = %e, "Failed to begin delete_object transaction");
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })?;

    objects::Entity::delete_by_id(object_uuid)
        .exec(&txn)
        .await
        .map_err(|e| {
            error!(
                object_id = %object_uuid,
                error = %e,
                "Failed to delete object row",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;

    let now = Utc::now().to_rfc3339();
    let event = event_log::ActiveModel {
        // Allocated after the object delete above has taken the write lock.
        seq: Set(state.next_event_seq()),
        user_id: Set(auth.user_id),
        event_type: Set(ObjectEventType::Deleted.to_string()),
        object_kind: Set("file".into()),
        object_id: Set(object_uuid),
        created_at: Set(now.clone()),
    };
    let inserted = match event.insert(&txn).await {
        Ok(inserted) => inserted,
        Err(e) => {
            error!(
                object_id = %object_uuid,
                error = %e,
                "Failed to insert deleted event",
            );
            _ = txn.rollback().await;
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::Database,
                "Database error",
            ));
        }
    };

    txn.commit().await.map_err(|e| {
        error!(
            object_id = %object_uuid,
            error = %e,
            "Failed to commit delete_object transaction",
        );
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })?;

    remove_paths(paths).await;
    _ = state.ws_tx().send(WsBroadcast {
        user_id: auth.user_id,
        source_device_id: auth.device_id,
        seq: inserted.seq,
        event_type: ObjectEventType::Deleted,
        object_kind: ObjectKind::File,
        object_id: object_uuid.into(),
        created_at: now,
    });

    Ok(Postcard(ObjectDeleteResponse {
        deleted_seq: inserted.seq,
    }))
}

async fn validate_object_init_envelope(
    state: &AppState,
    user_id: Uuid,
    device_id: Uuid,
    req: &ObjectInitRequest,
) -> Result<(), ApiError> {
    let body = &req.envelope.body;
    let object_id = req.id.into_uuid();
    if body.object_id != req.id
        || body.object_type != req.kind
        || body.object_version != 1
        || body.source_device_id.into_uuid() != device_id
        || body.operation != ObjectEnvelopeOperation::Create
        || body.meta_nonce != req.meta_nonce
    {
        debug!(
            object_id = %object_id,
            device_id = %device_id,
            "Rejected object init with envelope fields that do not match request context",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::InvalidObjectEnvelope,
            "Object envelope does not match request",
        ));
    }

    let meta_hash = crypto::sha256(&req.meta_ciphertext);
    if body.sha256_meta_ciphertext.as_slice() != meta_hash.as_slice() {
        debug!(
            object_id = %object_id,
            "Rejected object init with metadata hash mismatch in envelope",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::InvalidObjectEnvelope,
            "Object envelope metadata hash mismatch",
        ));
    }

    let mut payloads_by_id = HashMap::new();
    for payload in &req.payloads {
        if payloads_by_id.insert(payload.id, payload).is_some() {
            debug!(
                object_id = %object_id,
                payload_id = %payload.id,
                "Rejected object init with duplicate payload id",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::DuplicateObjectPayloadId,
                "Duplicate object payload id",
            ));
        }
    }
    if body.payloads.len() != payloads_by_id.len() {
        debug!(
            object_id = %object_id,
            request_payloads = payloads_by_id.len(),
            envelope_payloads = body.payloads.len(),
            "Rejected object init with mismatched envelope payload count",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::InvalidObjectEnvelope,
            "Object envelope payload set mismatch",
        ));
    }

    for envelope_payload in &body.payloads {
        let Some(payload) = payloads_by_id.remove(&envelope_payload.id) else {
            debug!(
                object_id = %object_id,
                payload_id = %envelope_payload.id,
                "Rejected object init with envelope payload missing from request",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::InvalidObjectEnvelope,
                "Object envelope payload set mismatch",
            ));
        };
        validate_envelope_payload(object_id, payload, envelope_payload)?;
    }

    let public_key = devices::Entity::find_by_id(device_id)
        .filter(devices::Column::UserId.eq(user_id))
        .select_only()
        .column(devices::Column::SigningPublicKey)
        .into_tuple::<Vec<u8>>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                device_id = %device_id,
                user_id = %user_id,
                error = %e,
                "Failed to load device signing key for object envelope validation",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .ok_or_else(|| {
            warn!(
                device_id = %device_id,
                user_id = %user_id,
                "Authenticated session references missing device row",
            );
            ApiError::from_code_with_message(ApiErrorCode::Unauthorized, "Unauthorized")
        })?;

    crypto::verify_object_envelope_signature(&public_key, &req.envelope).map_err(|e| {
        warn!(
            object_id = %object_id,
            device_id = %device_id,
            error = %e,
            "Rejected object init with invalid envelope signature",
        );
        ApiError::from_code_with_message(
            ApiErrorCode::InvalidObjectEnvelope,
            "Invalid object envelope signature",
        )
    })
}

fn validate_envelope_payload(
    object_id: Uuid,
    payload: &ObjectPayloadInit,
    envelope_payload: &clipper_core::models::ObjectEnvelopePayloadV1,
) -> Result<(), ApiError> {
    if envelope_payload.nonce != payload.nonce
        || envelope_payload.ciphertext_size != payload.ciphertext_size
        || envelope_payload.sha256_ciphertext != payload.sha256_ciphertext
    {
        debug!(
            object_id = %object_id,
            payload_id = %payload.id,
            "Rejected object init with envelope payload metadata mismatch",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::InvalidObjectEnvelope,
            "Object envelope payload metadata mismatch",
        ));
    }
    Ok(())
}

async fn object_for_upload(
    state: &AppState,
    user_id: Uuid,
    device_id: Uuid,
    object_id: Uuid,
) -> Result<ObjectUploadRow, ApiError> {
    let object = objects::Entity::find_by_id(object_id)
        .filter(objects::Column::UserId.eq(user_id))
        .into_partial_model::<ObjectUploadRow>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_id,
                user_id = %user_id,
                error = %e,
                "Failed to look up object for upload context",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .ok_or_else(|| {
            debug!(
                object_id = %object_id,
                user_id = %user_id,
                "Object upload context lookup found no matching object",
            );
            ApiError::from_code_with_message(ApiErrorCode::ObjectNotFound, "Object not found")
        })?;

    if object.source_device_id != device_id {
        warn!(
            object_id = %object_id,
            user_id = %user_id,
            source_device_id = %object.source_device_id,
            request_device_id = %device_id,
            "Rejected object upload mutation from non-source device",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectForbidden,
            "Forbidden",
        ));
    }

    Ok(object)
}

async fn idempotent_init_response(
    state: &AppState,
    user_id: Uuid,
    device_id: Uuid,
    req: &ObjectInitRequest,
    envelope_bytes: &[u8],
    existing: objects::Model,
) -> Result<ObjectInitResponse, ApiError> {
    let object_id = req.id.into_uuid();
    if existing.user_id != user_id
        || existing.source_device_id != device_id
        || existing.kind != req.kind.to_string()
        || existing.meta_nonce != req.meta_nonce
        || existing.meta_ciphertext != req.meta_ciphertext
        || existing.envelope.as_slice() != envelope_bytes
    {
        warn!(
            object_id = %object_id,
            user_id = %user_id,
            device_id = %device_id,
            "Rejected object init for conflicting existing object id",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectAlreadyExists,
            "Object already exists with different data",
        ));
    }

    let payloads = object_payloads::Entity::find()
        .filter(object_payloads::Column::ObjectId.eq(object_id))
        .select_only()
        .column(object_payloads::Column::PayloadId)
        .column(object_payloads::Column::Nonce)
        .column(object_payloads::Column::CiphertextSize)
        .column(object_payloads::Column::Sha256Ciphertext)
        .column(object_payloads::Column::Status)
        .into_tuple::<(Uuid, Vec<u8>, i64, Vec<u8>, String)>()
        .all(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_id,
                error = %e,
                "Failed to load existing object payloads for idempotent init",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;
    if payloads.len() != req.payloads.len() {
        warn!(
            object_id = %object_id,
            expected = req.payloads.len(),
            actual = payloads.len(),
            "Rejected object init for existing object with different payload count",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectAlreadyExists,
            "Object already exists with different payloads",
        ));
    }

    let mut payloads_by_id = payloads
        .into_iter()
        .map(|(id, nonce, ciphertext_size, sha256_ciphertext, status)| {
            (id, (nonce, ciphertext_size, sha256_ciphertext, status))
        })
        .collect::<HashMap<_, _>>();
    let mut upload_urls = Vec::new();
    for payload in &req.payloads {
        let payload_id = payload.id.into_uuid();
        let Some((nonce, ciphertext_size, sha256_ciphertext, status)) =
            payloads_by_id.remove(&payload_id)
        else {
            warn!(
                object_id = %object_id,
                payload_id = %payload.id,
                "Rejected object init for existing object missing requested payload",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::ObjectAlreadyExists,
                "Object already exists with different payloads",
            ));
        };
        if nonce != payload.nonce
            || ciphertext_size != payload.ciphertext_size
            || sha256_ciphertext != payload.sha256_ciphertext
        {
            warn!(
                object_id = %object_id,
                payload_id = %payload.id,
                "Rejected object init for existing object with different payload metadata",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::ObjectAlreadyExists,
                "Object already exists with different payload metadata",
            ));
        }
        if status == "pending" || status == "uploading" {
            upload_urls.push(ObjectPayloadUpload {
                id: payload.id,
                upload_url: format!("/api/objects/{}/payloads/{}", req.id, payload.id),
            });
        }
    }

    if !payloads_by_id.is_empty() {
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectAlreadyExists,
            "Object already exists with different payloads",
        ));
    }

    if existing.status == "complete" {
        let created_seq = existing.created_seq.ok_or_else(|| {
            error!(
                object_id = %object_id,
                user_id = %user_id,
                "Complete object is missing created_seq during idempotent init",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;
        debug!(
            object_id = %object_id,
            user_id = %user_id,
            device_id = %device_id,
            "Accepted idempotent init for already complete object",
        );
        return Ok(ObjectInitResponse::Complete { created_seq });
    }

    debug!(
        object_id = %object_id,
        user_id = %user_id,
        device_id = %device_id,
        upload_urls = upload_urls.len(),
        "Accepted idempotent init for pending object",
    );
    Ok(ObjectInitResponse::Pending { upload_urls })
}

async fn set_object_created_seq<C>(
    db: &C,
    user_id: Uuid,
    object_id: Uuid,
    created_seq: i64,
) -> Result<(), ApiError>
where
    C: sea_orm::ConnectionTrait,
{
    let updated = objects::Entity::update_many()
        .col_expr(
            objects::Column::CreatedSeq,
            sea_orm::sea_query::Expr::value(created_seq),
        )
        .col_expr(
            objects::Column::Status,
            sea_orm::sea_query::Expr::value("complete"),
        )
        .filter(objects::Column::Id.eq(object_id))
        .filter(objects::Column::UserId.eq(user_id))
        .exec(db)
        .await
        .map_err(|e| {
            error!(
                object_id = %object_id,
                user_id = %user_id,
                error = %e,
                "Failed to set object created_seq",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?;
    if updated.rows_affected != 1 {
        error!(
            object_id = %object_id,
            user_id = %user_id,
            rows_affected = updated.rows_affected,
            "Setting object created_seq affected an unexpected row count",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::Database,
            "Database error",
        ));
    }
    Ok(())
}

async fn insert_created_event<C>(
    db: &C,
    user_id: Uuid,
    kind: ObjectKind,
    object_id: Uuid,
    now: &str,
    seq: i64,
) -> Result<event_log::Model, ApiError>
where
    C: sea_orm::ConnectionTrait,
{
    event_log::ActiveModel {
        seq: Set(seq),
        user_id: Set(user_id),
        event_type: Set(ObjectEventType::Created.to_string()),
        object_kind: Set(kind.to_string()),
        object_id: Set(object_id),
        created_at: Set(now.into()),
    }
    .insert(db)
    .await
    .map_err(|e| {
        error!(
            object_id = %object_id,
            user_id = %user_id,
            kind = kind.as_ref(),
            error = %e,
            "Failed to insert created event",
        );
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })
}

fn parse_object_kind(object_id: Uuid, kind: &str) -> Result<ObjectKind, ApiError> {
    kind.parse().map_err(|_| {
        error!(
            object_id = %object_id,
            kind = %kind,
            "Object row has unknown kind value in database",
        );
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })
}

#[cfg(test)]
async fn object_event_seq(
    state: &AppState,
    user_id: Uuid,
    kind: ObjectKind,
    object_id: Uuid,
    event_type: ObjectEventType,
) -> Result<i64, ApiError> {
    event_log::Entity::find()
        .filter(event_log::Column::UserId.eq(user_id))
        .filter(event_log::Column::ObjectId.eq(object_id))
        .filter(event_log::Column::ObjectKind.eq(kind.to_string()))
        .filter(event_log::Column::EventType.eq(event_type.to_string()))
        .order_by_desc(event_log::Column::Seq)
        .select_only()
        .column(event_log::Column::Seq)
        .into_tuple::<i64>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(
                object_id = %object_id,
                user_id = %user_id,
                event_type = %event_type,
                error = %e,
                "Failed to look up committed object event seq",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .ok_or_else(|| {
            error!(
                object_id = %object_id,
                user_id = %user_id,
                event_type = %event_type,
                "Object is missing its committed event seq",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })
}

fn map_payload_batch_insert_error(error: DbErr, object_id: Uuid) -> ApiError {
    match error.sql_err() {
        Some(SqlErr::UniqueConstraintViolation(constraint)) => {
            if is_duplicate_payload_id_violation(&constraint) {
                warn!(
                    object_id = %object_id,
                    constraint = %constraint,
                    "Duplicate payload id in init_object request",
                );
                ApiError::from_code_with_message(
                    ApiErrorCode::DuplicateObjectPayloadId,
                    "Duplicate object payload id",
                )
            } else if is_payload_path_conflict(&constraint) {
                warn!(
                    object_id = %object_id,
                    constraint = %constraint,
                    "Object payload ids resolve to conflicting storage paths",
                );
                ApiError::from_code_with_message(
                    ApiErrorCode::BadRequest,
                    "Object payload ids conflict",
                )
            } else {
                error!(
                    object_id = %object_id,
                    constraint = %constraint,
                    error = %error,
                    "Failed to batch insert object payload rows due to a uniqueness violation",
                );
                ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
            }
        }
        _ => {
            error!(
                object_id = %object_id,
                error = %error,
                "Failed to batch insert object payload rows",
            );
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        }
    }
}

fn is_duplicate_payload_id_violation(constraint: &str) -> bool {
    (constraint.contains("object_payloads.object_id")
        && constraint.contains("object_payloads.payload_id"))
        || constraint.contains("pk_object_payloads")
}

fn is_payload_path_conflict(constraint: &str) -> bool {
    constraint.contains("object_payloads.ciphertext_path")
}

fn broadcast_created(
    state: &AppState,
    user_id: Uuid,
    source_device_id: Uuid,
    seq: i64,
    kind: ObjectKind,
    object_id: &str,
    now: &str,
) {
    _ = state.ws_tx().send(WsBroadcast {
        user_id,
        source_device_id,
        seq,
        event_type: ObjectEventType::Created,
        object_kind: kind,
        object_id: object_id
            .parse()
            .expect("broadcast object_id was already validated"),
        created_at: now.into(),
    });
}

fn spawn_clipboard_trim(state: AppState, user_id: Uuid) {
    tokio::spawn(async move {
        if let Err(err) = crate::cleanup::trim_user_clipboard(&state, user_id).await {
            tracing::warn!(user_id = %user_id, error = %err, "Clipboard trim failed");
        }
    });
}

fn object_payload_filename(object_id: &str, payload_id: &str) -> String {
    format!("{object_id}.{payload_id}.bin")
}

async fn stream_body_to_payload_file(
    body: Body,
    expected_size: u64,
    tmp_path: &std::path::Path,
) -> Result<(), ApiError> {
    let mut out_file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp_path)
        .await
        .map_err(|e| {
            error!(
                path = %tmp_path.display(),
                error = %e,
                "Failed to create tmp payload file",
            );
            ApiError::from_code_with_message(ApiErrorCode::Storage, "Object payload storage error")
        })?;

    use futures_util::StreamExt;
    let mut stream = body.into_data_stream();
    let mut total_size = 0_u64;
    while let Some(chunk) = stream.next().await {
        let data = chunk.map_err(|e| {
            debug!(error = %e, "Payload upload stream error (client disconnect or network)");
            ApiError::from_code_with_message(ApiErrorCode::Stream, "Stream error")
        })?;
        total_size += data.len() as u64;
        if total_size > expected_size {
            drop(out_file);
            debug!(
                expected_size,
                actual_size = total_size,
                "Rejected payload upload larger than initialized size",
            );
            return Err(ApiError::from_code_with_message(
                ApiErrorCode::ObjectPayloadIntegrityMismatch,
                "Payload size does not match initialized size",
            ));
        }
        out_file.write_all(&data).await.map_err(|e| {
            error!(
                path = %tmp_path.display(),
                error = %e,
                "Failed to write payload chunk to disk",
            );
            ApiError::from_code_with_message(
                ApiErrorCode::PayloadWrite,
                "Object payload write error",
            )
        })?;
    }

    out_file.flush().await.map_err(|e| {
        error!(
            path = %tmp_path.display(),
            error = %e,
            "Failed to flush tmp payload file",
        );
        ApiError::from_code_with_message(ApiErrorCode::PayloadWrite, "Object payload flush error")
    })?;
    drop(out_file);

    if total_size != expected_size {
        debug!(
            expected_size,
            actual_size = total_size,
            "Rejected payload upload smaller than initialized size",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectPayloadIntegrityMismatch,
            "Payload size does not match initialized size",
        ));
    }

    Ok(())
}

async fn reset_payload_status(
    state: &AppState,
    object_id: Uuid,
    payload_id: Uuid,
    from: &str,
    to: &str,
) {
    let now = Utc::now().to_rfc3339();
    if let Err(e) = object_payloads::Entity::update_many()
        .col_expr(
            object_payloads::Column::Status,
            sea_orm::sea_query::Expr::value(to),
        )
        .col_expr(
            object_payloads::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(object_payloads::Column::ObjectId.eq(object_id))
        .filter(object_payloads::Column::PayloadId.eq(payload_id))
        .filter(object_payloads::Column::Status.eq(from))
        .exec(state.db())
        .await
    {
        warn!(
            object_id = %object_id,
            payload_id = %payload_id,
            from = from,
            to = to,
            error = %e,
            "Best-effort payload status reset failed",
        );
    }
}

async fn sha256_file(path: &std::path::Path) -> std::io::Result<([u8; SHA256_BYTES], u64)> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 16 * 1024];
    let mut size = 0_u64;

    loop {
        let read = file.read(&mut buf).await?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buf[..read]);
    }

    Ok((hasher.finalize().into(), size))
}

async fn remove_paths(paths: Vec<std::path::PathBuf>) {
    for path in paths {
        if let Err(error) = tokio::fs::remove_file(&path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(
                path = %path.display(),
                error = %error,
                "Best-effort payload file removal failed",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, http::StatusCode};
    use clipper_core::{
        crypto::{self, XCHACHA20_NONCE_BYTES, sha256},
        models::{
            ObjectEnvelopeBodyV1, ObjectEnvelopeOperation, ObjectEnvelopePayloadV1,
            ObjectEnvelopeV1, ObjectPayloadComplete, ObjectPayloadInit,
        },
    };
    use sea_orm::Database;
    use tempfile::TempDir;

    use super::*;
    use crate::entity::{access_keys, devices, users};

    async fn test_state() -> (AppState, TempDir) {
        test_state_with_max_items(100).await
    }

    async fn test_state_with_max_items(max_items: u64) -> (AppState, TempDir) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        let mut config = crate::config::ServerConfig::default();
        config.server.data_dir = data_dir.path().to_path_buf();
        config.clipboard.max_items = max_items;
        let state = AppState::open_with_db_and_config(
            db,
            config,
            crate::secret::ServerSecrets::test_fixture(),
        )
        .await
        .expect("state");
        (state, data_dir)
    }

    fn auth(user_id: Uuid, device_id: Uuid) -> AuthInfo {
        AuthInfo {
            session_id: Uuid::now_v7(),
            user_id,
            device_id,
        }
    }

    fn postcard<T>(value: T) -> Postcard<T>
    where
        T: garde::Validate,
        T::Context: Default,
    {
        Postcard::validated(value).expect("valid request")
    }

    fn init_created_seq(response: ObjectInitResponse) -> i64 {
        match response {
            ObjectInitResponse::Complete { created_seq } => created_seq,
            ObjectInitResponse::Pending { .. } => panic!("expected complete object init response"),
        }
    }

    fn init_upload_urls(response: ObjectInitResponse) -> Vec<ObjectPayloadUpload> {
        match response {
            ObjectInitResponse::Pending { upload_urls } => upload_urls,
            ObjectInitResponse::Complete { .. } => panic!("expected pending object init response"),
        }
    }

    async fn insert_user(state: &AppState) -> Uuid {
        let now = Utc::now().to_rfc3339();
        let user_id = Uuid::now_v7();
        let access_key_hash = Uuid::now_v7().to_string();
        access_keys::ActiveModel {
            key_hash: Set(access_key_hash.clone()),
            created_at: Set(now.clone()),
            expires_at: Set(None),
            used_at: Set(Some(now.clone())),
            used_by_user_id: Set(Some(user_id)),
        }
        .insert(state.db())
        .await
        .expect("insert access key");
        users::ActiveModel {
            id: Set(user_id),
            username: Set(user_id.as_simple().to_string()),
            opaque_password_file: Set(vec![2]),
            encryption_salt: Set(vec![3]),
            access_key_hash: Set(access_key_hash),
            created_at: Set(now.clone()),
            updated_at: Set(now),
        }
        .insert(state.db())
        .await
        .expect("insert user");
        user_id
    }

    async fn insert_device(
        state: &AppState,
        user_id: Uuid,
        id: Uuid,
    ) -> [u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES] {
        let now = Utc::now().to_rfc3339();
        let signing_secret_key = crypto::generate_device_signing_secret_key();
        let signing_public_key = crypto::device_signing_public_key(&signing_secret_key);
        devices::ActiveModel {
            id: Set(id),
            user_id: Set(user_id),
            name: Set("test-device".into()),
            platform: Set("test".into()),
            signing_public_key: Set(signing_public_key.to_vec()),
            created_at: Set(now.clone()),
            updated_at: Set(now.clone()),
            last_seen_at: Set(now),
        }
        .insert(state.db())
        .await
        .expect("insert device");
        signing_secret_key
    }

    fn init_request(
        object_id: String,
        payload_id: String,
        kind: ObjectKind,
        ciphertext: &[u8],
        inline: bool,
        device_id: Uuid,
        signing_secret_key: &[u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES],
    ) -> ObjectInitRequest {
        let meta_nonce = vec![1_u8; XCHACHA20_NONCE_BYTES];
        let meta_ciphertext = b"encrypted metadata".to_vec();
        let payload_nonce = vec![2_u8; XCHACHA20_NONCE_BYTES];
        let payload_hash = sha256(ciphertext).to_vec();
        let envelope = signed_envelope(
            object_id.parse().expect("object id"),
            kind,
            meta_nonce.clone(),
            &meta_ciphertext,
            vec![ObjectEnvelopePayloadV1 {
                id: payload_id.parse().expect("payload id"),
                nonce: payload_nonce.clone(),
                ciphertext_size: ciphertext.len() as i64,
                sha256_ciphertext: payload_hash.clone(),
            }],
            device_id,
            signing_secret_key,
        );
        ObjectInitRequest {
            id: object_id.parse().expect("object id"),
            kind,
            meta_nonce,
            meta_ciphertext,
            payloads: vec![ObjectPayloadInit {
                id: payload_id.parse().expect("payload id"),
                nonce: payload_nonce,
                ciphertext_size: ciphertext.len() as i64,
                sha256_ciphertext: payload_hash,
                inline_ciphertext: inline.then(|| ciphertext.to_vec()),
            }],
            envelope,
        }
    }

    fn signed_envelope(
        object_id: clipper_core::models::ObjectId,
        kind: ObjectKind,
        meta_nonce: Vec<u8>,
        meta_ciphertext: &[u8],
        payloads: Vec<ObjectEnvelopePayloadV1>,
        device_id: Uuid,
        signing_secret_key: &[u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES],
    ) -> ObjectEnvelopeV1 {
        let body = ObjectEnvelopeBodyV1 {
            object_id,
            object_type: kind,
            object_version: 1,
            source_device_id: device_id.into(),
            created_at: Utc::now().to_rfc3339(),
            operation: ObjectEnvelopeOperation::Create,
            meta_nonce,
            sha256_meta_ciphertext: sha256(meta_ciphertext).to_vec(),
            payloads,
        };
        ObjectEnvelopeV1 {
            signature: crypto::sign_object_envelope_body(signing_secret_key, &body)
                .expect("sign envelope"),
            body,
        }
    }

    #[tokio::test]
    async fn init_rejects_wrong_payload_nonce_length_before_writing() {
        let (_state, data_dir) = test_state().await;
        let device_id = Uuid::now_v7();
        let signing_secret_key = crypto::generate_device_signing_secret_key();
        let object_id = Uuid::now_v7().to_string();
        let payload_id = Uuid::now_v7().to_string();
        let mut req = init_request(
            object_id.clone(),
            payload_id.clone(),
            ObjectKind::Clipboard,
            b"payload",
            true,
            device_id,
            &signing_secret_key,
        );
        req.payloads[0].nonce = vec![2_u8; 12];

        let result = Postcard::validated(req);

        assert_eq!(result.unwrap_err().status(), StatusCode::BAD_REQUEST);
        assert!(
            !data_dir
                .path()
                .join("objects")
                .join(object_payload_filename(&object_id, &payload_id))
                .exists()
        );
    }

    #[tokio::test]
    async fn init_rejects_wrong_payload_sha256_length_before_writing() {
        let (_state, data_dir) = test_state().await;
        let device_id = Uuid::now_v7();
        let signing_secret_key = crypto::generate_device_signing_secret_key();
        let object_id = Uuid::now_v7().to_string();
        let payload_id = Uuid::now_v7().to_string();
        let mut req = init_request(
            object_id.clone(),
            payload_id.clone(),
            ObjectKind::Clipboard,
            b"payload",
            true,
            device_id,
            &signing_secret_key,
        );
        req.payloads[0].sha256_ciphertext = vec![3_u8; SHA256_BYTES - 1];

        let result = Postcard::validated(req);

        assert_eq!(result.unwrap_err().status(), StatusCode::BAD_REQUEST);
        assert!(
            !data_dir
                .path()
                .join("objects")
                .join(object_payload_filename(&object_id, &payload_id))
                .exists()
        );
    }

    #[tokio::test]
    async fn inline_init_completes_lists_and_downloads_object() {
        let (state, _data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        let signing_secret_key = insert_device(&state, user_id, device_id).await;
        let object_id = Uuid::now_v7().to_string();
        let payload_id = Uuid::now_v7().to_string();
        let ciphertext = b"encrypted clipboard payload";
        let mut rx = state.ws_tx().subscribe();

        let Postcard(resp) = init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            postcard(init_request(
                object_id.clone(),
                payload_id.clone(),
                ObjectKind::Clipboard,
                ciphertext,
                true,
                device_id,
                &signing_secret_key,
            )),
        )
        .await
        .expect("init");

        let created_seq = init_created_seq(resp);
        assert!(created_seq > 0);
        let broadcast = rx.try_recv().expect("created broadcast");
        assert_eq!(broadcast.user_id, user_id);
        assert_eq!(broadcast.source_device_id, device_id);
        assert_eq!(broadcast.seq, created_seq);
        assert_eq!(broadcast.event_type, ObjectEventType::Created);
        assert_eq!(broadcast.object_kind, ObjectKind::Clipboard);
        assert_eq!(broadcast.object_id.to_string(), object_id);
        assert_eq!(
            object_event_seq(
                &state,
                user_id,
                ObjectKind::Clipboard,
                object_id.parse().expect("object id"),
                ObjectEventType::Created,
            )
            .await
            .expect("event seq"),
            created_seq,
        );

        let Postcard(list) = list_objects(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            Query(ObjectListQuery {
                kind: Some("clipboard".into()),
                limit: None,
                created_seq_lte: None,
                after: None,
            }),
        )
        .await
        .expect("list");
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].id.to_string(), object_id);
        assert_eq!(list.items[0].created_seq, created_seq);

        let Postcard(target) = get_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            Path(object_id.clone()),
        )
        .await
        .expect("targeted get");
        assert_eq!(target.id.to_string(), object_id);
        assert_eq!(target.created_seq, created_seq);

        let body = download_payload(
            State(state),
            Extension(auth(user_id, device_id)),
            Path((object_id, payload_id)),
        )
        .await
        .expect("download");
        let bytes = to_bytes(body, usize::MAX).await.expect("bytes");
        assert_eq!(&bytes[..], ciphertext);
    }

    #[tokio::test]
    async fn inline_init_accepts_multiple_payloads_in_one_batch() {
        let (state, _data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        let signing_secret_key = insert_device(&state, user_id, device_id).await;
        let object_id = Uuid::now_v7().to_string();
        let object_id_typed = object_id.parse().expect("object id");
        let object_uuid = object_id.parse::<Uuid>().expect("object id");
        let first_payload_id = Uuid::now_v7();
        let second_payload_id = Uuid::now_v7();
        let meta_nonce = vec![1_u8; XCHACHA20_NONCE_BYTES];
        let meta_ciphertext = b"encrypted metadata".to_vec();
        let payloads = [
            (first_payload_id, b"first encrypted payload".to_vec()),
            (second_payload_id, b"second encrypted payload".to_vec()),
        ];
        let envelope_payloads = payloads
            .iter()
            .map(|(id, ciphertext)| ObjectEnvelopePayloadV1 {
                id: (*id).into(),
                nonce: vec![2_u8; XCHACHA20_NONCE_BYTES],
                ciphertext_size: ciphertext.len() as i64,
                sha256_ciphertext: sha256(ciphertext).to_vec(),
            })
            .collect::<Vec<_>>();
        let envelope = signed_envelope(
            object_id_typed,
            ObjectKind::Clipboard,
            meta_nonce.clone(),
            &meta_ciphertext,
            envelope_payloads,
            device_id,
            &signing_secret_key,
        );

        let Postcard(resp) = init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            postcard(ObjectInitRequest {
                id: object_id_typed,
                kind: ObjectKind::Clipboard,
                meta_nonce,
                meta_ciphertext,
                payloads: payloads
                    .iter()
                    .map(|(id, ciphertext)| ObjectPayloadInit {
                        id: (*id).into(),
                        nonce: vec![2_u8; XCHACHA20_NONCE_BYTES],
                        ciphertext_size: ciphertext.len() as i64,
                        sha256_ciphertext: sha256(ciphertext).to_vec(),
                        inline_ciphertext: Some(ciphertext.clone()),
                    })
                    .collect(),
                envelope,
            }),
        )
        .await
        .expect("init");

        assert!(init_created_seq(resp) > 0);

        let rows = object_payloads::Entity::find()
            .filter(object_payloads::Column::ObjectId.eq(object_uuid))
            .order_by(object_payloads::Column::PayloadId, Order::Asc)
            .all(state.db())
            .await
            .expect("payload rows");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.status == "complete"));

        let Postcard(list) = list_objects(
            State(state),
            Extension(auth(user_id, device_id)),
            Query(ObjectListQuery {
                kind: Some("clipboard".into()),
                limit: None,
                created_seq_lte: None,
                after: None,
            }),
        )
        .await
        .expect("list");
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].payloads.len(), 2);
    }

    #[tokio::test]
    async fn init_rejects_duplicate_payload_id_before_insert() {
        let (state, _data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        let signing_secret_key = insert_device(&state, user_id, device_id).await;
        let object_id = Uuid::now_v7();
        let payload_id = Uuid::now_v7();
        let ciphertext = b"encrypted file payload";
        let meta_nonce = vec![1_u8; XCHACHA20_NONCE_BYTES];
        let meta_ciphertext = b"encrypted metadata".to_vec();
        let envelope = signed_envelope(
            object_id.into(),
            ObjectKind::File,
            meta_nonce.clone(),
            &meta_ciphertext,
            vec![ObjectEnvelopePayloadV1 {
                id: payload_id.into(),
                nonce: vec![2_u8; XCHACHA20_NONCE_BYTES],
                ciphertext_size: ciphertext.len() as i64,
                sha256_ciphertext: sha256(ciphertext).to_vec(),
            }],
            device_id,
            &signing_secret_key,
        );

        // Bypass request validation to cover route-level duplicate detection.
        let result = init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            Postcard(ObjectInitRequest {
                id: object_id.into(),
                kind: ObjectKind::File,
                meta_nonce,
                meta_ciphertext,
                payloads: vec![
                    ObjectPayloadInit {
                        id: payload_id.into(),
                        nonce: vec![2_u8; XCHACHA20_NONCE_BYTES],
                        ciphertext_size: ciphertext.len() as i64,
                        sha256_ciphertext: sha256(ciphertext).to_vec(),
                        inline_ciphertext: None,
                    },
                    ObjectPayloadInit {
                        id: payload_id.into(),
                        nonce: vec![3_u8; XCHACHA20_NONCE_BYTES],
                        ciphertext_size: ciphertext.len() as i64,
                        sha256_ciphertext: sha256(ciphertext).to_vec(),
                        inline_ciphertext: None,
                    },
                ],
                envelope,
            }),
        )
        .await;

        let err = result.expect_err("duplicate payload id should fail");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.body().code, ApiErrorCode::DuplicateObjectPayloadId);

        let object = objects::Entity::find_by_id(object_id)
            .one(state.db())
            .await
            .expect("object lookup");
        assert!(object.is_none(), "failed init transaction rolls back");
    }

    #[test]
    fn payload_uniqueness_helpers_separate_payload_id_from_path_conflict() {
        let duplicate_payload_id =
            "UNIQUE constraint failed: object_payloads.object_id, object_payloads.payload_id";
        let path_conflict = "UNIQUE constraint failed: object_payloads.ciphertext_path";
        let unknown = "UNIQUE constraint failed: other.column";

        assert!(is_duplicate_payload_id_violation(duplicate_payload_id));
        assert!(!is_payload_path_conflict(duplicate_payload_id));

        assert!(
            !is_duplicate_payload_id_violation(path_conflict),
            "ciphertext_path is not the payload-id primary key"
        );
        assert!(is_payload_path_conflict(path_conflict));

        assert!(!is_duplicate_payload_id_violation(unknown));
        assert!(!is_payload_path_conflict(unknown));
    }

    #[tokio::test]
    async fn streaming_upload_completes_after_exact_size_and_hash_check() {
        let (state, _data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        let signing_secret_key = insert_device(&state, user_id, device_id).await;
        let object_id = Uuid::now_v7().to_string();
        let payload_id = Uuid::now_v7().to_string();
        let ciphertext = b"encrypted file payload";
        let req = init_request(
            object_id.clone(),
            payload_id.clone(),
            ObjectKind::File,
            ciphertext,
            false,
            device_id,
            &signing_secret_key,
        );

        let Postcard(resp) = init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            postcard(req.clone()),
        )
        .await
        .expect("init");

        let upload_urls = init_upload_urls(resp);
        assert_eq!(upload_urls.len(), 1);

        upload_payload(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            Path((object_id.clone(), payload_id.clone())),
            Body::from(ciphertext.to_vec()),
        )
        .await
        .expect("upload");

        let Postcard(idempotent_uploaded_init) = init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            postcard(req.clone()),
        )
        .await
        .expect("idempotent init after upload");
        assert!(init_upload_urls(idempotent_uploaded_init).is_empty());

        let mut rx = state.ws_tx().subscribe();
        let Postcard(complete_resp) = complete_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            Path(object_id.clone()),
            postcard(ObjectCompleteRequest {
                payloads: vec![ObjectPayloadComplete {
                    id: payload_id.parse().expect("payload id"),
                    ciphertext_size: ciphertext.len() as i64,
                    sha256_ciphertext: sha256(ciphertext).to_vec(),
                }],
            }),
        )
        .await
        .expect("complete");
        assert!(complete_resp.created_seq > 0);
        let broadcast = rx.try_recv().expect("created broadcast");
        assert_eq!(broadcast.user_id, user_id);
        assert_eq!(broadcast.source_device_id, device_id);
        assert_eq!(broadcast.seq, complete_resp.created_seq);
        assert_eq!(broadcast.event_type, ObjectEventType::Created);
        assert_eq!(broadcast.object_kind, ObjectKind::File);
        assert_eq!(broadcast.object_id.to_string(), object_id);

        let Postcard(idempotent_resp) = complete_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            Path(object_id.clone()),
            postcard(ObjectCompleteRequest {
                payloads: vec![ObjectPayloadComplete {
                    id: payload_id.parse().expect("payload id"),
                    ciphertext_size: ciphertext.len() as i64,
                    sha256_ciphertext: sha256(ciphertext).to_vec(),
                }],
            }),
        )
        .await
        .expect("idempotent complete");
        assert_eq!(idempotent_resp.created_seq, complete_resp.created_seq);

        let Postcard(idempotent_complete_init) = init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            postcard(req),
        )
        .await
        .expect("idempotent init after complete");
        assert_eq!(
            init_created_seq(idempotent_complete_init),
            complete_resp.created_seq
        );

        let Postcard(list) = list_objects(
            State(state),
            Extension(auth(user_id, device_id)),
            Query(ObjectListQuery {
                kind: Some("file".into()),
                limit: None,
                created_seq_lte: None,
                after: None,
            }),
        )
        .await
        .expect("list");
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].id.to_string(), object_id);
        assert_eq!(list.items[0].created_seq, complete_resp.created_seq);
    }

    #[tokio::test]
    async fn delete_file_returns_deleted_seq_and_broadcast_actor() {
        let (state, data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        let signing_secret_key = insert_device(&state, user_id, device_id).await;
        let object_id = Uuid::now_v7().to_string();
        let object_uuid = object_id.parse::<Uuid>().expect("object id");
        let payload_id = Uuid::now_v7().to_string();
        let ciphertext = b"encrypted file payload";

        init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            postcard(init_request(
                object_id.clone(),
                payload_id.clone(),
                ObjectKind::File,
                ciphertext,
                true,
                device_id,
                &signing_secret_key,
            )),
        )
        .await
        .expect("init");

        let mut rx = state.ws_tx().subscribe();
        let Postcard(delete_resp) = delete_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            Path(object_id.clone()),
        )
        .await
        .expect("delete");

        assert!(delete_resp.deleted_seq > 0);
        let broadcast = rx.try_recv().expect("deleted broadcast");
        assert_eq!(broadcast.user_id, user_id);
        assert_eq!(broadcast.source_device_id, device_id);
        assert_eq!(broadcast.seq, delete_resp.deleted_seq);
        assert_eq!(broadcast.event_type, ObjectEventType::Deleted);
        assert_eq!(broadcast.object_kind, ObjectKind::File);
        assert_eq!(broadcast.object_id.to_string(), object_id);
        assert_eq!(
            object_event_seq(
                &state,
                user_id,
                ObjectKind::File,
                object_uuid,
                ObjectEventType::Deleted,
            )
            .await
            .expect("event seq"),
            delete_resp.deleted_seq,
        );

        let object = objects::Entity::find_by_id(object_uuid)
            .one(state.db())
            .await
            .expect("object lookup");
        assert!(object.is_none());
        assert!(
            !data_dir
                .path()
                .join("objects")
                .join(object_payload_filename(&object_id, &payload_id))
                .exists(),
            "deleted payload file was removed",
        );
    }

    #[tokio::test]
    async fn streaming_upload_rejects_size_mismatch_without_final_file() {
        let (state, data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        let signing_secret_key = insert_device(&state, user_id, device_id).await;
        let object_id = Uuid::now_v7().to_string();
        let payload_id = Uuid::now_v7().to_string();

        init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            postcard(init_request(
                object_id.clone(),
                payload_id.clone(),
                ObjectKind::Clipboard,
                b"ok",
                false,
                device_id,
                &signing_secret_key,
            )),
        )
        .await
        .expect("init");

        let result = upload_payload(
            State(state),
            Extension(auth(user_id, device_id)),
            Path((object_id.clone(), payload_id.clone())),
            Body::from(b"too long".to_vec()),
        )
        .await;

        assert_eq!(result.unwrap_err().status(), StatusCode::BAD_REQUEST);
        assert!(
            !data_dir
                .path()
                .join("objects")
                .join(object_payload_filename(&object_id, &payload_id))
                .exists()
        );
    }

    #[tokio::test]
    async fn init_rejects_payload_exceeding_max_blob_bytes() {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        let mut config = crate::config::ServerConfig::default();
        config.server.data_dir = data_dir.path().to_path_buf();
        config.limits.max_file_blob_bytes = 8;
        let state = AppState::open_with_db_and_config(
            db,
            config,
            crate::secret::ServerSecrets::test_fixture(),
        )
        .await
        .expect("state");
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        let signing_secret_key = insert_device(&state, user_id, device_id).await;

        let object_id = Uuid::now_v7().to_string();
        let payload_id = Uuid::now_v7().to_string();
        // Declared ciphertext_size (9) exceeds the 8-byte ceiling. Use a
        // streaming payload so nothing is written before the size gate.
        let req = init_request(
            object_id.clone(),
            payload_id.clone(),
            ObjectKind::File,
            b"123456789",
            false,
            device_id,
            &signing_secret_key,
        );

        let result = init_object(
            State(state),
            Extension(auth(user_id, device_id)),
            postcard(req),
        )
        .await;

        assert_eq!(result.unwrap_err().status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            !data_dir
                .path()
                .join("objects")
                .join(object_payload_filename(&object_id, &payload_id))
                .exists()
        );
    }

    #[tokio::test]
    async fn clipboard_object_gets_ttl_and_file_object_does_not() {
        let (state, _data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        let signing_secret_key = insert_device(&state, user_id, device_id).await;

        let clip_object_id = Uuid::now_v7().to_string();
        let clip_payload_id = Uuid::now_v7().to_string();
        init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            postcard(init_request(
                clip_object_id.clone(),
                clip_payload_id,
                ObjectKind::Clipboard,
                b"clip",
                true,
                device_id,
                &signing_secret_key,
            )),
        )
        .await
        .expect("init clipboard");

        let file_object_id = Uuid::now_v7().to_string();
        let file_payload_id = Uuid::now_v7().to_string();
        init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            postcard(init_request(
                file_object_id.clone(),
                file_payload_id,
                ObjectKind::File,
                b"file",
                true,
                device_id,
                &signing_secret_key,
            )),
        )
        .await
        .expect("init file");

        let clip = objects::Entity::find_by_id(clip_object_id.parse::<Uuid>().expect("uuid"))
            .one(state.db())
            .await
            .expect("query")
            .expect("clip row");
        let file = objects::Entity::find_by_id(file_object_id.parse::<Uuid>().expect("uuid"))
            .one(state.db())
            .await
            .expect("query")
            .expect("file row");

        let clip_expires = clip.expires_at.expect("clipboard objects carry a TTL");
        let created = chrono::DateTime::parse_from_rfc3339(&clip.created_at).expect("rfc3339");
        let expires = chrono::DateTime::parse_from_rfc3339(&clip_expires).expect("rfc3339");
        let delta = expires.signed_duration_since(created);
        assert_eq!(
            delta.num_days(),
            state.config().clipboard.ttl_days,
            "clipboard expires_at = created_at + ttl_days",
        );
        assert!(file.expires_at.is_none(), "file objects have no TTL");

        objects::Entity::update_many()
            .col_expr(
                objects::Column::ExpiresAt,
                sea_orm::sea_query::Expr::value("2000-01-01T00:00:00+00:00"),
            )
            .filter(objects::Column::Id.eq(clip.id))
            .exec(state.db())
            .await
            .expect("expire clipboard");
        let result = get_object(
            State(state),
            Extension(auth(user_id, device_id)),
            Path(clip_object_id),
        )
        .await;
        assert_eq!(result.unwrap_err().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn trim_user_clipboard_keeps_newest_and_drops_files() {
        let (state, data_dir) = test_state_with_max_items(2).await;
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        let signing_secret_key = insert_device(&state, user_id, device_id).await;

        let mut ids = Vec::new();
        for i in 0_u8..4 {
            let object_id = Uuid::now_v7().to_string();
            let payload_id = Uuid::now_v7().to_string();
            init_object(
                State(state.clone()),
                Extension(auth(user_id, device_id)),
                postcard(init_request(
                    object_id.clone(),
                    payload_id.clone(),
                    ObjectKind::Clipboard,
                    &[i; 8],
                    true,
                    device_id,
                    &signing_secret_key,
                )),
            )
            .await
            .expect("init");
            ids.push((object_id, payload_id));
            // Distinct created_at — RFC3339 second resolution would collide otherwise.
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        }

        crate::cleanup::trim_user_clipboard(&state, user_id)
            .await
            .expect("trim");

        let remaining = objects::Entity::find()
            .filter(objects::Column::UserId.eq(user_id))
            .filter(objects::Column::Kind.eq("clipboard"))
            .order_by(objects::Column::CreatedAt, Order::Desc)
            .all(state.db())
            .await
            .expect("query");
        assert_eq!(remaining.len(), 2, "max_items=2 should retain 2 rows");
        let kept: std::collections::HashSet<_> = remaining.iter().map(|o| o.id).collect();
        let last_two: std::collections::HashSet<_> = ids[2..]
            .iter()
            .map(|(o, _)| o.parse::<Uuid>().expect("uuid"))
            .collect();
        assert_eq!(kept, last_two, "newest two items survive trim");

        for (object_id, payload_id) in &ids[..2] {
            assert!(
                !data_dir
                    .path()
                    .join("objects")
                    .join(object_payload_filename(object_id, payload_id))
                    .exists(),
                "trimmed payload file was deleted from disk",
            );
        }
        for (object_id, payload_id) in &ids[2..] {
            assert!(
                data_dir
                    .path()
                    .join("objects")
                    .join(object_payload_filename(object_id, payload_id))
                    .exists(),
                "retained payload file still on disk",
            );
        }
    }
}
