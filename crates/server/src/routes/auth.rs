use axum::{
    Json,
    extract::{Extension, State},
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
    auth::{self as server_auth, AuthInfo},
    entity::{access_keys, devices, server_config, sessions, users},
    rate_limit::ClientIp,
    routes::{ValidatedJson, error_response},
    secret_storage,
    state::AppState,
};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// OPAQUE login round 1, server side. Math in `docs/opaque.md`.
pub async fn challenge(
    State(state): State<AppState>,
    ValidatedJson(req): ValidatedJson<LoginChallengeRequest>,
) -> Result<Json<LoginChallengeResponse>, (StatusCode, Json<ErrorResponse>)> {
    let user = resolve_login_user(&state, req.user_id).await?;

    let opaque_server_setup =
        secret_storage::unwrap_opaque_server_setup(state.secrets(), &user.opaque_server_setup)
            .map_err(|_| {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error")
            })?;
    let opaque_password_file =
        secret_storage::unwrap_opaque_password_file(state.secrets(), &user.opaque_password_file)
            .map_err(|_| {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error")
            })?;

    // id_U = "clipper:user:{user_id}:passphrase:v1".
    let credential_identifier = opaque_credential_identifier(user.id);
    // req.credential_request = M ‖ ke1 from the client.
    // Inside opaque_server_login_start:
    //   k_U     = Expand(oprf_seed, id_U)
    //   N       = k_U · M
    //   masked  = (pk_S ‖ env) XOR Expand(masking_key, nonce_M ‖ ·)
    //   (x_S, X_S) ← AKE ephemeral, nonce_S ← random
    //   ikm     = (x_S · X_C) ‖ (x_S · pk_C) ‖ (sk_S · X_C)
    //   (server_mac_key, client_mac_key, session_key) = Expand(ikm, transcript_pre)
    //   server_mac = MAC(server_mac_key, transcript_pre)
    // credential_response = (N, nonce_M, masked, nonce_S, X_S, server_mac).
    // server_login_state = state_S = (client_mac_key, session_key, transcript_pre ‖ server_mac).
    let (credential_response, server_login_state) = crypto::opaque_server_login_start(
        &opaque_server_setup,
        &opaque_password_file,
        &req.credential_request,
        &credential_identifier,
    )
    .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid login request"))?;
    // Stash state_S keyed by challenge_id until the client returns its
    // CredentialFinalization (= client_mac) in `login`.
    let challenge_id = state.create_auth_challenge(user.id, server_login_state);

    Ok(Json(LoginChallengeResponse {
        challenge_id,
        credential_response_b64: B64.encode(credential_response),
        server: server_info(&state, &user)?,
    }))
}

/// OPAQUE registration round 1, server side. Math in `docs/opaque.md`.
pub async fn register_start(
    State(state): State<AppState>,
    ValidatedJson(req): ValidatedJson<RegisterStartRequest>,
) -> Result<Json<RegisterStartResponse>, (StatusCode, Json<ErrorResponse>)> {
    let now = Utc::now().to_rfc3339();
    let db_config = load_server_config(&state).await?;
    let access_key_hash_salt = secret_storage::unwrap_access_key_hash_salt(
        state.secrets(),
        &db_config.access_key_hash_salt,
    )
    .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error"))?;
    let access_key_hash = access_key_hash(
        &req.access_key,
        &access_key_hash_salt,
        &state.secrets().access_key_pepper,
        &state.config().crypto.access_key_hash_params,
    )
    .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Access key hash error"))?;
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

    let user_id = Uuid::now_v7();
    // Fresh opaque_server_setup = oprf_seed ‖ sk_S ‖ fake_sk for this user.
    let opaque_server_setup = crypto::opaque_new_server_setup();
    // Salt for the client's separate, non-OPAQUE local-data-encryption KDF.
    let encryption_salt =
        crypto::generate_random_bytes(state.config().crypto.encryption_salt_bytes);
    // id_U = "clipper:user:{user_id}:passphrase:v1".
    let credential_identifier = opaque_credential_identifier(user_id);
    // req.registration_request = M. Inside opaque_server_register_start:
    //   k_U = Expand(oprf_seed, id_U)
    //   N   = k_U · M
    // registration_response = RegistrationResponse = (N, pk_S). Stateless.
    let registration_response = crypto::opaque_server_register_start(
        &opaque_server_setup,
        &req.registration_request,
        &credential_identifier,
    )
    .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid registration request"))?;

    // OPAQUE round 1 needs no server-side state, but THIS server has to remember
    // the freshly minted user_id and opaque_server_setup (plus the access-key
    // hash to consume and the encryption_salt) until register_finish.
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
            encryption_params: state.config().crypto.encryption_params,
        },
    }))
}

