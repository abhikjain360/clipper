use axum::{
    Json,
    extract::{Extension, Query, State},
    http::StatusCode,
};
use base64::Engine;
use chrono::{Duration, Utc};
use clipper_core::models::{
    ClipboardItem, ClipboardListResponse, ClipboardUploadRequest, ErrorResponse, OkResponse,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use tokio::io::AsyncWriteExt;
use tracing::info;

use crate::{
    auth::AuthInfo,
    entity::{clipboard_items, event_log},
    routes::{ValidatedJson, error_response},
    state::AppState,
    ws::WsBroadcast,
};

pub async fn upload(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    ValidatedJson(req): ValidatedJson<ClipboardUploadRequest>,
) -> Result<Json<OkResponse>, (StatusCode, Json<ErrorResponse>)> {
    let item_id = req.id.into_uuid();
    let ciphertext = req.ciphertext;
    let nonce = req.nonce;
    let ciphertext_hash = req.ciphertext_sha256;

    if clipboard_items::Entity::find_by_id(item_id)
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .is_some()
    {
        return Err(error_response(
            StatusCode::CONFLICT,
            "Clipboard item already exists",
        ));
    }

    // Write ciphertext to disk
    let filename = format!("{item_id}.bin");
    let clip_dir = state.clipboard_dir();
    let path = clip_dir.join(&filename);
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                error_response(StatusCode::CONFLICT, "Clipboard item already exists")
            } else {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Storage error")
            }
        })?;
    file.write_all(&ciphertext)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Storage error"))?;
    file.flush()
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Storage error"))?;
    drop(file);

    let now = Utc::now().to_rfc3339();
    let expires = (Utc::now() + Duration::days(state.config().clipboard.ttl_days)).to_rfc3339();

    let txn = state
        .db()
        .begin()
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    let item = clipboard_items::ActiveModel {
        id: Set(item_id),
        user_id: Set(auth.user_id),
        ciphertext_path: Set(filename),
        nonce: Set(nonce),
        ciphertext_size: Set(ciphertext.len() as i64),
        sha256_ciphertext: Set(ciphertext_hash),
        created_at: Set(now.clone()),
        expires_at: Set(expires),
        source_device_id: Set(auth.device_id),
    };
    if item.insert(&txn).await.is_err() {
        let _ = txn.rollback().await;
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database error",
        ));
    }

    // Log event
    let event = event_log::ActiveModel {
        seq: Default::default(),
        user_id: Set(auth.user_id),
        event_type: Set("clipboard.created".into()),
        object_kind: Set("clipboard".into()),
        object_id: Set(item_id),
        created_at: Set(now.clone()),
    };
    let inserted = match event.insert(&txn).await {
        Ok(inserted) => inserted,
        Err(_) => {
            let _ = txn.rollback().await;
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database error",
            ));
        }
    };

    if txn.commit().await.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database error",
        ));
    }

    // Broadcast to WebSocket clients
    let _ = state.ws_tx().send(WsBroadcast {
        user_id: auth.user_id,
        seq: i64::from(inserted.seq),
        event_type: "clipboard.created".into(),
        object_kind: "clipboard".into(),
        object_id: item_id.to_string(),
        created_at: now,
    });

    info!(device_id = %auth.device_id, "Clipboard item uploaded");

    Ok(Json(OkResponse { ok: true }))
}

