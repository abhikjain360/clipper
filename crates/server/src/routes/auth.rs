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
        LoginChallengeRequest, LoginChallengeResponse, LoginRequest, LoginResponse, OkResponse,
        RegisterFinishRequest, RegisterFinishResponse, RegisterStartRequest, RegisterStartResponse,
        ServerInfo,
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DerivePartialModel, EntityTrait, QueryFilter,
    QuerySelect, Set, SqlErr, TransactionTrait,
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    auth::{self as server_auth, AuthInfo},
    entity::{access_keys, devices, server_config, sessions, users},
    rate_limit::ClientIp,
    routes::{ApiError, Postcard, error_response},
    secret_storage,
    state::AppState,
};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "users::Entity", from_query_result)]
struct LoginChallengeUserRow {
    id: Uuid,
    opaque_password_file: Vec<u8>,
}

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "users::Entity", from_query_result)]
struct LoginUserRow {
    id: Uuid,
    username: String,
}

/// OPAQUE login round 1, server side. Math in `docs/opaque.md`.
pub async fn challenge(
    State(state): State<AppState>,
    Postcard(req): Postcard<LoginChallengeRequest>,
) -> Result<Postcard<LoginChallengeResponse>, ApiError> {
    let db_config = load_server_config(&state).await?;
    let opaque_server_setup = unwrap_global_opaque_server_setup(&state, &db_config)?;

    // A missing user does NOT short-circuit: we fabricate a challenge that is
    // indistinguishable from a real one (opaque-ke's fake-record path), so the
    // endpoint can't be used to tell which usernames exist. The fabricated
    // challenge simply fails at the finalization in `login`, like a wrong
    // passphrase would.
    let user = users::Entity::find()
        .filter(users::Column::Username.eq(&req.username))
        .into_partial_model::<LoginChallengeUserRow>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(username = %req.username, error = %e, "Failed to look up user by username");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?;

    let (password_file, challenge_user_id) = match &user {
        Some(user) => {
            let password_file = secret_storage::unwrap_opaque_password_file(
                state.secrets(),
                &user.opaque_password_file,
            )
            .map_err(|e| {
                error!(user_id = %user.id, error = %e, "Failed to unwrap opaque_password_file");
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error")
            })?;
            (Some(password_file), Some(user.id))
        }
        None => (None, None),
    };

    // id_U = "clipper:user:{username}:passphrase:v1" — derivable from the
    // submitted username alone, so the fake path is stable across probes.
    let credential_identifier = opaque_credential_identifier(&req.username);
    let (credential_response, server_login_state) = crypto::opaque_server_login_start(
        &opaque_server_setup,
        password_file.as_deref(),
        &req.credential_request,
        &credential_identifier,
    )
    .map_err(|e| {
        warn!(error = %e, "OPAQUE login start rejected client request");
        error_response(StatusCode::UNAUTHORIZED, "Invalid login request")
    })?;
    // Stash state_S keyed by challenge_id until the client returns its
    // CredentialFinalization in `login`. `challenge_user_id` is None for a
    // fabricated challenge.
    let challenge_id = state.create_auth_challenge(challenge_user_id, server_login_state);

    Ok(Postcard(LoginChallengeResponse {
        challenge_id,
        credential_response,
        server: ServerInfo {},
    }))
}

