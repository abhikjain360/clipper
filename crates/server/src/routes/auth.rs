use std::net::SocketAddr;

use axum::{
    Json,
    extract::{ConnectInfo, Extension, State},
    http::{HeaderMap, StatusCode},
};
use base64::Engine;
use chrono::{Duration, Utc};
use clipper_core::{
    crypto,
    models::{
        ErrorResponse, LoginChallengeRequest, LoginChallengeResponse, LoginRequest, LoginResponse,
        OkResponse, RegisterFinishRequest, RegisterFinishResponse, RegisterStartRequest,
        RegisterStartResponse, ServerInfo,
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect, Set,
    TransactionTrait,
};
use tracing::info;
use uuid::Uuid;

use crate::{
    auth::AuthInfo,
    entity::{access_keys, devices, sessions, users},
    rate_limit::RateLimiter,
    routes::{error_response, validate_client_id},
    state::AppState,
};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub async fn challenge(
    State(state): State<AppState>,
    Extension(limiter): Extension<std::sync::Arc<RateLimiter>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    Json(req): Json<LoginChallengeRequest>,
) -> Result<Json<LoginChallengeResponse>, (StatusCode, Json<ErrorResponse>)> {
    let ip = peer_addr.ip().to_string();
    if !limiter.check(&ip) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse {
                error: "Too many requests".into(),
            }),
        ));
    }

    let user = resolve_login_user(&state, req.user_id.as_deref()).await?;

    let credential_request = B64
        .decode(&req.credential_request_b64)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid credential_request_b64"))?;
    let credential_identifier = opaque_credential_identifier_for_user(&user);
    let (credential_response, server_login_state) =
        crypto::opaque_server_login_start_with_identifier(
            &user.opaque_server_setup,
            &user.opaque_password_file,
            &credential_request,
            &credential_identifier,
        )
        .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid login request"))?;
    let challenge_id = state.create_auth_challenge(user.id, server_login_state);

    Ok(Json(LoginChallengeResponse {
        challenge_id,
        credential_response_b64: B64.encode(credential_response),
        server: server_info(&user),
    }))
}

pub async fn register_start(
    State(state): State<AppState>,
    Extension(limiter): Extension<std::sync::Arc<RateLimiter>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    Json(req): Json<RegisterStartRequest>,
) -> Result<Json<RegisterStartResponse>, (StatusCode, Json<ErrorResponse>)> {
    let ip = peer_addr.ip().to_string();
    if !limiter.check(&ip) {
        return Err(error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests",
        ));
    }

    if req.access_key.is_empty() {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Invalid access key",
        ));
    }

    let now = Utc::now().to_rfc3339();
    let access_key_hash = access_key_hash(&req.access_key);
    let access_key = access_keys::Entity::find_by_id(access_key_hash.clone())
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Invalid access key"))?;

    if access_key.used_at.is_some()
        || access_key
            .expires_at
            .as_deref()
            .is_some_and(|expires_at| expires_at <= now.as_str())
    {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Invalid access key",
        ));
    }

    let registration_request = B64
        .decode(&req.registration_request_b64)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid registration_request_b64"))?;
    let user_id = Uuid::new_v4();
    let opaque_server_setup = crypto::opaque_new_server_setup();
    let encryption_salt = crypto::generate_encryption_salt().to_vec();
    let credential_identifier = opaque_credential_identifier(user_id);
    let registration_response = crypto::opaque_server_register_start(
        &opaque_server_setup,
        &registration_request,
        &credential_identifier,
    )
    .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid registration request"))?;

    let registration_id = state.create_pending_registration(
        user_id,
        access_key_hash,
        opaque_server_setup,
        encryption_salt.clone(),
    );

    Ok(Json(RegisterStartResponse {
        registration_id,
        user_id: user_id.to_string(),
        registration_response_b64: B64.encode(registration_response),
        server: ServerInfo {
            encryption_salt_b64: B64.encode(encryption_salt),
            encryption_params: crypto::Argon2Params::default(),
        },
    }))
}