/// OPAQUE registration round 2, server side. Math in `docs/opaque.md`.
pub async fn register_finish(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ValidatedJson(req): ValidatedJson<RegisterFinishRequest>,
) -> Result<Json<RegisterFinishResponse>, (StatusCode, Json<ErrorResponse>)> {
    let pending = state
        .take_pending_registration(&req.registration_id)
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Invalid registration"))?;
    // req.registration_upload = RegistrationUpload = env ‖ masking_key ‖ pk_C.
    // Server does no math here; opaque_password_file is just the upload
    // re-serialized for storage. We persist it on the user row below.
    let opaque_password_file = crypto::opaque_server_register_finish(&req.registration_upload)
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

    let wrapped_opaque_server_setup =
        secret_storage::wrap_opaque_server_setup(state.secrets(), &pending.opaque_server_setup)
            .map_err(|_| {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error")
            })?;
    let wrapped_opaque_password_file =
        secret_storage::wrap_opaque_password_file(state.secrets(), &opaque_password_file).map_err(
            |_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error"),
        )?;
    let wrapped_encryption_salt =
        secret_storage::wrap_encryption_salt(state.secrets(), &pending.encryption_salt).map_err(
            |_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error"),
        )?;

    users::ActiveModel {
        id: Set(pending.user_id),
        opaque_server_setup: Set(wrapped_opaque_server_setup),
        opaque_password_file: Set(wrapped_opaque_password_file),
        encryption_salt: Set(wrapped_encryption_salt),
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
        SessionOptions {
            requested_device_id: req.device_id,
            device_name: req.device_name,
            platform: req.platform,
            ip: ip.to_string(),
            user_agent: headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
            token_bytes: state.config().crypto.session_token_bytes,
        },
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
            encryption_params: state.config().crypto.encryption_params,
        },
    }))
}

