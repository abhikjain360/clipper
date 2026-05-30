//! Generic encrypted object routes.

use std::collections::HashMap;

use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
};
use chrono::{Duration, Utc};
use clipper_core::{
    crypto::SHA256_BYTES,
    models::{
        ApiErrorCode, ObjectCompleteRequest, ObjectInitRequest, ObjectInitResponse, ObjectKind,
        ObjectListItem, ObjectListResponse, ObjectPayloadDescriptor, ObjectPayloadUpload,
        OkResponse,
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DerivePartialModel, EntityTrait, Order, QueryFilter, QueryOrder,
    QuerySelect, Set, SqlErr, TransactionTrait,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    auth::AuthInfo,
    entity::{event_log, object_payloads, objects},
    routes::{ApiError, Postcard},
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

    if objects::Entity::find_by_id(object_id)
        .select_only()
        .column(objects::Column::Id)
        .into_tuple::<Uuid>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(object_id = %object_id, error = %e, "Failed to look up object in init_object");
            ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
        })?
        .is_some()
    {
        debug!(
            object_id = %object_id,
            user_id = %auth.user_id,
            "Rejected object init for existing object id",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectAlreadyExists,
            "Object already exists",
        ));
    }

    let mut all_inline = true;
    let mut written_paths = Vec::new();
    for payload in &req.payloads {
        let Some(inline_ciphertext) = &payload.inline_ciphertext else {
            all_inline = false;
            continue;
        };

        let payload_id = payload.id.to_string();
        let path = state
            .objects_dir()
            .join(object_payload_filename(&object_id_text, &payload_id));
        write_payload_bytes_create_new(&path, inline_ciphertext)
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
        written_paths.push(path);
    }
    let all_inline = all_inline;

    let now = Utc::now().to_rfc3339();
    let expires_at = match req.kind {
        ObjectKind::Clipboard => {
            let created = chrono::DateTime::parse_from_rfc3339(&now).ok();
            created.map(|created| {
                (created + Duration::days(state.config().clipboard.ttl_days)).to_rfc3339()
            })
        }
        ObjectKind::File => None,
    };
    let txn = state.db().begin().await.map_err(|e| {
        error!(error = %e, "Failed to begin init_object transaction");
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })?;

    let object = objects::ActiveModel {
        id: Set(object_id),
        user_id: Set(auth.user_id),
        kind: Set(req.kind.to_string()),
        meta_ciphertext: Set(req.meta_ciphertext),
        meta_nonce: Set(req.meta_nonce),
        created_at: Set(now.clone()),
        updated_at: Set(now.clone()),
        expires_at: Set(expires_at),
        source_device_id: Set(auth.device_id),
        status: Set(if all_inline { "complete" } else { "pending" }.into()),
    };

    if let Err(e) = object.insert(&txn).await {
        _ = txn.rollback().await;
        remove_paths(written_paths).await;
        return Err(match e.sql_err() {
            Some(SqlErr::UniqueConstraintViolation(constraint)) => {
                warn!(
                    object_id = %object_id,
                    user_id = %auth.user_id,
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
        });
    }

    for payload in &req.payloads {
        let payload_id = payload.id.to_string();
        let payload_model = object_payloads::ActiveModel {
            object_id: Set(object_id),
            payload_id: Set(payload.id.into_uuid()),
            ciphertext_path: Set(object_payload_filename(&object_id_text, &payload_id)),
            nonce: Set(payload.nonce.clone()),
            ciphertext_size: Set(payload.ciphertext_size),
            sha256_ciphertext: Set(payload.sha256_ciphertext.clone()),
            created_at: Set(now.clone()),
            updated_at: Set(now.clone()),
            status: Set(if payload.inline_ciphertext.is_some() {
                "complete"
            } else {
                "pending"
            }
            .into()),
        };

        if let Err(e) = payload_model.insert(&txn).await {
            let _ = txn.rollback().await;
            remove_paths(written_paths).await;
            return Err(match e.sql_err() {
                Some(SqlErr::UniqueConstraintViolation(_)) => {
                    warn!(
                        object_id = %object_id,
                        payload_id = %payload_id,
                        "Duplicate payload id in init_object request",
                    );
                    ApiError::from_code_with_message(
                        ApiErrorCode::DuplicateObjectPayloadId,
                        "Duplicate object payload id",
                    )
                }
                _ => {
                    error!(
                        object_id = %object_id,
                        payload_id = %payload_id,
                        error = %e,
                        "Failed to insert object payload row",
                    );
                    ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
                }
            });
        }
    }

    let inserted_event = if all_inline {
        // Allocated here, after the object/payload inserts above have taken the
        // write lock, so seq order matches commit order.
        Some(
            insert_created_event(
                &txn,
                auth.user_id,
                req.kind,
                object_id,
                &now,
                state.next_event_seq(),
            )
            .await?,
        )
    } else {
        None
    };

    if let Err(e) = txn.commit().await {
        error!(object_id = %object_id, error = %e, "Failed to commit init_object transaction");
        remove_paths(written_paths).await;
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::Database,
            "Database error",
        ));
    }

    if let Some(inserted) = inserted_event {
        broadcast_created(
            &state,
            auth.user_id,
            inserted.seq,
            req.kind,
            &object_id_text,
            &now,
        );
        if req.kind == ObjectKind::Clipboard {
            spawn_clipboard_trim(state.clone(), auth.user_id);
        }
    }

    let upload_urls = req
        .payloads
        .iter()
        .filter(|p| p.inline_ciphertext.is_none())
        .map(|p| ObjectPayloadUpload {
            id: p.id,
            upload_url: format!("/api/objects/{}/payloads/{}", object_id_text, p.id),
        })
        .collect();

    info!(device_id = %auth.device_id, object_id = %object_id_text, kind = req.kind.as_ref(), "Object initialized");

    Ok(Postcard(ObjectInitResponse {
        upload_urls,
        complete: all_inline,
    }))
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
        let _ = tokio::fs::remove_file(&tmp_path).await;
        reset_payload_status(&state, object_uuid, payload_uuid, "uploading", "pending").await;
        return Err(response);
    }

    let _ = tokio::fs::remove_file(&final_path).await;
    if let Err(e) = tokio::fs::rename(&tmp_path, &final_path).await {
        error!(
            object_id = %object_uuid,
            payload_id = %payload_uuid,
            tmp = %tmp_path.display(),
            dest = %final_path.display(),
            error = %e,
            "Failed to rename tmp payload to final path",
        );
        let _ = tokio::fs::remove_file(&tmp_path).await;
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
        let _ = tokio::fs::remove_file(&final_path).await;
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
    Ok(Postcard(OkResponse { ok: true }))
}