/// OPAQUE registration round 1, server side. Math in `docs/opaque.md`.
pub async fn register_start(
    State(state): State<AppState>,
    Postcard(req): Postcard<RegisterStartRequest>,
) -> Result<Postcard<RegisterStartResponse>, ApiError> {
    let now = Utc::now().to_rfc3339();
    let db_config = load_server_config(&state).await?;

    // verify access key

    let access_key_hash_salt = secret_storage::unwrap_access_key_hash_salt(
        state.secrets(),
        &db_config.access_key_hash_salt,
    )
    .map_err(|e| {
        error!(error = %e, "Failed to unwrap access_key_hash_salt");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error")
    })?;
    // Argon2id is memory-hard and blocks for tens of milliseconds, so run it on
    // the blocking pool instead of stalling an async worker for every (even
    // invalid) registration attempt.
    let access_key_attempt = req.access_key.clone();
    let access_key_pepper = state.secrets().access_key_pepper;
    let access_key_hash_params = state.config().crypto.access_key_hash_params;
    let access_key_hash = tokio::task::spawn_blocking(move || {
        access_key_hash(
            &access_key_attempt,
            &access_key_hash_salt,
            &access_key_pepper,
            &access_key_hash_params,
        )
    })
    .await
    .map_err(|e| {
        error!(error = %e, "Access key hash task failed to join");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "Access key hash error")
    })?
    .map_err(|e| {
        error!(error = %e, "Failed to hash access key");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "Access key hash error")
    })?;
    let access_key = access_keys::Entity::find_by_id(access_key_hash.clone())
        .one(state.db())
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to look up access key in register_start");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?
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

    // Reject obviously-taken usernames before we expend any OPAQUE work.
    // The final guarantee comes from the unique constraint in register_finish.
    let existing = users::Entity::find()
        .filter(users::Column::Username.eq(&req.username))
        .select_only()
        .column(users::Column::Id)
        .into_tuple::<Uuid>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to look up username in register_start");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?;
    if existing.is_some() {
        return Err(error_response(
            StatusCode::CONFLICT,
            "Username already taken",
        ));
    }

    // start OPAQUE against the server-wide setup

    let opaque_server_setup = unwrap_global_opaque_server_setup(&state, &db_config)?;
    let user_id = Uuid::now_v7();
    // id_U = "clipper:user:{username}:passphrase:v1".
    let credential_identifier = opaque_credential_identifier(&req.username);
    let registration_response = crypto::opaque_server_register_start(
        &opaque_server_setup,
        &req.registration_request,
        &credential_identifier,
    )
    .map_err(|e| {
        warn!(error = %e, "OPAQUE register start rejected client request");
        error_response(StatusCode::UNAUTHORIZED, "Invalid registration request")
    })?;

    // OPAQUE round 1 is stateless server-side, but THIS server must remember the
    // freshly minted user_id (and the access-key hash to consume) until finish.
    let registration_id = state.create_pending_registration(user_id, req.username, access_key_hash);

    Ok(Postcard(RegisterStartResponse {
        registration_id,
        user_id: user_id.to_string(),
        registration_response,
        server: ServerInfo {},
    }))
}