pub async fn register_finish(
    State(state): State<AppState>,
    Extension(limiter): Extension<std::sync::Arc<RateLimiter>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RegisterFinishRequest>,
) -> Result<Json<RegisterFinishResponse>, (StatusCode, Json<ErrorResponse>)> {
    let ip = peer_addr.ip().to_string();
    if !limiter.check(&ip) {
        return Err(error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests",
        ));
    }

    let pending = state
        .take_pending_registration(&req.registration_id)
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Invalid registration"))?;
    let registration_upload = B64
        .decode(&req.registration_upload_b64)
        .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid registration upload"))?;
    let opaque_password_file = crypto::opaque_server_register_finish(&registration_upload)
        .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid registration upload"))?;

    let now = Utc::now().to_rfc3339();
    let txn = state
        .db()
        .begin()
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    let access_key = access_keys::Entity::find_by_id(pending.access_key_hash.clone())
        .one(&txn)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Invalid access key"))?;
    if access_key.used_at.is_some()
        || access_key
            .expires_at
            .as_deref()
            .is_some_and(|expires_at| expires_at <= now.as_str())
    {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Invalid access key",
        ));
    }

    users::ActiveModel {
        id: Set(pending.user_id),
        opaque_server_setup: Set(pending.opaque_server_setup),
        opaque_password_file: Set(opaque_password_file),
        encryption_salt: Set(pending.encryption_salt.clone()),
        access_key_hash: Set(pending.access_key_hash.clone()),
        created_at: Set(now.clone()),
        updated_at: Set(now.clone()),
    }
    .insert(&txn)
    .await
    .map_err(|_| error_response(StatusCode::CONFLICT, "Access key already used"))?;

    let consumed = access_keys::Entity::update_many()
        .col_expr(
            access_keys::Column::UsedAt,
            sea_orm::sea_query::Expr::value(now.clone()),
        )
        .col_expr(
            access_keys::Column::UsedByUserId,
            sea_orm::sea_query::Expr::value(pending.user_id),
        )
        .filter(access_keys::Column::KeyHash.eq(pending.access_key_hash))
        .filter(access_keys::Column::UsedAt.is_null())
        .exec(&txn)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;
    if consumed.rows_affected != 1 {
        return Err(error_response(
            StatusCode::CONFLICT,
            "Access key already used",
        ));
    }

    let session = issue_session(
        &txn,
        pending.user_id,
        req.device_id,
        req.device_name,
        req.platform,
        &headers,
        ip,
    )
    .await?;

    txn.commit()
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    info!(user_id = %pending.user_id, device_id = %session.device_id, "Registration successful");

    Ok(Json(RegisterFinishResponse {
        token: session.token,
        user_id: pending.user_id.to_string(),
        device_id: session.device_id.to_string(),
        server: ServerInfo {
            encryption_salt_b64: B64.encode(pending.encryption_salt),
            encryption_params: crypto::Argon2Params::default(),
        },
    }))
}

pub async fn login(
    State(state): State<AppState>,
    Extension(limiter): Extension<std::sync::Arc<RateLimiter>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<ErrorResponse>)> {
    let ip = peer_addr.ip().to_string();

    if !limiter.check(&ip) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse {
                error: "Too many requests".into(),
            }),
        ));
    }

    // Finish the OPAQUE login without receiving the raw passphrase or a DB-reusable secret.
    let auth_challenge = state
        .take_auth_challenge(&req.challenge_id)
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Invalid challenge"))?;
    let user = users::Entity::find_by_id(auth_challenge.user_id)
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Invalid challenge"))?;
    let credential_finalization = B64
        .decode(&req.credential_finalization_b64)
        .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid credential finalization"))?;

    crypto::opaque_server_login_finish(
        &auth_challenge.server_login_state,
        &credential_finalization,
    )
    .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid passphrase"))?;

    let session = issue_session(
        state.db(),
        user.id,
        req.device_id,
        req.device_name,
        req.platform,
        &headers,
        ip,
    )
    .await?;

    info!(user_id = %user.id, device_id = %session.device_id, "Login successful");

    Ok(Json(LoginResponse {
        token: session.token,
        user_id: user.id.to_string(),
        device_id: session.device_id.to_string(),
        server: server_info(&user),
    }))
}

