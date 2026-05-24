//! Generic encrypted object routes.

use std::collections::{HashMap, HashSet};

use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, Set,
    QuerySelect, TransactionTrait,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;
use tracing::info;
use uuid::Uuid;

use crate::auth::AuthInfo;
use crate::entity::{event_log, object_payloads, objects};
use crate::routes::{Postcard, error_response, validate_client_id, validate_exact_byte_len};
use crate::state::AppState;
use crate::ws::WsBroadcast;
use clipper_core::crypto::{SHA256_BYTES, XCHACHA20_NONCE_BYTES};
use clipper_core::models::{
    ErrorResponse, ObjectCompleteRequest, ObjectInitRequest, ObjectInitResponse, ObjectKind,
    ObjectListItem, ObjectListResponse, ObjectPayloadDescriptor, ObjectPayloadUpload, OkResponse,
};

const MAX_OBJECT_META_CIPHERTEXT_BYTES: usize = 64 * 1024;

pub async fn init_object(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Postcard(req): Postcard<ObjectInitRequest>,
) -> Result<Postcard<ObjectInitResponse>, (StatusCode, axum::Json<ErrorResponse>)> {
    let object_id = validate_client_id(&req.id)?;
    validate_exact_byte_len(&req.meta_nonce, XCHACHA20_NONCE_BYTES, "meta_nonce")?;
    crate::routes::validate_max_byte_len(
        &req.meta_ciphertext,
        MAX_OBJECT_META_CIPHERTEXT_BYTES,
        "Object metadata ciphertext exceeds maximum size",
    )?;
    if req.payloads.is_empty() {
        return Err(error_response(StatusCode::BAD_REQUEST, "Missing object payloads"));
    }

    if objects::Entity::find_by_id(object_id)
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .is_some()
    {
        return Err(error_response(StatusCode::CONFLICT, "Object already exists"));
    }

    let mut seen_payload_ids = HashSet::new();
    for payload in &req.payloads {
        validate_client_id(&payload.id)?;
        if !seen_payload_ids.insert(payload.id.clone()) {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Duplicate payload id",
            ));
        }
        validate_exact_byte_len(&payload.nonce, XCHACHA20_NONCE_BYTES, "payload nonce")?;
        validate_exact_byte_len(
            &payload.sha256_ciphertext,
            SHA256_BYTES,
            "payload sha256_ciphertext",
        )?;
        let expected_size = validate_object_payload_size(payload.ciphertext_size)?;
        if let Some(inline) = &payload.inline_ciphertext {
            if inline.len() as u64 != expected_size {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "Inline payload size does not match declared size",
                ));
            }
            if Sha256::digest(inline).as_slice() != payload.sha256_ciphertext.as_slice() {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "Inline payload SHA-256 mismatch",
                ));
            }
        }
    }

    let all_inline = req.payloads.iter().all(|p| p.inline_ciphertext.is_some());
    let mut written_paths = Vec::new();
    for payload in req.payloads.iter().filter(|p| p.inline_ciphertext.is_some()) {
        let path = object_payload_path(&state, &req.id, &payload.id);
        let inline = payload.inline_ciphertext.as_ref().expect("inline exists");
        write_payload_bytes_create_new(&path, inline).await.map_err(|_| {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Object payload storage error")
        })?;
        written_paths.push(path);
    }

    let now = Utc::now().to_rfc3339();
    let txn = state
        .db()
        .begin()
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    let object = objects::ActiveModel {
        id: Set(object_id),
        user_id: Set(auth.user_id),
        kind: Set(req.kind.as_str().to_string()),
        meta_ciphertext: Set(req.meta_ciphertext),
        meta_nonce: Set(req.meta_nonce),
        created_at: Set(now.clone()),
        updated_at: Set(now.clone()),
        source_device_id: Set(auth.device_id),
        status: Set(if all_inline { "complete" } else { "pending" }.into()),
    };

    if object.insert(&txn).await.is_err() {
        let _ = txn.rollback().await;
        remove_paths(written_paths).await;
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database error",
        ));
    }

    for payload in &req.payloads {
        let payload_model = object_payloads::ActiveModel {
            object_id: Set(object_id),
            payload_id: Set(payload.id.clone()),
            ciphertext_path: Set(object_payload_filename(&req.id, &payload.id)),
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

        if payload_model.insert(&txn).await.is_err() {
            let _ = txn.rollback().await;
            remove_paths(written_paths).await;
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database error",
            ));
        }
    }

    let inserted_event = if all_inline {
        Some(insert_created_event(&txn, auth.user_id, req.kind, object_id, &now).await?)
    } else {
        None
    };

    if txn.commit().await.is_err() {
        remove_paths(written_paths).await;
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database error",
        ));
    }

    if let Some(inserted) = inserted_event {
        broadcast_created(&state, auth.user_id, i64::from(inserted.seq), req.kind, &req.id, &now);
    }

    let upload_urls = req
        .payloads
        .iter()
        .filter(|p| p.inline_ciphertext.is_none())
        .map(|p| ObjectPayloadUpload {
            id: p.id.clone(),
            upload_url: format!("/api/objects/{}/payloads/{}", req.id, p.id),
        })
        .collect();

    info!(device_id = %auth.device_id, object_id = %req.id, kind = req.kind.as_str(), "Object initialized");

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
) -> Result<Postcard<OkResponse>, (StatusCode, axum::Json<ErrorResponse>)> {
    let object_uuid = validate_client_id(&object_id)?;
    validate_client_id(&payload_id)?;
    let object = object_for_upload(&state, auth.user_id, auth.device_id, object_uuid).await?;

    let payload = object_payloads::Entity::find_by_id((object_uuid, payload_id.clone()))
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Object payload not found"))?;

    if payload.status != "pending" {
        return Err(error_response(
            StatusCode::CONFLICT,
            "Object payload already uploaded",
        ));
    }

    let expected_size = validate_object_payload_size(payload.ciphertext_size)?;
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
        .filter(object_payloads::Column::PayloadId.eq(payload_id.clone()))
        .filter(object_payloads::Column::Status.eq("pending"))
        .exec(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    if claimed.rows_affected != 1 {
        return Err(error_response(
            StatusCode::CONFLICT,
            "Object payload upload in progress",
        ));
    }

    let final_path = state.objects_dir().join(&payload.ciphertext_path);
    let tmp_path = state.objects_dir().join(format!(
        "{}.{}.tmp",
        payload.ciphertext_path,
        uuid::Uuid::new_v4()
    ));

    if let Err(response) = stream_body_to_payload_file(body, expected_size, &tmp_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        reset_payload_status(&state, object_uuid, &payload_id, "uploading", "pending").await;
        return Err(response);
    }

    let _ = tokio::fs::remove_file(&final_path).await;
    if tokio::fs::rename(&tmp_path, &final_path).await.is_err() {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        reset_payload_status(&state, object_uuid, &payload_id, "uploading", "pending").await;
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
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
        .filter(object_payloads::Column::PayloadId.eq(payload_id.clone()))
        .filter(object_payloads::Column::Status.eq("uploading"))
        .exec(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    if uploaded.rows_affected != 1 {
        let _ = tokio::fs::remove_file(&final_path).await;
        return Err(error_response(
            StatusCode::CONFLICT,
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
) -> Result<Postcard<OkResponse>, (StatusCode, axum::Json<ErrorResponse>)> {
    let object_uuid = validate_client_id(&object_id)?;
    let object = object_for_upload(&state, auth.user_id, auth.device_id, object_uuid).await?;

    if object.status == "complete" {
        return Ok(Postcard(OkResponse { ok: true }));
    }

    let payloads = object_payloads::Entity::find()
        .filter(object_payloads::Column::ObjectId.eq(object_uuid))
        .all(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    if payloads.is_empty() {
        return Err(error_response(StatusCode::BAD_REQUEST, "Missing object payloads"));
    }

    let mut completion_by_id = HashMap::new();
    for payload in &req.payloads {
        validate_client_id(&payload.id)?;
        validate_exact_byte_len(
            &payload.sha256_ciphertext,
            SHA256_BYTES,
            "payload sha256_ciphertext",
        )?;
        validate_object_payload_size(payload.ciphertext_size)?;
        if completion_by_id
            .insert(payload.id.clone(), payload)
            .is_some()
        {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Duplicate payload id",
            ));
        }
    }

    if completion_by_id.len() != payloads.len() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Complete request does not cover all object payloads",
        ));
    }

    for payload in &payloads {
        let complete = completion_by_id
            .get(&payload.payload_id)
            .ok_or_else(|| error_response(StatusCode::BAD_REQUEST, "Missing payload completion"))?;
        if payload.status != "uploaded" && payload.status != "complete" {
            return Err(error_response(
                StatusCode::CONFLICT,
                "Object payload has not been uploaded",
            ));
        }
        if complete.ciphertext_size != payload.ciphertext_size
            || complete.sha256_ciphertext.as_slice() != payload.sha256_ciphertext.as_slice()
        {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Payload metadata does not match initialized values",
            ));
        }

        let path = state.objects_dir().join(&payload.ciphertext_path);
        let (computed_hash, actual_size) = sha256_file(&path)
            .await
            .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Payload not found"))?;
        if actual_size != payload.ciphertext_size as u64
            || computed_hash.as_slice() != payload.sha256_ciphertext.as_slice()
        {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Payload size or SHA-256 mismatch",
            ));
        }
    }

    let kind = object_kind_from_db(&object.kind)?;
    let now = Utc::now().to_rfc3339();
    let txn = state
        .db()
        .begin()
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

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
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

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
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    if updated.rows_affected != 1 {
        let _ = txn.rollback().await;
        return Err(error_response(
            StatusCode::CONFLICT,
            "Object is no longer ready to complete",
        ));
    }

    let inserted = insert_created_event(&txn, auth.user_id, kind, object_uuid, &now).await?;

    txn.commit()
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    broadcast_created(
        &state,
        auth.user_id,
        i64::from(inserted.seq),
        kind,
        &object_id,
        &now,
    );

    info!(device_id = %auth.device_id, object_id = %object_id, kind = kind.as_str(), "Object completed");
    Ok(Postcard(OkResponse { ok: true }))
}