pub async fn complete_object(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path(object_id): Path<String>,
    Postcard(req): Postcard<ObjectCompleteRequest>,
) -> Result<Postcard<OkResponse>, ApiError> {
    let object_uuid =
        Uuid::parse_str(&object_id).map_err(|_| ApiError::from_code(ApiErrorCode::InvalidId))?;
    let object = object_for_upload(&state, auth.user_id, auth.device_id, object_uuid).await?;

    if object.status == "complete" {
        debug!(
            object_id = %object_uuid,
            user_id = %auth.user_id,
            device_id = %auth.device_id,
            "Accepted idempotent complete_object for already complete object",
        );
        return Ok(Postcard(OkResponse { ok: true }));
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

    let kind = object.kind.parse().map_err(|_| {
        error!(
            object_id = %object_uuid,
            kind = %object.kind,
            "Object row has unknown kind value in database",
        );
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })?;
    let now = Utc::now().to_rfc3339();
    let txn = state.db().begin().await.map_err(|e| {
        error!(error = %e, "Failed to begin complete_object transaction");
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })?;

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
        let _ = txn.rollback().await;
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

    let inserted = insert_created_event(
        &txn,
        auth.user_id,
        kind,
        object_uuid,
        &now,
        state.next_event_seq(),
    )
    .await?;

    txn.commit().await.map_err(|e| {
        error!(
            object_id = %object_uuid,
            error = %e,
            "Failed to commit complete_object transaction",
        );
        ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
    })?;

    broadcast_created(&state, auth.user_id, inserted.seq, kind, &object_id, &now);
    if kind == ObjectKind::Clipboard {
        spawn_clipboard_trim(state.clone(), auth.user_id);
    }

    info!(device_id = %auth.device_id, object_id = %object_id, kind = kind.as_ref(), "Object completed");
    Ok(Postcard(OkResponse { ok: true }))
}

#[derive(Debug, serde::Deserialize)]
pub struct ObjectListQuery {
    pub kind: Option<String>,
    pub limit: Option<u64>,
    pub before: Option<String>,
}

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "objects::Entity", from_query_result)]
struct ListedObjectRow {
    id: Uuid,
    kind: String,
    meta_ciphertext: Vec<u8>,
    meta_nonce: Vec<u8>,
    created_at: String,
    source_device_id: Uuid,
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
    let mut q = objects::Entity::find()
        .filter(objects::Column::UserId.eq(auth.user_id))
        .filter(objects::Column::Status.eq("complete"))
        .order_by(objects::Column::CreatedAt, Order::Desc);