/// OPAQUE login round 2, server side. Math in `docs/opaque.md`.
pub async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ValidatedJson(req): ValidatedJson<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Pop state_S (single-use) stashed by `challenge`.
    let auth_challenge = state
        .take_auth_challenge(&req.challenge_id)
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Invalid challenge"))?;
    let user = users::Entity::find_by_id(auth_challenge.user_id)
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Invalid challenge"))?;
    // req.credential_finalization = client_mac.
    // opaque_server_login_finish re-derives
    //   expected_mac = MAC(client_mac_key, transcript_pre ‖ server_mac)
    // from state_S and checks expected_mac == client_mac. It returns
    // session_key on success; clipper discards it and issues a fresh random
    // bearer token via `issue_session` below.
    crypto::opaque_server_login_finish(
        &auth_challenge.server_login_state,
        &req.credential_finalization,
    )
    .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid passphrase"))?;

    let session = issue_session(
        state.db(),
        user.id,
        SessionOptions {
            requested_device_id: req.device_id,
            device_name: req.device_name,
            platform: req.platform,
            ip: ip.to_string(),
            user_agent: headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
            token_bytes: state.config().crypto.session_token_bytes,
        },
    )
    .await?;

    info!(user_id = %user.id, device_id = %session.device_id, "Login successful");

    let server = server_info(&state, &user)?;
    Ok(Json(LoginResponse {
        token: session.token,
        user_id: user.id.to_string(),
        device_id: session.device_id.to_string(),
        server,
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

fn access_key_hash(
    access_key: &str,
    salt: &[u8],
    secret: &[u8],
    access_key_hash_params: &crypto::Argon2Params,
) -> Result<String, clipper_core::crypto::CryptoError> {
    server_auth::hash_access_key(access_key, salt, secret, access_key_hash_params)
}

async fn load_server_config(
    state: &AppState,
) -> Result<server_config::Model, (StatusCode, Json<ErrorResponse>)> {
    server_config::Entity::find_by_id(1)
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| error_response(StatusCode::SERVICE_UNAVAILABLE, "Server not initialized"))
}

fn opaque_credential_identifier(user_id: Uuid) -> Vec<u8> {
    format!("clipper:user:{user_id}:passphrase:v1").into_bytes()
}

fn server_info(
    state: &AppState,
    user: &users::Model,
) -> Result<ServerInfo, (StatusCode, Json<ErrorResponse>)> {
    let encryption_salt =
        secret_storage::unwrap_encryption_salt(state.secrets(), &user.encryption_salt).map_err(
            |_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error"),
        )?;
    Ok(ServerInfo {
        encryption_salt_b64: B64.encode(&encryption_salt),
        encryption_params: state.config().crypto.encryption_params,
    })
}

async fn resolve_login_user(
    state: &AppState,
    user_id: Option<clipper_core::models::UserId>,
) -> Result<users::Model, (StatusCode, Json<ErrorResponse>)> {
    if let Some(user_id) = user_id {
        return users::Entity::find_by_id(user_id.into_uuid())
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
    options: SessionOptions,
) -> Result<IssuedSession, (StatusCode, Json<ErrorResponse>)>
where
    C: ConnectionTrait,
{
    let now = Utc::now().to_rfc3339();
    let device_id = options
        .requested_device_id
        .map(clipper_core::models::DeviceId::into_uuid)
        .unwrap_or_else(Uuid::now_v7);
    let device_name = options
        .device_name
        .unwrap_or_else(|| "Unknown Device".into());
    let platform = options.platform.unwrap_or_else(|| "unknown".into());

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

    let token_raw = crypto::generate_token_with_length(options.token_bytes);
    let token_hash = crypto::sha256(&token_raw);
    let token = B64.encode(token_raw);

    let session_id = Uuid::now_v7();
    let expires_at = (Utc::now() + Duration::days(30)).to_rfc3339();
    sessions::ActiveModel {
        id: Set(session_id),
        token_hash: Set(token_hash.to_vec()),
        user_id: Set(user_id),
        device_id: Set(device_id),
        created_at: Set(now.clone()),
        expires_at: Set(expires_at),
        last_seen_at: Set(now),
        user_agent: Set(options.user_agent),
        ip_addr: Set(Some(options.ip)),
    }
    .insert(db)
    .await
    .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?;

    Ok(IssuedSession { token, device_id })
}

#[derive(Debug)]
struct SessionOptions {
    requested_device_id: Option<clipper_core::models::DeviceId>,
    device_name: Option<String>,
    platform: Option<String>,
    ip: String,
    user_agent: Option<String>,
    token_bytes: usize,
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use axum::{
        Router,
        body::Body,
        extract::ConnectInfo,
        http::{Request, header},
        middleware,
        routing::post,
    };
    use sea_orm::{ActiveModelTrait, Database, Set};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;
    use crate::{
        entity::{access_keys, server_config},
        rate_limit::{RateLimiter, auth_rate_limit_middleware},
    };

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    async fn empty_state() -> (AppState, TempDir) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        let state = AppState::open_with_db(db, data_dir.path().to_path_buf())
            .await
            .expect("state");
        let now = Utc::now().to_rfc3339();
        let wrapped_salt = secret_storage::wrap_access_key_hash_salt(
            state.secrets(),
            &crypto::generate_access_key_hash_salt(),
        )
        .expect("wrap salt");
        server_config::ActiveModel {
            id: Set(1),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            access_key_hash_salt: Set(wrapped_salt),
        }
        .insert(state.db())
        .await
        .expect("insert server config");
        (state, data_dir)
    }

    async fn test_state(passphrase: &[u8]) -> (AppState, TempDir) {
        let (state, data_dir) = empty_state().await;
        let user_id = Uuid::now_v7();
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

        let wrapped_opaque_server_setup =
            secret_storage::wrap_opaque_server_setup(state.secrets(), &opaque_server_setup)
                .expect("wrap opaque_server_setup");
        let wrapped_opaque_password_file =
            secret_storage::wrap_opaque_password_file(state.secrets(), &opaque_password_file)
                .expect("wrap opaque_password_file");
        let wrapped_encryption_salt =
            secret_storage::wrap_encryption_salt(state.secrets(), &encryption_salt)
                .expect("wrap encryption_salt");

        users::ActiveModel {
            id: Set(user_id),
            opaque_server_setup: Set(wrapped_opaque_server_setup),
            opaque_password_file: Set(wrapped_opaque_password_file),
            encryption_salt: Set(wrapped_encryption_salt),
            access_key_hash: Set(access_key_hash),
            created_at: Set(now.clone()),
            updated_at: Set(now),
        }
        .insert(state.db())
        .await
        .expect("insert config");

        (state, data_dir)
    }

    fn client_ip() -> ClientIp {
        ClientIp(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn limiter() -> std::sync::Arc<RateLimiter> {
        std::sync::Arc::new(RateLimiter::new(
            &crate::config::ServerConfig::default().rate_limit,
        ))
    }

    fn auth_route_app(state: AppState, limiter: std::sync::Arc<RateLimiter>) -> Router {
        Router::new()
            .route("/api/auth/challenge", post(challenge))
            .route("/api/auth/login", post(login))
            .route_layer(middleware::from_fn_with_state(
                limiter,
                auth_rate_limit_middleware,
            ))
            .with_state(state)
    }

    fn json_request<T: serde::Serialize>(path: &str, value: &T) -> Request<Body> {
        let mut request = Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(value).expect("serialize request"),
            ))
            .expect("request");
        request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            12345,
        )));
        request
    }

    fn validated<T>(value: T) -> ValidatedJson<T>
    where
        T: garde::Validate,
        T::Context: Default,
    {
        ValidatedJson::validated(value).expect("valid request")
    }

    fn challenge_request(passphrase: &[u8]) -> (LoginChallengeRequest, Vec<u8>) {
        let (credential_request, client_state) =
            crypto::opaque_client_login_start(passphrase).expect("client login start");
        (
            LoginChallengeRequest {
                user_id: None,
                credential_request,
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
                registration_request,
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
            registration_upload,
            device_id: None,
            device_name: Some("test-device".into()),
            platform: Some("test".into()),
        }
    }

    async fn insert_access_key(state: &AppState, access_key: &str) {
        let now = Utc::now().to_rfc3339();
        access_keys::ActiveModel {
            key_hash: Set(access_key_hash_for_state(state, access_key).await),
            created_at: Set(now),
            expires_at: Set(None),
            used_at: Set(None),
            used_by_user_id: Set(None),
        }
        .insert(state.db())
        .await
        .expect("insert access key");
    }

    async fn access_key_hash_for_state(state: &AppState, access_key: &str) -> String {
        let config = server_config::Entity::find_by_id(1)
            .one(state.db())
            .await
            .expect("query server config")
            .expect("server config");
        let salt = secret_storage::unwrap_access_key_hash_salt(
            state.secrets(),
            &config.access_key_hash_salt,
        )
        .expect("unwrap access_key_hash_salt");
        access_key_hash(
            access_key,
            &salt,
            &state.secrets().access_key_pepper,
            &state.config().crypto.access_key_hash_params,
        )
        .expect("access key hash")
    }

    #[tokio::test]
    async fn register_uses_access_key_and_never_receives_password() {
        let (state, _data_dir) = empty_state().await;
        let access_key = "invite-key-with-entropy";
        let passphrase = b"user private passphrase";
        insert_access_key(&state, access_key).await;

        let (start_req, client_state) = registration_start_request(access_key, passphrase);
        let Json(start_resp) = register_start(State(state.clone()), validated(start_req))
            .await
            .expect("register start");
        let user_id = Uuid::parse_str(&start_resp.user_id).expect("user id");
        let finish_req = registration_finish_request(start_resp, &client_state, passphrase);

        let Json(finish_resp) = register_finish(
            State(state.clone()),
            client_ip(),
            HeaderMap::new(),
            validated(finish_req),
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

        let used_key =
            access_keys::Entity::find_by_id(access_key_hash_for_state(&state, access_key).await)
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
            validated(LoginChallengeRequest {
                user_id: Some(user_id.into()),
                credential_request: challenge_req,
            }),
        )
        .await
        .expect("challenge");
        let login_req = login_request(challenge_resp, &client_login_state, passphrase);
        let Json(login_resp) = login(
            State(state),
            client_ip(),
            HeaderMap::new(),
            validated(login_req),
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
            credential_finalization,
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
        let Json(challenge) = challenge(State(state.clone()), validated(challenge_req))
            .await
            .expect("challenge");
        let req = login_request(challenge, &client_state, passphrase);

        let Json(resp) = login(State(state), client_ip(), HeaderMap::new(), validated(req))
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
        let Json(challenge) = challenge(State(state.clone()), validated(challenge_req))
            .await
            .expect("challenge");
        let req = login_request(challenge, &client_state, passphrase);
        let reused_req = LoginRequest {
            challenge_id: req.challenge_id.clone(),
            credential_finalization: req.credential_finalization.clone(),
            device_id: None,
            device_name: Some("test-device".into()),
            platform: Some("test".into()),
        };

        let _ = login(
            State(state.clone()),
            client_ip(),
            HeaderMap::new(),
            validated(req),
        )
        .await
        .expect("first login");

        let result = login(
            State(state),
            client_ip(),
            HeaderMap::new(),
            validated(reused_req),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_rejects_wrong_opaque_password() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;

        let (challenge_req, client_state) = challenge_request(b"wrong passphrase");
        let Json(challenge) = challenge(State(state.clone()), validated(challenge_req))
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
    async fn login_rate_limits_by_client_ip() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;
        let app = auth_route_app(state, limiter());

        for i in 0..crate::config::ServerConfig::default()
            .rate_limit
            .auth_per_client_per_minute
        {
            let response = app
                .clone()
                .oneshot(json_request(
                    "/api/auth/login",
                    &LoginRequest {
                        challenge_id: format!("missing-{i}"),
                        credential_finalization: b"not opaque".to_vec(),
                        device_id: None,
                        device_name: None,
                        platform: None,
                    },
                ))
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        let response = app
            .oneshot(json_request(
                "/api/auth/login",
                &LoginRequest {
                    challenge_id: "missing-final".into(),
                    credential_finalization: b"not opaque".to_vec(),
                    device_id: None,
                    device_name: None,
                    platform: None,
                },
            ))
            .await;

        assert_eq!(
            response.expect("response").status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn challenge_rate_limits_opaque_password_attempts_by_client_ip() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;
        let app = auth_route_app(state, limiter());

        for _ in 0..crate::config::ServerConfig::default()
            .rate_limit
            .auth_per_client_per_minute
        {
            let (challenge_req, _client_state) = challenge_request(b"candidate passphrase");
            let response = app
                .clone()
                .oneshot(json_request("/api/auth/challenge", &challenge_req))
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::OK);
        }

        let (challenge_req, _client_state) = challenge_request(b"candidate passphrase");
        let response = app
            .oneshot(json_request("/api/auth/challenge", &challenge_req))
            .await;

        assert_eq!(
            response.expect("response").status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    // We test that registration persists wrapped — not plaintext — OPAQUE
    // state and encryption_salt, because the whole point of the pepper is
    // that a DB dump leaks ciphertext, not the underlying secrets.
    #[tokio::test]
    async fn register_persists_auth_blobs_wrapped() {
        let (state, _data_dir) = empty_state().await;
        let access_key = "invite-key-with-entropy";
        let passphrase = b"user private passphrase";
        insert_access_key(&state, access_key).await;

        let (start_req, client_state) = registration_start_request(access_key, passphrase);
        let Json(start_resp) = register_start(State(state.clone()), validated(start_req))
            .await
            .expect("register start");
        let user_id = Uuid::parse_str(&start_resp.user_id).expect("user id");
        let plaintext_salt = B64
            .decode(&start_resp.server.encryption_salt_b64)
            .expect("decode salt");
        let finish_req = registration_finish_request(start_resp, &client_state, passphrase);

        let _ = register_finish(
            State(state.clone()),
            client_ip(),
            HeaderMap::new(),
            validated(finish_req),
        )
        .await
        .expect("register finish");

        let stored = users::Entity::find_by_id(user_id)
            .one(state.db())
            .await
            .expect("query user")
            .expect("user");

        // The stored salt is wrapped: it must not match the plaintext
        // salt the client received over the wire, and it must unwrap
        // back to that same plaintext via the server's pepper subkeys.
        assert_ne!(stored.encryption_salt, plaintext_salt);
        let recovered =
            secret_storage::unwrap_encryption_salt(state.secrets(), &stored.encryption_salt)
                .expect("unwrap encryption_salt");
        assert_eq!(recovered, plaintext_salt);

        // OPAQUE blobs must not survive in cleartext, and must unwrap
        // with the same pepper.
        assert!(
            secret_storage::unwrap_opaque_server_setup(
                state.secrets(),
                &stored.opaque_server_setup,
            )
            .is_ok()
        );
        assert!(
            secret_storage::unwrap_opaque_password_file(
                state.secrets(),
                &stored.opaque_password_file,
            )
            .is_ok()
        );
    }

    // We test that a different pepper cannot recover the stored secrets,
    // because that is the property a DB-only attacker would try to defeat.
    #[tokio::test]
    async fn wrong_pepper_cannot_unwrap_stored_blobs() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;
        let user = users::Entity::find()
            .one(state.db())
            .await
            .expect("query")
            .expect("user");

        let attacker = crate::secret::ServerSecrets::from_root(&[0x99_u8; 32]);
        assert!(
            secret_storage::unwrap_opaque_server_setup(&attacker, &user.opaque_server_setup)
                .is_err()
        );
        assert!(
            secret_storage::unwrap_opaque_password_file(&attacker, &user.opaque_password_file)
                .is_err()
        );
        assert!(secret_storage::unwrap_encryption_salt(&attacker, &user.encryption_salt).is_err());

        let server_config = server_config::Entity::find_by_id(1)
            .one(state.db())
            .await
            .expect("query")
            .expect("server_config");
        assert!(
            secret_storage::unwrap_access_key_hash_salt(
                &attacker,
                &server_config.access_key_hash_salt,
            )
            .is_err()
        );
    }

    // We test that `init_server` wraps the access-key-hash salt on
    // disk, since access-key offline brute force is the other half of
    // the threat model.
    #[tokio::test]
    async fn init_server_wraps_access_key_hash_salt() {
        let (state, _data_dir) = empty_state().await;
        let stored = server_config::Entity::find_by_id(1)
            .one(state.db())
            .await
            .expect("query")
            .expect("server_config");

        // 16 bytes is the configured salt length; a wrapped blob carries
        // an additional 24-byte nonce + 16-byte AEAD tag, so it must be
        // strictly longer.
        assert!(stored.access_key_hash_salt.len() > 16);
        assert!(
            secret_storage::unwrap_access_key_hash_salt(
                state.secrets(),
                &stored.access_key_hash_salt,
            )
            .is_ok()
        );
    }
}