#[derive(Debug, serde::Deserialize)]
pub struct ObjectListQuery {
    pub kind: Option<String>,
    pub limit: Option<u64>,
    pub before: Option<String>,
}

pub async fn list_objects(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Query(query): Query<ObjectListQuery>,
) -> Result<Postcard<ObjectListResponse>, StatusCode> {
    let limit = query.limit.unwrap_or(100).min(500);
    let mut q = objects::Entity::find()
        .filter(objects::Column::UserId.eq(auth.user_id))
        .filter(objects::Column::Status.eq("complete"))
        .order_by(objects::Column::CreatedAt, Order::Desc);

    if let Some(kind) = &query.kind {
        let kind = object_kind_from_query(kind).map_err(|_| StatusCode::BAD_REQUEST)?;
        q = q.filter(objects::Column::Kind.eq(kind.as_str()));
    }

    if let Some(before) = &query.before {
        q = q.filter(objects::Column::CreatedAt.lt(before.clone()));
    }

    let objects = q
        .limit(limit + 1)
        .all(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let has_more = objects.len() as u64 > limit;
    let objects: Vec<objects::Model> = objects.into_iter().take(limit as usize).collect();

    let mut items = Vec::with_capacity(objects.len());
    for object in &objects {
        let payloads = object_payloads::Entity::find()
            .filter(object_payloads::Column::ObjectId.eq(object.id))
            .filter(object_payloads::Column::Status.eq("complete"))
            .order_by(object_payloads::Column::PayloadId, Order::Asc)
            .all(state.db())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        items.push(ObjectListItem {
            id: object.id.to_string(),
            kind: object_kind_from_db(&object.kind).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            meta_nonce: object.meta_nonce.clone(),
            meta_ciphertext: object.meta_ciphertext.clone(),
            payloads: payloads
                .into_iter()
                .map(|p| ObjectPayloadDescriptor {
                    id: p.payload_id,
                    nonce: p.nonce,
                    ciphertext_size: p.ciphertext_size,
                    sha256_ciphertext: p.sha256_ciphertext,
                })
                .collect(),
            created_at: object.created_at.clone(),
            source_device_id: object.source_device_id.to_string(),
        });
    }

    let next_before = if has_more {
        objects.last().map(|i| i.created_at.clone())
    } else {
        None
    };

    Ok(Postcard(ObjectListResponse { items, next_before }))
}

pub async fn download_payload(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path((object_id, payload_id)): Path<(String, String)>,
) -> Result<Body, StatusCode> {
    let object_uuid = validate_client_id(&object_id).map_err(|(status, _)| status)?;
    validate_client_id(&payload_id).map_err(|(status, _)| status)?;

    objects::Entity::find_by_id(object_uuid)
        .filter(objects::Column::UserId.eq(auth.user_id))
        .filter(objects::Column::Status.eq("complete"))
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let payload = object_payloads::Entity::find_by_id((object_uuid, payload_id))
        .filter(object_payloads::Column::Status.eq("complete"))
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let path = state.objects_dir().join(&payload.ciphertext_path);
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    Ok(Body::from_stream(ReaderStream::new(file)))
}

pub async fn delete_object(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path(object_id): Path<String>,
) -> Result<Postcard<OkResponse>, StatusCode> {
    let object_uuid = validate_client_id(&object_id).map_err(|(status, _)| status)?;
    let object = objects::Entity::find_by_id(object_uuid)
        .filter(objects::Column::UserId.eq(auth.user_id))
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let kind = object_kind_from_db(&object.kind).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if kind != ObjectKind::File {
        return Err(StatusCode::BAD_REQUEST);
    }

    let payloads = object_payloads::Entity::find()
        .filter(object_payloads::Column::ObjectId.eq(object_uuid))
        .all(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let paths: Vec<_> = payloads
        .iter()
        .map(|payload| state.objects_dir().join(&payload.ciphertext_path))
        .collect();

    let txn = state
        .db()
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    objects::Entity::delete_by_id(object_uuid)
        .exec(&txn)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let now = Utc::now().to_rfc3339();
    let event = event_log::ActiveModel {
        seq: Default::default(),
        user_id: Set(auth.user_id),
        event_type: Set("file.deleted".into()),
        object_kind: Set("file".into()),
        object_id: Set(object_uuid),
        created_at: Set(now.clone()),
    };
    let inserted = match event.insert(&txn).await {
        Ok(inserted) => inserted,
        Err(_) => {
            let _ = txn.rollback().await;
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    txn.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    remove_paths(paths).await;
    let _ = state.ws_tx().send(WsBroadcast {
        user_id: auth.user_id,
        seq: i64::from(inserted.seq),
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
) -> Result<objects::Model, (StatusCode, axum::Json<ErrorResponse>)> {
    let object = objects::Entity::find_by_id(object_id)
        .filter(objects::Column::UserId.eq(user_id))
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Object not found"))?;

    if object.source_device_id != device_id {
        return Err(error_response(StatusCode::FORBIDDEN, "Forbidden"));
    }

    Ok(object)
}

async fn insert_created_event<C>(
    db: &C,
    user_id: Uuid,
    kind: ObjectKind,
    object_id: Uuid,
    now: &str,
) -> Result<event_log::Model, (StatusCode, axum::Json<ErrorResponse>)>
where
    C: sea_orm::ConnectionTrait,
{
    event_log::ActiveModel {
        seq: Default::default(),
        user_id: Set(user_id),
        event_type: Set(format!("{}.created", kind.as_str())),
        object_kind: Set(kind.as_str().into()),
        object_id: Set(object_id),
        created_at: Set(now.into()),
    }
    .insert(db)
    .await
    .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))
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
        event_type: format!("{}.created", kind.as_str()),
        object_kind: kind.as_str().into(),
        object_id: object_id.into(),
        created_at: now.into(),
    });
}

fn object_kind_from_query(kind: &str) -> Result<ObjectKind, ()> {
    match kind {
        "clipboard" => Ok(ObjectKind::Clipboard),
        "file" => Ok(ObjectKind::File),
        _ => Err(()),
    }
}

fn object_kind_from_db(kind: &str) -> Result<ObjectKind, (StatusCode, axum::Json<ErrorResponse>)> {
    match kind {
        "clipboard" => Ok(ObjectKind::Clipboard),
        "file" => Ok(ObjectKind::File),
        _ => Err(error_response(StatusCode::BAD_REQUEST, "Invalid object kind")),
    }
}

fn validate_object_payload_size(
    size: i64,
) -> Result<u64, (StatusCode, axum::Json<ErrorResponse>)> {
    if size < 0 {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Invalid payload size",
        ));
    }

    Ok(size as u64)
}