#[derive(Debug, serde::Deserialize)]
pub struct ListQuery {
    pub limit: Option<u64>,
    pub before: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ClipboardListResponse>, StatusCode> {
    let b64 = &base64::engine::general_purpose::STANDARD;
    let limit = query
        .limit
        .unwrap_or(state.config().list.default_limit)
        .min(state.config().list.max_limit);

    let mut q = clipboard_items::Entity::find()
        .filter(clipboard_items::Column::UserId.eq(auth.user_id))
        .order_by(clipboard_items::Column::CreatedAt, Order::Desc);

    if let Some(before) = &query.before {
        q = q.filter(clipboard_items::Column::CreatedAt.lt(before.clone()));
    }

    let items: Vec<clipboard_items::Model> = q
        .all(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Take limit + 1 to determine if there are more
    let has_more = items.len() as u64 > limit;
    let items: Vec<clipboard_items::Model> = items.into_iter().take(limit as usize).collect();

    let mut result_items = Vec::new();
    for item in &items {
        // Read ciphertext from disk
        let path = state.clipboard_dir().join(&item.ciphertext_path);
        let ciphertext = tokio::fs::read(&path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        result_items.push(ClipboardItem {
            id: item.id.to_string(),
            nonce_b64: b64.encode(&item.nonce),
            ciphertext_b64: b64.encode(&ciphertext),
            created_at: item.created_at.clone(),
            source_device_id: item.source_device_id.to_string(),
        });
    }

    let next_before = if has_more {
        items.last().map(|i| i.created_at.clone())
    } else {
        None
    };

    Ok(Json(ClipboardListResponse {
        items: result_items,
        next_before,
    }))
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, extract::FromRequest, http::Request};
    use base64::Engine;
    use clipper_core::crypto::{SHA256_BYTES, XCHACHA20_NONCE_BYTES, sha256};
    use sea_orm::{ActiveModelTrait, Database, EntityTrait, Set};
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::entity::{access_keys, devices, users};

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    type TestResult = Result<(), (StatusCode, Json<ErrorResponse>)>;

    async fn test_state() -> Result<(AppState, TempDir), (StatusCode, Json<ErrorResponse>)> {
        let data_dir = tempfile::tempdir()
            .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Storage error"))?;
        let db = Database::connect("sqlite::memory:")
            .await
            .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;
        let state = AppState::open_with_db(db, data_dir.path().to_path_buf())
            .await
            .map_err(|error| match error {
                crate::error::ServerError::Io(_) => {
                    error_response(StatusCode::INTERNAL_SERVER_ERROR, "Storage error")
                }
                _ => error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
            })?;
        Ok((state, data_dir))
    }

    fn auth(user_id: Uuid, device_id: Uuid) -> AuthInfo {
        AuthInfo {
            session_id: Uuid::new_v4(),
            user_id,
            device_id,
        }
    }

    fn validated<T>(value: T) -> ValidatedJson<T>
    where
        T: garde::Validate,
        T::Context: Default,
    {
        ValidatedJson::validated(value).expect("valid request")
    }

    async fn insert_user(state: &AppState) -> Uuid {
        let now = Utc::now().to_rfc3339();
        let user_id = Uuid::new_v4();
        let access_key_hash = Uuid::new_v4().to_string();
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
            opaque_server_setup: Set(vec![1]),
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

    fn upload_request(id: Uuid, source_device_id: &str) -> ClipboardUploadRequest {
        upload_request_with_ciphertext(id, source_device_id, b"encrypted clipboard")
    }

    fn upload_request_with_ciphertext(
        id: Uuid,
        source_device_id: &str,
        ciphertext: &[u8],
    ) -> ClipboardUploadRequest {
        ClipboardUploadRequest {
            id: id.into(),
            nonce: vec![1_u8; XCHACHA20_NONCE_BYTES],
            ciphertext_sha256: sha256(ciphertext).to_vec(),
            ciphertext: ciphertext.to_vec(),
            source_device_id: source_device_id.into(),
            client_created_at: None,
        }
    }

    // This sends a path-like clipboard ID before any blob write happens. We
    // test it because object IDs become filenames, so accepting non-UUID IDs
    // would reopen path traversal bugs in the upload path.
    #[tokio::test]
    async fn upload_rejects_non_uuid_id_before_writing() -> TestResult {
        let (_state, data_dir) = test_state().await?;
        let body = serde_json::json!({
            "id": "../escape",
            "nonce_b64": B64.encode([1_u8; XCHACHA20_NONCE_BYTES]),
            "ciphertext_b64": B64.encode(b"encrypted clipboard"),
            "ciphertext_sha256_b64": B64.encode(sha256(b"encrypted clipboard")),
            "source_device_id": "device-a"
        });
        let req = Request::builder()
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");

        let result = ValidatedJson::<ClipboardUploadRequest>::from_request(req, &()).await;

        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert!(!data_dir.path().join("escape.bin").exists());
        Ok(())
    }

    #[tokio::test]
    async fn upload_rejects_wrong_nonce_length_before_writing() -> TestResult {
        let (_state, data_dir) = test_state().await?;
        let id = Uuid::new_v4();
        let mut req = upload_request(id, "device-a");
        req.nonce = vec![1_u8; 12];

        let result = ValidatedJson::validated(req);

        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert!(
            !data_dir
                .path()
                .join("clipboard")
                .join(format!("{id}.bin"))
                .exists()
        );
        Ok(())
    }

    #[tokio::test]
    async fn upload_rejects_wrong_sha256_length_before_writing() -> TestResult {
        let (_state, data_dir) = test_state().await?;
        let id = Uuid::new_v4();
        let mut req = upload_request(id, "device-a");
        req.ciphertext_sha256 = vec![1_u8; SHA256_BYTES - 1];

        let result = ValidatedJson::validated(req);

        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert!(
            !data_dir
                .path()
                .join("clipboard")
                .join(format!("{id}.bin"))
                .exists()
        );
        Ok(())
    }

    // This sends a spoofed source_device_id in the body while authenticated as a
    // different device. We test it because provenance must come from the bearer
    // token, not from client-controlled JSON.
    #[tokio::test]
    async fn upload_uses_authenticated_device_as_source() -> TestResult {
        let (state, _data_dir) = test_state().await?;
        let user_id = insert_user(&state).await;
        let device_auth = Uuid::new_v4();
        insert_device(&state, user_id, device_auth).await;
        let id = Uuid::new_v4();

        let _ = upload(
            State(state.clone()),
            Extension(auth(user_id, device_auth)),
            validated(upload_request(id, "device-spoof")),
        )
        .await
        .expect("upload");

        let item = clipboard_items::Entity::find_by_id(id)
            .one(state.db())
            .await
            .expect("query")
            .expect("item");
        assert_eq!(item.source_device_id, device_auth);
        Ok(())
    }

    // This uploads two clipboard blobs with the same client ID. We test it
    // because rejecting the duplicate in the database is not enough if the
    // second write already replaced the ciphertext on disk.
    #[tokio::test]
    async fn upload_rejects_duplicate_id_without_overwriting_blob() -> TestResult {
        let (state, data_dir) = test_state().await?;
        let user_id = insert_user(&state).await;
        let device_a = Uuid::new_v4();
        insert_device(&state, user_id, device_a).await;
        let id = Uuid::new_v4();

        let _ = upload(
            State(state.clone()),
            Extension(auth(user_id, device_a)),
            validated(upload_request_with_ciphertext(
                id,
                "device-a",
                b"first encrypted clipboard",
            )),
        )
        .await
        .expect("first upload");

        let result = upload(
            State(state),
            Extension(auth(user_id, device_a)),
            validated(upload_request_with_ciphertext(
                id,
                "device-a",
                b"second encrypted clipboard",
            )),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
        let blob = tokio::fs::read(data_dir.path().join("clipboard").join(format!("{id}.bin")))
            .await
            .expect("blob");
        assert_eq!(blob, b"first encrypted clipboard");
        Ok(())
    }

    #[tokio::test]
    async fn list_only_returns_items_for_authenticated_user() -> TestResult {
        let (state, _data_dir) = test_state().await?;
        let user_a = insert_user(&state).await;
        let user_b = insert_user(&state).await;
        let device_a = Uuid::new_v4();
        let device_b = Uuid::new_v4();
        insert_device(&state, user_a, device_a).await;
        insert_device(&state, user_b, device_b).await;
        let item_a = Uuid::new_v4();
        let item_b = Uuid::new_v4();

        let _ = upload(
            State(state.clone()),
            Extension(auth(user_a, device_a)),
            validated(upload_request_with_ciphertext(
                item_a,
                "device-a",
                b"user a encrypted clipboard",
            )),
        )
        .await
        .expect("upload a");
        let _ = upload(
            State(state.clone()),
            Extension(auth(user_b, device_b)),
            validated(upload_request_with_ciphertext(
                item_b,
                "device-b",
                b"user b encrypted clipboard",
            )),
        )
        .await
        .expect("upload b");

        let Json(resp) = list(
            State(state),
            Extension(auth(user_a, device_a)),
            Query(ListQuery {
                limit: None,
                before: None,
            }),
        )
        .await
        .expect("list");

        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].id, item_a.to_string());
        Ok(())
    }
}