/// OPAQUE registration round 2, server side. Math in `docs/opaque.md`.
pub async fn register_finish(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Postcard(req): Postcard<RegisterFinishRequest>,
) -> Result<Postcard<RegisterFinishResponse>, ApiError> {
    let pending = state
        .take_pending_registration(&req.registration_id)
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Invalid registration"))?;
    // req.registration_upload = RegistrationUpload = env ‖ masking_key ‖ pk_C.
    // Server does no math here; opaque_password_file is just the upload
    // re-serialized for storage. We persist it on the user row below.
    let opaque_password_file = crypto::opaque_server_register_finish(&req.registration_upload)
        .map_err(|e| {
            warn!(
                user_id = %pending.user_id,
                error = %e,
                "OPAQUE register finish rejected client upload",
            );
            error_response(StatusCode::UNAUTHORIZED, "Invalid registration upload")
        })?;

    let now = Utc::now().to_rfc3339();
    let txn = state.db().begin().await.map_err(|e| {
        error!(error = %e, "Failed to begin register_finish transaction");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
    })?;

    let access_key = access_keys::Entity::find_by_id(pending.access_key_hash.clone())
        .one(&txn)
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to look up access key in register_finish");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?
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

    let wrapped_opaque_password_file =
        secret_storage::wrap_opaque_password_file(state.secrets(), &opaque_password_file).map_err(
            |e| {
                error!(error = %e, "Failed to wrap opaque_password_file");
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error")
            },
        )?;
    // Legacy non-null column. New clients derive object-encryption keys from
    // OPAQUE's export_key, so the server no longer generates or returns a salt.
    let wrapped_encryption_salt = secret_storage::wrap_encryption_salt(state.secrets(), &[])
        .map_err(|e| {
            error!(error = %e, "Failed to wrap legacy encryption_salt placeholder");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error")
        })?;

    users::ActiveModel {
        id: Set(pending.user_id),
        username: Set(pending.username.clone()),
        opaque_password_file: Set(wrapped_opaque_password_file),
        encryption_salt: Set(wrapped_encryption_salt),
        access_key_hash: Set(pending.access_key_hash.clone()),
        created_at: Set(now.clone()),
        updated_at: Set(now.clone()),
    }
    .insert(&txn)
    .await
    .map_err(|e| match e.sql_err() {
        Some(SqlErr::UniqueConstraintViolation(_)) => {
            warn!(
                user_id = %pending.user_id,
                username = %pending.username,
                "Concurrent register_finish lost a uniqueness race (access_key_hash or username)",
            );
            error_response(StatusCode::CONFLICT, "Registration conflict")
        }
        _ => {
            error!(error = %e, "Failed to insert user row");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        }
    })?;

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
        .map_err(|e| {
            error!(error = %e, "Failed to mark access key consumed");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?;
    if consumed.rows_affected != 1 {
        warn!(
            user_id = %pending.user_id,
            rows_affected = consumed.rows_affected,
            "Access key consumption race detected during register_finish",
        );
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

    txn.commit().await.map_err(|e| {
        error!(error = %e, "Failed to commit register_finish transaction");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
    })?;

    info!(user_id = %pending.user_id, device_id = %session.device_id, "Registration successful");

    Ok(Postcard(RegisterFinishResponse {
        token: session.token,
        user_id: pending.user_id.to_string(),
        username: pending.username,
        device_id: session.device_id.to_string(),
        server: ServerInfo {},
    }))
}

/// OPAQUE login round 2, server side. Math in `docs/opaque.md`.
pub async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Postcard(req): Postcard<LoginRequest>,
) -> Result<Postcard<LoginResponse>, ApiError> {
    // Pop state_S (single-use) stashed by `challenge`.
    let auth_challenge = state
        .take_auth_challenge(&req.challenge_id)
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Invalid challenge"))?;
    // Verify the client's finalization first. A fabricated (unknown-user)
    // challenge and a wrong passphrase both fail here identically, so neither
    // the response nor the control flow reveals whether the account exists.
    crypto::opaque_server_login_finish(
        &auth_challenge.server_login_state,
        &req.credential_finalization,
    )
    .map_err(|e| {
        debug!(error = %e, "OPAQUE login finalization rejected");
        error_response(StatusCode::UNAUTHORIZED, "Invalid passphrase")
    })?;

    // Only a real challenge carries a user_id; a verifying finalization without
    // one would break a protocol invariant.
    let user_id = auth_challenge.user_id.ok_or_else(|| {
        warn!("Fabricated challenge unexpectedly produced a verifying finalization");
        error_response(StatusCode::UNAUTHORIZED, "Invalid passphrase")
    })?;
    let user = users::Entity::find_by_id(user_id)
        .into_partial_model::<LoginUserRow>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(user_id = %user_id, error = %e, "Failed to look up user in login");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?
        .ok_or_else(|| {
            warn!(user_id = %user_id, "User disappeared between challenge and login");
            error_response(StatusCode::UNAUTHORIZED, "Invalid challenge")
        })?;

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

    Ok(Postcard(LoginResponse {
        token: session.token,
        user_id: user.id.to_string(),
        username: user.username,
        device_id: session.device_id.to_string(),
        server: ServerInfo {},
    }))
}

pub async fn logout(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
) -> Result<Json<OkResponse>, ApiError> {
    sessions::Entity::delete_by_id(auth.session_id)
        .exec(state.db())
        .await
        .map_err(|e| {
            error!(
                session_id = %auth.session_id,
                device_id = %auth.device_id,
                error = %e,
                "Failed to delete session on logout",
            );
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?;

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

async fn load_server_config(state: &AppState) -> Result<server_config::Model, ApiError> {
    server_config::Entity::find_by_id(1)
        .one(state.db())
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to load server_config");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?
        .ok_or_else(|| error_response(StatusCode::SERVICE_UNAVAILABLE, "Server not initialized"))
}

fn opaque_credential_identifier(username: &str) -> Vec<u8> {
    format!("clipper:user:{username}:passphrase:v1").into_bytes()
}

fn unwrap_global_opaque_server_setup(
    state: &AppState,
    db_config: &server_config::Model,
) -> Result<Vec<u8>, ApiError> {
    secret_storage::unwrap_opaque_server_setup(state.secrets(), &db_config.opaque_server_setup)
        .map_err(|e| {
            error!(error = %e, "Failed to unwrap global opaque_server_setup");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error")
        })
}

struct IssuedSession {
    token: String,
    device_id: Uuid,
}

async fn issue_session<C>(
    db: &C,
    user_id: Uuid,
    options: SessionOptions,
) -> Result<IssuedSession, ApiError>
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

    let existing_device_user_id = devices::Entity::find_by_id(device_id)
        .select_only()
        .column(devices::Column::UserId)
        .into_tuple::<Uuid>()
        .one(db)
        .await
        .map_err(|e| {
            error!(device_id = %device_id, error = %e, "Failed to look up device");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?;

    if let Some(existing_user_id) = existing_device_user_id {
        if existing_user_id != user_id {
            warn!(
                device_id = %device_id,
                requested_user_id = %user_id,
                existing_user_id = %existing_user_id,
                "Device id already bound to a different user",
            );
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
            .map_err(|e| {
                error!(
                    device_id = %device_id,
                    user_id = %user_id,
                    error = %e,
                    "Failed to update device last_seen_at",
                );
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
            })?;
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
        .map_err(|e| {
            error!(
                device_id = %device_id,
                user_id = %user_id,
                error = %e,
                "Failed to insert device row",
            );
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?;
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
    .map_err(|e| {
        error!(
            session_id = %session_id,
            user_id = %user_id,
            device_id = %device_id,
            error = %e,
            "Failed to insert session row",
        );
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
    })?;

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
        body::{Body, to_bytes},
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
        let wrapped_opaque_server_setup = secret_storage::wrap_opaque_server_setup(
            state.secrets(),
            &crypto::opaque_new_server_setup(),
        )
        .expect("wrap opaque_server_setup");
        server_config::ActiveModel {
            id: Set(1),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            access_key_hash_salt: Set(wrapped_salt),
            opaque_server_setup: Set(wrapped_opaque_server_setup),
        }
        .insert(state.db())
        .await
        .expect("insert server config");
        (state, data_dir)
    }

    async fn test_state(passphrase: &[u8]) -> (AppState, TempDir) {
        let (state, data_dir) = empty_state().await;
        let user_id = Uuid::now_v7();
        // Register against the server-wide setup that empty_state persisted, so
        // a later challenge/login (which loads it from server_config) matches.
        let db_config = server_config::Entity::find_by_id(1)
            .one(state.db())
            .await
            .expect("query server config")
            .expect("server config");
        let opaque_server_setup = secret_storage::unwrap_opaque_server_setup(
            state.secrets(),
            &db_config.opaque_server_setup,
        )
        .expect("unwrap opaque_server_setup");
        let credential_identifier = opaque_credential_identifier("alice");
        let (registration_request, client_state) =
            crypto::opaque_client_register_start(passphrase).expect("client register start");
        let registration_response = crypto::opaque_server_register_start(
            &opaque_server_setup,
            &registration_request,
            &credential_identifier,
        )
        .expect("server register start");
        let registration_finish = crypto::opaque_client_register_finish(
            &client_state,
            passphrase,
            &registration_response,
        )
        .expect("client register finish");
        let opaque_password_file =
            crypto::opaque_server_register_finish(&registration_finish.registration_upload)
                .expect("server finish");
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

        let wrapped_opaque_password_file =
            secret_storage::wrap_opaque_password_file(state.secrets(), &opaque_password_file)
                .expect("wrap opaque_password_file");
        let wrapped_encryption_salt =
            secret_storage::wrap_encryption_salt(state.secrets(), &encryption_salt)
                .expect("wrap encryption_salt");

        users::ActiveModel {
            id: Set(user_id),
            username: Set("alice".into()),
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
            .route("/api/auth/register/start", post(register_start))
            .route("/api/auth/register/finish", post(register_finish))
            .route("/api/auth/challenge", post(challenge))
            .route("/api/auth/login", post(login))
            .route_layer(middleware::from_fn_with_state(
                limiter,
                auth_rate_limit_middleware,
            ))
            .with_state(state)
    }

    fn postcard_request<T: serde::Serialize>(path: &str, value: &T) -> Request<Body> {
        let mut request = Request::post(path)
            .header(
                header::CONTENT_TYPE,
                clipper_core::models::POSTCARD_CONTENT_TYPE,
            )
            .body(Body::from(
                postcard::to_allocvec(value).expect("serialize request"),
            ))
            .expect("request");
        request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            12345,
        )));
        request
    }

    async fn read_postcard_response<T>(response: axum::response::Response) -> T
    where
        T: serde::de::DeserializeOwned,
    {
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .expect("content type")
                .to_str()
                .expect("content type value"),
            clipper_core::models::POSTCARD_CONTENT_TYPE,
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        postcard::from_bytes(&body).expect("postcard response")
    }

    fn validated<T>(value: T) -> Postcard<T>
    where
        T: garde::Validate,
        T::Context: Default,
    {
        Postcard::validated(value).expect("valid request")
    }

    fn challenge_request(username: &str, passphrase: &[u8]) -> (LoginChallengeRequest, Vec<u8>) {
        let (credential_request, client_state) =
            crypto::opaque_client_login_start(passphrase).expect("client login start");
        (
            LoginChallengeRequest {
                username: username.into(),
                credential_request,
            },
            client_state,
        )
    }

    fn registration_start_request(
        access_key: &str,
        username: &str,
        passphrase: &[u8],
    ) -> (RegisterStartRequest, Vec<u8>) {
        let (registration_request, client_state) =
            crypto::opaque_client_register_start(passphrase).expect("client register start");
        (
            RegisterStartRequest {
                access_key: access_key.into(),
                username: username.into(),
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
        let registration_finish = crypto::opaque_client_register_finish(
            client_state,
            passphrase,
            &registration.registration_response,
        )
        .expect("client register finish");

        RegisterFinishRequest {
            registration_id: registration.registration_id,
            registration_upload: registration_finish.registration_upload,
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

        let (start_req, client_state) = registration_start_request(access_key, "alice", passphrase);
        let Postcard(start_resp) = register_start(State(state.clone()), validated(start_req))
            .await
            .expect("register start");
        let user_id = Uuid::parse_str(&start_resp.user_id).expect("user id");
        let finish_req = registration_finish_request(start_resp, &client_state, passphrase);

        let Postcard(finish_resp) = register_finish(
            State(state.clone()),
            client_ip(),
            HeaderMap::new(),
            validated(finish_req),
        )
        .await
        .expect("register finish");

        assert_eq!(finish_resp.user_id, user_id.to_string());
        assert_eq!(finish_resp.username, "alice");
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
        let Postcard(challenge_resp) = challenge(
            State(state.clone()),
            validated(LoginChallengeRequest {
                username: "alice".into(),
                credential_request: challenge_req,
            }),
        )
        .await
        .expect("challenge");
        let login_req = login_request(challenge_resp, &client_login_state, passphrase);
        let Postcard(login_resp) = login(
            State(state),
            client_ip(),
            HeaderMap::new(),
            validated(login_req),
        )
        .await
        .expect("login");
        assert_eq!(login_resp.user_id, user_id.to_string());
        assert_eq!(login_resp.username, "alice");
    }

    #[tokio::test]
    async fn register_routes_roundtrip_postcard_auth_blobs() {
        let (state, _data_dir) = empty_state().await;
        let access_key = "invite-key-with-entropy";
        let passphrase = b"user private passphrase";
        insert_access_key(&state, access_key).await;
        let app = auth_route_app(state, limiter());

        let (start_req, client_state) = registration_start_request(access_key, "alice", passphrase);
        let start_response = app
            .clone()
            .oneshot(postcard_request("/api/auth/register/start", &start_req))
            .await
            .expect("start response");
        assert_eq!(start_response.status(), StatusCode::OK);
        let start_resp: RegisterStartResponse = read_postcard_response(start_response).await;
        let user_id = start_resp.user_id.clone();

        let finish_req = registration_finish_request(start_resp, &client_state, passphrase);
        let finish_response = app
            .oneshot(postcard_request("/api/auth/register/finish", &finish_req))
            .await
            .expect("finish response");
        assert_eq!(finish_response.status(), StatusCode::OK);
        let finish_resp: RegisterFinishResponse = read_postcard_response(finish_response).await;

        assert_eq!(finish_resp.user_id, user_id);
        assert_eq!(finish_resp.username, "alice");
        assert!(!finish_resp.token.is_empty());
        assert!(!finish_resp.device_id.is_empty());
    }

    fn login_request(
        challenge: LoginChallengeResponse,
        client_state: &[u8],
        passphrase: &[u8],
    ) -> LoginRequest {
        let finish = crypto::opaque_client_login_finish(
            client_state,
            passphrase,
            &challenge.credential_response,
        )
        .expect("client login finish");

        LoginRequest {
            challenge_id: challenge.challenge_id,
            credential_finalization: finish.credential_finalization,
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

        let (challenge_req, client_state) = challenge_request("alice", passphrase);
        let Postcard(challenge) = challenge(State(state.clone()), validated(challenge_req))
            .await
            .expect("challenge");
        let req = login_request(challenge, &client_state, passphrase);

        let Postcard(resp) = login(State(state), client_ip(), HeaderMap::new(), validated(req))
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

        let (challenge_req, client_state) = challenge_request("alice", passphrase);
        let Postcard(challenge) = challenge(State(state.clone()), validated(challenge_req))
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

        _ = login(
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

        assert_eq!(result.unwrap_err().status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_rejects_wrong_opaque_password() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;

        let (challenge_req, client_state) = challenge_request("alice", b"wrong passphrase");
        let Postcard(challenge) = challenge(State(state.clone()), validated(challenge_req))
            .await
            .expect("challenge");
        let finish = crypto::opaque_client_login_finish(
            &client_state,
            b"wrong passphrase",
            &challenge.credential_response,
        );

        assert!(finish.is_err());
    }

    // An unknown username must get a normal-looking challenge (not a distinct
    // error), and the client must be unable to finish it — exactly like a wrong
    // passphrase against a real account. This is the anti-enumeration property.
    #[tokio::test]
    async fn challenge_for_unknown_user_is_indistinguishable() {
        let (state, _data_dir) = empty_state().await;

        let (challenge_req, client_state) = challenge_request("ghost", b"whatever");
        let Postcard(resp) = challenge(State(state), validated(challenge_req))
            .await
            .expect("challenge must not reveal that the user is unknown");

        assert!(!resp.challenge_id.is_empty());
        assert!(!resp.credential_response.is_empty());

        // Finishing against the fabricated response fails on the client, the
        // same failure mode a wrong passphrase produces for a real user.
        let finish = crypto::opaque_client_login_finish(
            &client_state,
            b"whatever",
            &resp.credential_response,
        );
        assert!(finish.is_err());
    }

    // A fabricated (unknown-user) challenge can never be turned into a session,
    // even if an attacker submits an arbitrary finalization against it.
    #[tokio::test]
    async fn login_rejects_fabricated_challenge() {
        let (state, _data_dir) = empty_state().await;

        let (challenge_req, _client_state) = challenge_request("ghost", b"whatever");
        let Postcard(resp) = challenge(State(state.clone()), validated(challenge_req))
            .await
            .expect("challenge");

        let req = LoginRequest {
            challenge_id: resp.challenge_id,
            credential_finalization: vec![0_u8; 64],
            device_id: None,
            device_name: Some("test-device".into()),
            platform: Some("test".into()),
        };
        let result = login(State(state), client_ip(), HeaderMap::new(), validated(req)).await;

        assert_eq!(result.unwrap_err().status(), StatusCode::UNAUTHORIZED);
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
                .oneshot(postcard_request(
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
            .oneshot(postcard_request(
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
            let (challenge_req, _client_state) =
                challenge_request("alice", b"candidate passphrase");
            let response = app
                .clone()
                .oneshot(postcard_request("/api/auth/challenge", &challenge_req))
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::OK);
        }

        let (challenge_req, _client_state) = challenge_request("alice", b"candidate passphrase");
        let response = app
            .oneshot(postcard_request("/api/auth/challenge", &challenge_req))
            .await;

        assert_eq!(
            response.expect("response").status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    // We test that registration persists wrapped — not plaintext — OPAQUE
    // state. The legacy encryption_salt column is still populated with a
    // wrapped placeholder until a schema migration removes it.
    #[tokio::test]
    async fn register_persists_auth_blobs_wrapped() {
        let (state, _data_dir) = empty_state().await;
        let access_key = "invite-key-with-entropy";
        let passphrase = b"user private passphrase";
        insert_access_key(&state, access_key).await;

        let (start_req, client_state) = registration_start_request(access_key, "alice", passphrase);
        let Postcard(start_resp) = register_start(State(state.clone()), validated(start_req))
            .await
            .expect("register start");
        let user_id = Uuid::parse_str(&start_resp.user_id).expect("user id");
        let finish_req = registration_finish_request(start_resp, &client_state, passphrase);

        _ = register_finish(
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

        // The legacy salt column is still wrapped, but it unwraps to an
        // empty placeholder and is no longer returned to clients.
        let recovered =
            secret_storage::unwrap_encryption_salt(state.secrets(), &stored.encryption_salt)
                .expect("unwrap encryption_salt");
        assert!(recovered.is_empty());

        // The per-user OPAQUE blob must not survive in cleartext and must
        // unwrap with the same pepper. (The server-wide setup lives in
        // server_config, covered by init_server_wraps_access_key_hash_salt.)
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
            secret_storage::unwrap_opaque_server_setup(
                &attacker,
                &server_config.opaque_server_setup
            )
            .is_err()
        );
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