fn object_payload_filename(object_id: &str, payload_id: &str) -> String {
    format!("{object_id}.{payload_id}.bin")
}

fn object_payload_path(state: &AppState, object_id: &str, payload_id: &str) -> std::path::PathBuf {
    state
        .objects_dir()
        .join(object_payload_filename(object_id, payload_id))
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
) -> Result<(), (StatusCode, axum::Json<ErrorResponse>)> {
    let mut out_file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp_path)
        .await
        .map_err(|_| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Object payload storage error",
            )
        })?;

    use futures_util::StreamExt;
    let mut stream = body.into_data_stream();
    let mut total_size = 0_u64;
    while let Some(chunk) = stream.next().await {
        let data = chunk.map_err(|_| error_response(StatusCode::BAD_REQUEST, "Stream error"))?;
        total_size += data.len() as u64;
        if total_size > expected_size {
            drop(out_file);
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Payload size does not match initialized size",
            ));
        }
        out_file.write_all(&data).await.map_err(|_| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Object payload write error",
            )
        })?;
    }

    out_file.flush().await.map_err(|_| {
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Object payload flush error",
        )
    })?;
    drop(out_file);

    if total_size != expected_size {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Payload size does not match initialized size",
        ));
    }

    Ok(())
}

async fn reset_payload_status(
    state: &AppState,
    object_id: Uuid,
    payload_id: &str,
    from: &str,
    to: &str,
) {
    let now = Utc::now().to_rfc3339();
    let _ = object_payloads::Entity::update_many()
        .col_expr(
            object_payloads::Column::Status,
            sea_orm::sea_query::Expr::value(to),
        )
        .col_expr(
            object_payloads::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(object_payloads::Column::ObjectId.eq(object_id))
        .filter(object_payloads::Column::PayloadId.eq(payload_id.to_string()))
        .filter(object_payloads::Column::Status.eq(from))
        .exec(state.db())
        .await;
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
        let _ = tokio::fs::remove_file(path).await;
    }
}