pub async fn logout(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
) -> Result<Json<OkResponse>, StatusCode> {
    sessions::Entity::delete_by_id(auth.session_id)
        .exec(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    info!(device_id = %auth.device_id, "Logout");

    Ok(Json(OkResponse { ok: true }))
}

fn access_key_hash(access_key: &str) -> String {
    B64.encode(crypto::sha256(access_key.as_bytes()))
}

fn opaque_credential_identifier(user_id: Uuid) -> Vec<u8> {
    format!("clipper:user:{user_id}:passphrase:v1").into_bytes()
}

fn opaque_credential_identifier_for_user(user: &users::Model) -> Vec<u8> {
    if user.access_key_hash == "_legacy_single_user" {
        b"clipper:passphrase:v1".to_vec()
    } else {
        opaque_credential_identifier(user.id)
    }
}

fn server_info(user: &users::Model) -> ServerInfo {
    ServerInfo {
        encryption_salt_b64: B64.encode(&user.encryption_salt),
        encryption_params: crypto::Argon2Params::default(),
    }
}

async fn resolve_login_user(
    state: &AppState,
    user_id: Option<&str>,
) -> Result<users::Model, (StatusCode, Json<ErrorResponse>)> {
    if let Some(user_id) = user_id {
        let user_id = validate_client_id(user_id)?;
        return users::Entity::find_by_id(user_id)
            .one(state.db())
            .await
            .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
            .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Unknown user"));
    }

    let mut users = users::Entity::find()
        .limit(2)
        .all(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;
    match users.len() {
        1 => Ok(users.remove(0)),
        0 => Err(error_response(StatusCode::UNAUTHORIZED, "Unknown user")),
        _ => Err(error_response(
            StatusCode::BAD_REQUEST,
            "user_id is required",
        )),
    }
}

struct IssuedSession {
    token: String,
    device_id: Uuid,
}

async fn issue_session<C>(
    db: &C,
    user_id: Uuid,
    requested_device_id: Option<String>,
    device_name: Option<String>,
    platform: Option<String>,
    headers: &HeaderMap,
    ip: String,
) -> Result<IssuedSession, (StatusCode, Json<ErrorResponse>)>
where
    C: ConnectionTrait,
{
    let now = Utc::now().to_rfc3339();
    let device_id = match requested_device_id {
        Some(id) => validate_client_id(&id)?,
        None => Uuid::new_v4(),
    };
    let device_name = device_name.unwrap_or_else(|| "Unknown Device".into());
    let platform = platform.unwrap_or_else(|| "unknown".into());

    let existing_device = devices::Entity::find_by_id(device_id)
        .one(db)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    if let Some(existing_device) = existing_device {
        if existing_device.user_id != user_id {
            return Err(error_response(
                StatusCode::CONFLICT,
                "Device id already exists",
            ));
        }
        devices::Entity::update_many()
            .col_expr(
                devices::Column::LastSeenAt,
                sea_orm::sea_query::Expr::value(&now),
            )
            .col_expr(
                devices::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(&now),
            )
            .filter(devices::Column::Id.eq(device_id))
            .filter(devices::Column::UserId.eq(user_id))
            .exec(db)
            .await
            .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;
    } else {
        devices::ActiveModel {
            id: Set(device_id),
            user_id: Set(user_id),
            name: Set(device_name),
            platform: Set(platform),
            created_at: Set(now.clone()),
            updated_at: Set(now.clone()),
            last_seen_at: Set(now.clone()),
        }
        .insert(db)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;
    }

    let token_raw = crypto::generate_token();
    let token_hash = crypto::sha256(&token_raw);
    let token = B64.encode(token_raw);

    let session_id = Uuid::new_v4();
    let expires_at = (Utc::now() + Duration::days(30)).to_rfc3339();
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    sessions::ActiveModel {
        id: Set(session_id),
        token_hash: Set(token_hash.to_vec()),
        user_id: Set(user_id),
        device_id: Set(device_id),
        created_at: Set(now.clone()),
        expires_at: Set(expires_at),
        last_seen_at: Set(now),
        user_agent: Set(user_agent),
        ip_addr: Set(Some(ip)),
    }
    .insert(db)
    .await
    .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    Ok(IssuedSession { token, device_id })
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use sea_orm::{ActiveModelTrait, Database, Set};
    use tempfile::TempDir;

    use super::*;
    use crate::entity::access_keys;

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    async fn empty_state() -> (AppState, TempDir) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        let state = AppState::open_with_db(db, data_dir.path().to_path_buf())
            .await
            .expect("state");
        (state, data_dir)
    }

    async fn test_state(passphrase: &[u8]) -> (AppState, TempDir) {
        let (state, data_dir) = empty_state().await;
        let user_id = Uuid::new_v4();
        let opaque_server_setup = crypto::opaque_new_server_setup();
        let (registration_request, client_state) =
            crypto::opaque_client_register_start(passphrase).expect("client register start");
        let registration_response = crypto::opaque_server_register_start(
            &opaque_server_setup,
            &registration_request,
            &opaque_credential_identifier(user_id),
        )
        .expect("server register start");
        let registration_upload = crypto::opaque_client_register_finish(
            &client_state,
            passphrase,
            &registration_response,
        )
        .expect("client register finish");
        let opaque_password_file =
            crypto::opaque_server_register_finish(&registration_upload).expect("server finish");
        let encryption_salt = crypto::generate_encryption_salt();
        let now = Utc::now().to_rfc3339();
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
            opaque_server_setup: Set(opaque_server_setup),
            opaque_password_file: Set(opaque_password_file),
            encryption_salt: Set(encryption_salt.to_vec()),
            access_key_hash: Set(access_key_hash),
            created_at: Set(now.clone()),
            updated_at: Set(now),
        }
        .insert(state.db())
        .await
        .expect("insert config");

        (state, data_dir)
    }

    fn peer() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345))
    }

    fn challenge_request(passphrase: &[u8]) -> (LoginChallengeRequest, Vec<u8>) {
        let (credential_request, client_state) =
            crypto::opaque_client_login_start(passphrase).expect("client login start");
        (
            LoginChallengeRequest {
                user_id: None,
                credential_request_b64: B64.encode(credential_request),
            },
            client_state,
        )
    }

    fn registration_start_request(
        access_key: &str,
        passphrase: &[u8],
    ) -> (RegisterStartRequest, Vec<u8>) {
        let (registration_request, client_state) =
            crypto::opaque_client_register_start(passphrase).expect("client register start");
        (
            RegisterStartRequest {
                access_key: access_key.into(),
                registration_request_b64: B64.encode(registration_request),
            },
            client_state,
        )
    }

    fn registration_finish_request(
        registration: RegisterStartResponse,
        client_state: &[u8],
        passphrase: &[u8],
    ) -> RegisterFinishRequest {
        let registration_response = B64
            .decode(registration.registration_response_b64)
            .expect("registration response");
        let registration_upload =
            crypto::opaque_client_register_finish(client_state, passphrase, &registration_response)
                .expect("client register finish");

        RegisterFinishRequest {
            registration_id: registration.registration_id,
            registration_upload_b64: B64.encode(registration_upload),
            device_id: None,
            device_name: Some("test-device".into()),
            platform: Some("test".into()),
        }
    }

    async fn insert_access_key(state: &AppState, access_key: &str) {
        let now = Utc::now().to_rfc3339();
        access_keys::ActiveModel {
            key_hash: Set(access_key_hash(access_key)),
            created_at: Set(now),
            expires_at: Set(None),
            used_at: Set(None),
            used_by_user_id: Set(None),
        }
        .insert(state.db())
        .await
        .expect("insert access key");
    }

    #[tokio::test]
    async fn register_uses_access_key_and_never_receives_password() {
        let (state, _data_dir) = empty_state().await;
        let access_key = "invite-key-with-entropy";
        let passphrase = b"user private passphrase";
        insert_access_key(&state, access_key).await;

        let (start_req, client_state) = registration_start_request(access_key, passphrase);
        let Json(start_resp) = register_start(
            State(state.clone()),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            peer(),
            Json(start_req),
        )
        .await
        .expect("register start");
        let user_id = Uuid::parse_str(&start_resp.user_id).expect("user id");
        let finish_req = registration_finish_request(start_resp, &client_state, passphrase);

        let Json(finish_resp) = register_finish(
            State(state.clone()),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            peer(),
            HeaderMap::new(),
            Json(finish_req),
        )
        .await
        .expect("register finish");

        assert_eq!(finish_resp.user_id, user_id.to_string());
        assert!(!finish_resp.token.is_empty());
        assert!(!finish_resp.device_id.is_empty());

        let stored_user = users::Entity::find_by_id(user_id)
            .one(state.db())
            .await
            .expect("query user")
            .expect("user");
        assert!(!stored_user.opaque_password_file.is_empty());
        assert!(!stored_user.encryption_salt.is_empty());

        let used_key = access_keys::Entity::find_by_id(access_key_hash(access_key))
            .one(state.db())
            .await
            .expect("query access key")
            .expect("access key");
        assert_eq!(used_key.used_by_user_id, Some(user_id));
        assert!(used_key.used_at.is_some());

        let (challenge_req, client_login_state) =
            crypto::opaque_client_login_start(passphrase).expect("client login start");
        let Json(challenge_resp) = challenge(
            State(state.clone()),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            peer(),
            Json(LoginChallengeRequest {
                user_id: Some(user_id.to_string()),
                credential_request_b64: B64.encode(challenge_req),
            }),
        )
        .await
        .expect("challenge");
        let login_req = login_request(challenge_resp, &client_login_state, passphrase);
        let Json(login_resp) = login(
            State(state),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            peer(),
            HeaderMap::new(),
            Json(login_req),
        )
        .await
        .expect("login");
        assert_eq!(login_resp.user_id, user_id.to_string());
    }

    fn login_request(
        challenge: LoginChallengeResponse,
        client_state: &[u8],
        passphrase: &[u8],
    ) -> LoginRequest {
        let credential_response = B64
            .decode(challenge.credential_response_b64)
            .expect("credential response");
        let (credential_finalization, _) =
            crypto::opaque_client_login_finish(client_state, passphrase, &credential_response)
                .expect("client login finish");

        LoginRequest {
            challenge_id: challenge.challenge_id,
            credential_finalization_b64: B64.encode(credential_finalization),
            device_id: None,
            device_name: Some("test-device".into()),
            platform: Some("test".into()),
        }
    }

    // This exercises the happy path for OPAQUE login. We test it because
    // clients must prove knowledge of the passphrase without sending the raw
    // passphrase or a reusable verifier to the server.
    #[tokio::test]
    async fn login_accepts_opaque_finalization() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;

        let (challenge_req, client_state) = challenge_request(passphrase);
        let Json(challenge) = challenge(
            State(state.clone()),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            peer(),
            Json(challenge_req),
        )
        .await
        .expect("challenge");
        let req = login_request(challenge, &client_state, passphrase);

        let Json(resp) = login(
            State(state),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            peer(),
            HeaderMap::new(),
            Json(req),
        )
        .await
        .expect("login");

        assert!(!resp.token.is_empty());
        assert!(!resp.device_id.is_empty());
    }

    // This reuses a captured OPAQUE finalization against the same challenge.
    // We test it because login challenges must be single-use; replayable
    // finalizations would let a network observer or log leak mint fresh sessions.
    #[tokio::test]
    async fn login_challenge_is_single_use() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;

        let (challenge_req, client_state) = challenge_request(passphrase);
        let Json(challenge) = challenge(
            State(state.clone()),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            peer(),
            Json(challenge_req),
        )
        .await
        .expect("challenge");
        let req = login_request(challenge, &client_state, passphrase);
        let reused_req = LoginRequest {
            challenge_id: req.challenge_id.clone(),
            credential_finalization_b64: req.credential_finalization_b64.clone(),
            device_id: None,
            device_name: Some("test-device".into()),
            platform: Some("test".into()),
        };

        let _ = login(
            State(state.clone()),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            peer(),
            HeaderMap::new(),
            Json(req),
        )
        .await
        .expect("first login");

        let result = login(
            State(state),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            peer(),
            HeaderMap::new(),
            Json(reused_req),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_rejects_wrong_opaque_password() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;

        let (challenge_req, client_state) = challenge_request(b"wrong passphrase");
        let Json(challenge) = challenge(
            State(state.clone()),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            peer(),
            Json(challenge_req),
        )
        .await
        .expect("challenge");
        let credential_response = B64
            .decode(challenge.credential_response_b64)
            .expect("credential response");
        let finish = crypto::opaque_client_login_finish(
            &client_state,
            b"wrong passphrase",
            &credential_response,
        );

        assert!(finish.is_err());
    }

    #[tokio::test]
    async fn login_rate_limit_ignores_spoofed_forwarded_headers() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;
        let limiter = std::sync::Arc::new(RateLimiter::new());

        for i in 0..10 {
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-forwarded-for",
                format!("203.0.113.{i}").parse().expect("header"),
            );
            let result = login(
                State(state.clone()),
                Extension(limiter.clone()),
                peer(),
                headers,
                Json(LoginRequest {
                    challenge_id: format!("missing-{i}"),
                    credential_finalization_b64: B64.encode(b"not opaque"),
                    device_id: None,
                    device_name: None,
                    platform: None,
                }),
            )
            .await;

            assert_eq!(result.unwrap_err().0, StatusCode::UNAUTHORIZED);
        }

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.99".parse().expect("header"));
        let result = login(
            State(state),
            Extension(limiter),
            peer(),
            headers,
            Json(LoginRequest {
                challenge_id: "missing-final".into(),
                credential_finalization_b64: B64.encode(b"not opaque"),
                device_id: None,
                device_name: None,
                platform: None,
            }),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn challenge_rate_limits_opaque_password_attempts_by_peer() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;
        let limiter = std::sync::Arc::new(RateLimiter::new());

        for _ in 0..10 {
            let (challenge_req, _client_state) = challenge_request(b"candidate passphrase");
            let result = challenge(
                State(state.clone()),
                Extension(limiter.clone()),
                peer(),
                Json(challenge_req),
            )
            .await;
            assert!(result.is_ok());
        }

        let (challenge_req, _client_state) = challenge_request(b"candidate passphrase");
        let result = challenge(
            State(state),
            Extension(limiter),
            peer(),
            Json(challenge_req),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::TOO_MANY_REQUESTS);
    }
}