    if let Some(kind) = &query.kind {
        let kind: ObjectKind = kind.parse().map_err(|_| {
            debug!(kind = %kind, "Rejected unknown object kind in list query");
            ApiError::from_code_with_message(ApiErrorCode::InvalidObjectKind, "Invalid object kind")
        })?;
        q = q.filter(objects::Column::Kind.eq(kind.as_ref()));
    }

    if let Some(before) = &query.before {
        q = q.filter(objects::Column::CreatedAt.lt(before.clone()));
    }

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
                    user_id = %auth.user_id,
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

    let mut items = Vec::with_capacity(objects.len());
    for object in &objects {
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
        });
    }

    let next_before = if has_more {
        objects.last().map(|i| i.created_at.clone())
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

    Ok(Postcard(ObjectListResponse { items, next_before }))
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

    let object_exists = objects::Entity::find_by_id(object_uuid)
        .filter(objects::Column::UserId.eq(auth.user_id))
        .filter(objects::Column::Status.eq("complete"))
        .select_only()
        .column(objects::Column::Id)
        .into_tuple::<Uuid>()
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
    if object_exists.is_none() {
        debug!(
            object_id = %object_uuid,
            user_id = %auth.user_id,
            "Rejected payload download for missing or incomplete object",
        );
        return Err(ApiError::from_code_with_message(
            ApiErrorCode::ObjectNotFound,
            "Object not found",
        ));
    }

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
) -> Result<Postcard<OkResponse>, ApiError> {
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
        event_type: Set("file.deleted".into()),
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
                "Failed to insert file.deleted event",
            );
            let _ = txn.rollback().await;
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
    let _ = state.ws_tx().send(WsBroadcast {
        user_id: auth.user_id,
        seq: inserted.seq,
        event_type: "file.deleted".into(),
        object_kind: "file".into(),
        object_id,
        created_at: now,
    });

    Ok(Postcard(OkResponse { ok: true }))
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
        event_type: Set(format!("{}.created", kind.as_ref())),
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

fn broadcast_created(
    state: &AppState,
    user_id: Uuid,
    seq: i64,
    kind: ObjectKind,
    object_id: &str,
    now: &str,
) {
    let _ = state.ws_tx().send(WsBroadcast {
        user_id,
        seq,
        event_type: format!("{}.created", kind.as_ref()),
        object_kind: kind.to_string(),
        object_id: object_id.into(),
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

async fn write_payload_bytes_create_new(
    path: &std::path::Path,
    data: &[u8],
) -> std::io::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await?;
    file.write_all(data).await?;
    file.flush().await
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
        crypto::{XCHACHA20_NONCE_BYTES, sha256},
        models::{ObjectPayloadComplete, ObjectPayloadInit},
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

    async fn insert_device(state: &AppState, user_id: Uuid, id: Uuid) {
        let now = Utc::now().to_rfc3339();
        devices::ActiveModel {
            id: Set(id),
            user_id: Set(user_id),
            name: Set("test-device".into()),
            platform: Set("test".into()),
            created_at: Set(now.clone()),
            updated_at: Set(now.clone()),
            last_seen_at: Set(now),
        }
        .insert(state.db())
        .await
        .expect("insert device");
    }

    fn init_request(
        object_id: String,
        payload_id: String,
        kind: ObjectKind,
        ciphertext: &[u8],
        inline: bool,
    ) -> ObjectInitRequest {
        ObjectInitRequest {
            id: object_id.parse().expect("object id"),
            kind,
            meta_nonce: vec![1_u8; XCHACHA20_NONCE_BYTES],
            meta_ciphertext: b"encrypted metadata".to_vec(),
            payloads: vec![ObjectPayloadInit {
                id: payload_id.parse().expect("payload id"),
                nonce: vec![2_u8; XCHACHA20_NONCE_BYTES],
                ciphertext_size: ciphertext.len() as i64,
                sha256_ciphertext: sha256(ciphertext).to_vec(),
                inline_ciphertext: inline.then(|| ciphertext.to_vec()),
            }],
        }
    }

    #[tokio::test]
    async fn init_rejects_wrong_payload_nonce_length_before_writing() {
        let (_state, data_dir) = test_state().await;
        let object_id = Uuid::now_v7().to_string();
        let payload_id = Uuid::now_v7().to_string();
        let mut req = init_request(
            object_id.clone(),
            payload_id.clone(),
            ObjectKind::Clipboard,
            b"payload",
            true,
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
        let object_id = Uuid::now_v7().to_string();
        let payload_id = Uuid::now_v7().to_string();
        let mut req = init_request(
            object_id.clone(),
            payload_id.clone(),
            ObjectKind::Clipboard,
            b"payload",
            true,
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
        insert_device(&state, user_id, device_id).await;
        let object_id = Uuid::now_v7().to_string();
        let payload_id = Uuid::now_v7().to_string();
        let ciphertext = b"encrypted clipboard payload";

        let Postcard(resp) = init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            postcard(init_request(
                object_id.clone(),
                payload_id.clone(),
                ObjectKind::Clipboard,
                ciphertext,
                true,
            )),
        )
        .await
        .expect("init");

        assert!(resp.complete);
        assert!(resp.upload_urls.is_empty());

        let Postcard(list) = list_objects(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            Query(ObjectListQuery {
                kind: Some("clipboard".into()),
                limit: None,
                before: None,
            }),
        )
        .await
        .expect("list");
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].id.to_string(), object_id);

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
    async fn streaming_upload_completes_after_exact_size_and_hash_check() {
        let (state, _data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        insert_device(&state, user_id, device_id).await;
        let object_id = Uuid::now_v7().to_string();
        let payload_id = Uuid::now_v7().to_string();
        let ciphertext = b"encrypted file payload";

        let Postcard(resp) = init_object(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            postcard(init_request(
                object_id.clone(),
                payload_id.clone(),
                ObjectKind::File,
                ciphertext,
                false,
            )),
        )
        .await
        .expect("init");

        assert!(!resp.complete);
        assert_eq!(resp.upload_urls.len(), 1);

        upload_payload(
            State(state.clone()),
            Extension(auth(user_id, device_id)),
            Path((object_id.clone(), payload_id.clone())),
            Body::from(ciphertext.to_vec()),
        )
        .await
        .expect("upload");

        complete_object(
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

        let Postcard(list) = list_objects(
            State(state),
            Extension(auth(user_id, device_id)),
            Query(ObjectListQuery {
                kind: Some("file".into()),
                limit: None,
                before: None,
            }),
        )
        .await
        .expect("list");
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].id.to_string(), object_id);
    }

    #[tokio::test]
    async fn streaming_upload_rejects_size_mismatch_without_final_file() {
        let (state, data_dir) = test_state().await;
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        insert_device(&state, user_id, device_id).await;
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
        insert_device(&state, user_id, device_id).await;

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
        insert_device(&state, user_id, device_id).await;

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
    }

    #[tokio::test]
    async fn trim_user_clipboard_keeps_newest_and_drops_files() {
        let (state, data_dir) = test_state_with_max_items(2).await;
        let user_id = insert_user(&state).await;
        let device_id = Uuid::now_v7();
        insert_device(&state, user_id, device_id).await;

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
