use axum::{
    Json,
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode},
};
use base64::Engine;
use chrono::{Duration, Utc};
use clipper_core::{
    crypto,
    models::{
        DEVICE_LOGIN_PROOF_VERSION, DeviceListItem, DeviceListResponse, DeviceLoginProofBodyV1,
        LoginChallengeRequest, LoginChallengeResponse, LoginRequest, LoginResponse, OkResponse,
        RegisterFinishRequest, RegisterFinishResponse, RegisterStartRequest, RegisterStartResponse,
        ServerInfo,
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DerivePartialModel, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, Set, SqlErr, TransactionTrait,
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    auth::{self as server_auth, AuthInfo},
    entity::{access_keys, devices, server_config, sessions, users},
    rate_limit::{ClientIp, rate_limited_error},
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

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "devices::Entity", from_query_result)]
struct ExistingDeviceRow {
    user_id: Uuid,
    signing_public_key: Vec<u8>,
}

/// OPAQUE login round 1, server side. Math in `docs/opaque.md`.
pub async fn challenge(
    State(state): State<AppState>,
    Postcard(req): Postcard<LoginChallengeRequest>,
) -> Result<Postcard<LoginChallengeResponse>, ApiError> {
    // The per-client middleware cannot see the username, so the per-username
    // budget — the backstop against distributed guessing that rotates source
    // addresses — is enforced here, once the body is parsed.
    if !state.rate_limiter().check_auth_username(&req.username) {
        return Err(rate_limited_error());
    }

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
                user.id,
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
    let device_proof_challenge =
        crypto::generate_random_bytes(crypto::DEVICE_LOGIN_PROOF_CHALLENGE_BYTES);
    let challenge_id = state.create_auth_challenge(
        challenge_user_id,
        server_login_state,
        device_proof_challenge.clone(),
    );

    Ok(Postcard(LoginChallengeResponse {
        challenge_id,
        credential_response,
        device_proof_challenge,
        server: ServerInfo {},
    }))
}

/// OPAQUE registration round 1, server side. Math in `docs/opaque.md`.
pub async fn register_start(
    State(state): State<AppState>,
    Postcard(req): Postcard<RegisterStartRequest>,
) -> Result<Postcard<RegisterStartResponse>, ApiError> {
    let db_config = load_server_config(&state).await?;
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
    let access_key_hash =
        verify_unused_registration_access_key(&state, &db_config, &req.access_key).await?;

    // Username uniqueness is enforced at register_finish by the users.username
    // unique constraint. Keeping register_start response-shaped for taken and
    // available names avoids a username-existence oracle.

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

    let wrapped_opaque_password_file = secret_storage::wrap_opaque_password_file(
        state.secrets(),
        pending.user_id,
        &opaque_password_file,
    )
    .map_err(|e| {
        error!(error = %e, "Failed to wrap opaque_password_file");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error")
    })?;
    // Legacy non-null column. New clients derive object-encryption keys from
    // OPAQUE's export_key, so the server no longer generates or returns a salt.
    let wrapped_encryption_salt =
        secret_storage::wrap_encryption_salt(state.secrets(), pending.user_id, &[]).map_err(
            |e| {
                error!(error = %e, "Failed to wrap legacy encryption_salt placeholder");
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error")
            },
        )?;

    // Insert the user BEFORE consuming the invite, so a duplicate-username (or a
    // concurrent double-spend) fails here and rolls back without ever marking the
    // one-time access key used. The users.access_key_hash unique constraint is
    // what makes the invite single-use across concurrent finishes.
    let user_insert = users::ActiveModel {
        id: Set(pending.user_id),
        username: Set(pending.username.clone()),
        opaque_password_file: Set(wrapped_opaque_password_file),
        encryption_salt: Set(wrapped_encryption_salt),
        access_key_hash: Set(pending.access_key_hash.clone()),
        created_at: Set(now.clone()),
        updated_at: Set(now.clone()),
        storage_bytes: Set(0),
        object_count: Set(0),
    }
    .insert(&txn)
    .await;
    if let Err(e) = user_insert {
        return match e.sql_err() {
            Some(SqlErr::UniqueConstraintViolation(_)) => {
                // Roll back WITHOUT committing so the invite stays unconsumed for
                // its legitimate holder — returning Err drops the transaction,
                // which rolls it back.
                warn!(
                    user_id = %pending.user_id,
                    username = %pending.username,
                    "Registration conflict; rolling back so the access key is not consumed",
                );
                Err(error_response(
                    StatusCode::CONFLICT,
                    "Registration conflict",
                ))
            }
            _ => {
                error!(error = %e, "Failed to insert user row");
                Err(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Database error",
                ))
            }
        };
    }

    // The username was free and the user row now exists, so spend the invite
    // exactly once. Consuming only after a successful insert is what prevents a
    // duplicate-username attempt from burning someone else's access key.
    consume_registration_access_key(&txn, &pending.access_key_hash, &now, pending.user_id).await?;
    bind_consumed_registration_access_key_to_user(&txn, &pending.access_key_hash, pending.user_id)
        .await?;

    let session = issue_session(
        &txn,
        pending.user_id,
        SessionOptions {
            requested_device_id: req.device_id,
            device_signing_public_key: req.device_signing_public_key,
            device_name: req.device_name,
            platform: req.platform,
            ip: ip.to_string(),
            user_agent: headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
            token_bytes: state.config().crypto.session_token_bytes,
            max_devices: state.config().limits.max_user_devices,
            session_proof: SessionProof::Registration,
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

async fn consume_registration_access_key<C>(
    db: &C,
    access_key_hash: &str,
    now: &str,
    user_id: Uuid,
) -> Result<(), ApiError>
where
    C: ConnectionTrait,
{
    let consumed = access_keys::Entity::update_many()
        .col_expr(
            access_keys::Column::UsedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(access_keys::Column::KeyHash.eq(access_key_hash))
        .filter(access_keys::Column::UsedAt.is_null())
        .exec(db)
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to mark access key consumed");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?;
    if consumed.rows_affected != 1 {
        warn!(
            user_id = %user_id,
            rows_affected = consumed.rows_affected,
            "Access key consumption race detected during register_finish",
        );
        return Err(error_response(
            StatusCode::CONFLICT,
            "Access key already used",
        ));
    }

    Ok(())
}

async fn bind_consumed_registration_access_key_to_user<C>(
    db: &C,
    access_key_hash: &str,
    user_id: Uuid,
) -> Result<(), ApiError>
where
    C: ConnectionTrait,
{
    let bound = access_keys::Entity::update_many()
        .col_expr(
            access_keys::Column::UsedByUserId,
            sea_orm::sea_query::Expr::value(user_id),
        )
        .filter(access_keys::Column::KeyHash.eq(access_key_hash))
        .filter(access_keys::Column::UsedAt.is_not_null())
        .filter(access_keys::Column::UsedByUserId.is_null())
        .exec(db)
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to bind consumed access key to user");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?;
    if bound.rows_affected != 1 {
        error!(
            user_id = %user_id,
            rows_affected = bound.rows_affected,
            "Consumed access key could not be bound to newly inserted user",
        );
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database error",
        ));
    }

    Ok(())
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
            device_signing_public_key: req.device_signing_public_key,
            device_name: req.device_name,
            platform: req.platform,
            ip: ip.to_string(),
            user_agent: headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
            token_bytes: state.config().crypto.session_token_bytes,
            max_devices: state.config().limits.max_user_devices,
            session_proof: SessionProof::Login(DeviceLoginProof {
                challenge_id: req.challenge_id,
                challenge: auth_challenge.device_proof_challenge,
                username: user.username.clone(),
                signature: req.device_login_proof_signature,
            }),
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

    Ok(Json(OkResponse {}))
}

/// `GET /api/auth/devices` — list the authenticated user's registered devices,
/// most recently seen first. Scoped to `auth.user_id` so it never surfaces
/// another user's devices; internal columns (`user_id`, `signing_public_key`)
/// are not exposed.
pub async fn list_devices(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
) -> Result<Postcard<DeviceListResponse>, ApiError> {
    let rows = devices::Entity::find()
        .filter(devices::Column::UserId.eq(auth.user_id))
        .order_by_desc(devices::Column::LastSeenAt)
        .all(state.db())
        .await
        .map_err(|e| {
            error!(user_id = %auth.user_id, error = %e, "Failed to list devices");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?;

    let devices = rows
        .into_iter()
        .map(|device| DeviceListItem {
            id: device.id.into(),
            name: device.name,
            platform: device.platform,
            created_at: device.created_at,
            last_seen_at: device.last_seen_at,
        })
        .collect();

    Ok(Postcard(DeviceListResponse { devices }))
}

/// `DELETE /api/auth/devices/{id}` — remove one of the authenticated user's
/// devices (the counterpart to the per-user device cap). The delete is scoped to
/// `auth.user_id`, so a user can only remove their own devices and an unknown or
/// foreign id returns 404 rather than touching another account.
///
/// Removing the row relies on the device FKs: `sessions.device_id` is
/// `ON DELETE CASCADE`, so the device's sessions (its live bearer tokens) are
/// revoked; `objects.source_device_id` is `ON DELETE SET NULL`, so the objects
/// it created are detached and preserved, not deleted. A user may remove the
/// device they are currently using — that just invalidates the current session,
/// equivalent to logging out.
pub async fn delete_device(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
    Path(device_id): Path<String>,
) -> Result<Postcard<OkResponse>, ApiError> {
    let device_uuid = Uuid::parse_str(&device_id)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid device id"))?;

    let deleted = devices::Entity::delete_many()
        .filter(devices::Column::Id.eq(device_uuid))
        .filter(devices::Column::UserId.eq(auth.user_id))
        .exec(state.db())
        .await
        .map_err(|e| {
            error!(
                user_id = %auth.user_id,
                device_id = %device_uuid,
                error = %e,
                "Failed to delete device",
            );
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?;

    if deleted.rows_affected == 0 {
        return Err(error_response(StatusCode::NOT_FOUND, "Device not found"));
    }

    info!(user_id = %auth.user_id, device_id = %device_uuid, "Device removed");
    Ok(Postcard(OkResponse {}))
}

fn access_key_hash(
    access_key: &str,
    salt: &[u8],
    secret: &[u8],
    access_key_hash_params: &crypto::Argon2Params,
) -> Result<String, clipper_core::crypto::CryptoError> {
    server_auth::hash_access_key(access_key, salt, secret, access_key_hash_params)
}

async fn verify_unused_registration_access_key(
    state: &AppState,
    db_config: &server_config::Model,
    access_key_attempt: &str,
) -> Result<String, ApiError> {
    let access_key_hash_salt = secret_storage::unwrap_access_key_hash_salt(
        state.secrets(),
        &db_config.access_key_hash_salt,
    )
    .map_err(|e| {
        error!(error = %e, "Failed to unwrap access_key_hash_salt");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server secret error")
    })?;
    // Argon2id is memory-hard (~19 MiB) and blocks for tens of milliseconds, so
    // run it on the blocking pool instead of stalling an async worker for every
    // (even invalid) registration attempt — and gate it behind a semaphore so a
    // burst of attempts cannot run hundreds of hashes at once and exhaust memory.
    let access_key_attempt = access_key_attempt.to_owned();
    let access_key_pepper = state.secrets().access_key_pepper;
    let access_key_hash_params = state.config().crypto.access_key_hash_params;
    let access_key_hash = {
        let _permit = state
            .argon2_semaphore()
            .acquire_owned()
            .await
            .map_err(|e| {
                error!(error = %e, "Argon2 semaphore unexpectedly closed");
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Access key hash error")
            })?;
        tokio::task::spawn_blocking(move || {
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
        })?
    };
    let access_key = access_keys::Entity::find_by_id(access_key_hash.clone())
        .one(state.db())
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to look up access key in register_start");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Invalid access key"))?;

    let now = Utc::now().to_rfc3339();
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

    Ok(access_key_hash)
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

    let existing_device = devices::Entity::find_by_id(device_id)
        .into_partial_model::<ExistingDeviceRow>()
        .one(db)
        .await
        .map_err(|e| {
            error!(device_id = %device_id, error = %e, "Failed to look up device");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?;

    if let Some(existing_device) = existing_device {
        if existing_device.user_id != user_id {
            warn!(
                device_id = %device_id,
                requested_user_id = %user_id,
                existing_user_id = %existing_device.user_id,
                "Device id already bound to a different user",
            );
            return Err(error_response(
                StatusCode::CONFLICT,
                "Device id already exists",
            ));
        }
        if existing_device.signing_public_key != options.device_signing_public_key {
            warn!(
                device_id = %device_id,
                user_id = %user_id,
                "Device id was presented with a different signing public key",
            );
            return Err(error_response(
                StatusCode::CONFLICT,
                "Device signing key mismatch",
            ));
        }
        verify_existing_device_login_proof(
            user_id,
            device_id,
            &existing_device.signing_public_key,
            &options.device_signing_public_key,
            &options.session_proof,
        )?;
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
        // A new-device login mints a fresh device row. Cap how many a user can
        // accumulate: device rows sit outside the storage/object quotas, so
        // without this a credentialed account could grow the table without
        // bound. Existing devices re-authenticating take the branch above and
        // are never blocked; a user at the cap reclaims a device to free a slot.
        //
        // The count and insert are not one atomic step (login runs outside a
        // transaction), so two concurrent new-device logins could both pass the
        // check and overshoot by a small margin. That is acceptable for a coarse
        // anti-abuse bound — it still prevents the unbounded growth the cap targets.
        let device_count = devices::Entity::find()
            .filter(devices::Column::UserId.eq(user_id))
            .count(db)
            .await
            .map_err(|e| {
                error!(user_id = %user_id, error = %e, "Failed to count user devices for cap");
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
            })?;
        if device_count >= options.max_devices {
            warn!(
                user_id = %user_id,
                device_count,
                max_devices = options.max_devices,
                "Rejected new-device login that would exceed the per-user device cap",
            );
            return Err(error_response(
                StatusCode::FORBIDDEN,
                "Device limit reached",
            ));
        }
        devices::ActiveModel {
            id: Set(device_id),
            user_id: Set(user_id),
            name: Set(device_name),
            platform: Set(platform),
            signing_public_key: Set(options.device_signing_public_key),
            created_at: Set(now.clone()),
            updated_at: Set(now.clone()),
            last_seen_at: Set(now.clone()),
        }
        .insert(db)
        .await
        .map_err(|e| map_device_insert_error(e, device_id, user_id))?;
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

fn map_device_insert_error(error: sea_orm::DbErr, device_id: Uuid, user_id: Uuid) -> ApiError {
    match error.sql_err() {
        Some(SqlErr::UniqueConstraintViolation(constraint)) => {
            warn!(
                device_id = %device_id,
                user_id = %user_id,
                constraint = %constraint,
                "Concurrent session issue lost a uniqueness race on device id",
            );
            error_response(StatusCode::CONFLICT, "Device id already exists")
        }
        _ => {
            error!(
                device_id = %device_id,
                user_id = %user_id,
                error = %error,
                "Failed to insert device row",
            );
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        }
    }
}

#[derive(Debug)]
struct SessionOptions {
    requested_device_id: Option<clipper_core::models::DeviceId>,
    device_signing_public_key: Vec<u8>,
    device_name: Option<String>,
    platform: Option<String>,
    ip: String,
    user_agent: Option<String>,
    token_bytes: usize,
    max_devices: u64,
    session_proof: SessionProof,
}

#[derive(Debug)]
enum SessionProof {
    Registration,
    Login(DeviceLoginProof),
}

#[derive(Debug)]
struct DeviceLoginProof {
    challenge_id: String,
    challenge: Vec<u8>,
    username: String,
    signature: Option<Vec<u8>>,
}

fn verify_existing_device_login_proof(
    user_id: Uuid,
    device_id: Uuid,
    signing_public_key: &[u8],
    presented_signing_public_key: &[u8],
    proof: &SessionProof,
) -> Result<(), ApiError> {
    let SessionProof::Login(proof) = proof else {
        warn!(
            device_id = %device_id,
            user_id = %user_id,
            "Registration session issue attempted to reuse an existing device id",
        );
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Device proof required",
        ));
    };
    if proof.challenge.len() != crypto::DEVICE_LOGIN_PROOF_CHALLENGE_BYTES {
        warn!(
            device_id = %device_id,
            user_id = %user_id,
            challenge_len = proof.challenge.len(),
            "Stored device proof challenge had an invalid length",
        );
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Invalid device proof",
        ));
    }
    let signature = proof.signature.as_deref().ok_or_else(|| {
        warn!(
            device_id = %device_id,
            user_id = %user_id,
            "Existing device login did not include a proof signature",
        );
        error_response(StatusCode::UNAUTHORIZED, "Device proof required")
    })?;
    let body = DeviceLoginProofBodyV1 {
        version: DEVICE_LOGIN_PROOF_VERSION,
        challenge_id: proof.challenge_id.clone(),
        challenge: proof.challenge.clone(),
        username: proof.username.clone(),
        device_id: device_id.into(),
        device_signing_public_key: presented_signing_public_key.to_vec(),
    };
    crypto::verify_device_login_proof_signature(signing_public_key, &body, signature).map_err(|e| {
        warn!(
            device_id = %device_id,
            user_id = %user_id,
            error = %e,
            "Existing device login presented an invalid proof signature",
        );
        error_response(StatusCode::UNAUTHORIZED, "Invalid device proof")
    })
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
    use sea_orm::{ActiveModelTrait, Database, QuerySelect, Set};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;
    use crate::{
        entity::{access_keys, server_config},
        rate_limit::auth_rate_limit_middleware,
    };

    async fn empty_state() -> (AppState, TempDir) {
        empty_state_with_config(|_| {}).await
    }

    async fn empty_state_with_config(
        apply: impl FnOnce(&mut crate::config::ServerConfig),
    ) -> (AppState, TempDir) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        let mut config = crate::config::ServerConfig::default();
        config.server.data_dir = data_dir.path().to_path_buf();
        apply(&mut config);
        let state = AppState::open_with_db_and_config(
            db,
            config,
            crate::secret::ServerSecrets::test_fixture(),
        )
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
        test_state_with_config(passphrase, |_| {}).await
    }

    async fn test_state_with_config(
        passphrase: &[u8],
        apply: impl FnOnce(&mut crate::config::ServerConfig),
    ) -> (AppState, TempDir) {
        let (state, data_dir) = empty_state_with_config(apply).await;
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

        let wrapped_opaque_password_file = secret_storage::wrap_opaque_password_file(
            state.secrets(),
            user_id,
            &opaque_password_file,
        )
        .expect("wrap opaque_password_file");
        let wrapped_encryption_salt =
            secret_storage::wrap_encryption_salt(state.secrets(), user_id, &encryption_salt)
                .expect("wrap encryption_salt");

        users::ActiveModel {
            id: Set(user_id),
            username: Set("alice".into()),
            opaque_password_file: Set(wrapped_opaque_password_file),
            encryption_salt: Set(wrapped_encryption_salt),
            access_key_hash: Set(access_key_hash),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            storage_bytes: Set(0),
            object_count: Set(0),
        }
        .insert(state.db())
        .await
        .expect("insert config");

        (state, data_dir)
    }

    fn client_ip() -> ClientIp {
        ClientIp(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn auth_route_app(state: AppState) -> Router {
        Router::new()
            .route("/api/auth/register/start", post(register_start))
            .route("/api/auth/register/finish", post(register_finish))
            .route("/api/auth/challenge", post(challenge))
            .route("/api/auth/login", post(login))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                auth_rate_limit_middleware,
            ))
            .with_state(state)
    }

    fn postcard_request<T: serde::Serialize>(path: &str, value: &T) -> Request<Body> {
        postcard_request_from(path, value, IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn postcard_request_from<T: serde::Serialize>(
        path: &str,
        value: &T,
        ip: IpAddr,
    ) -> Request<Body> {
        let mut request = Request::post(path)
            .header(
                header::CONTENT_TYPE,
                clipper_core::models::POSTCARD_CONTENT_TYPE,
            )
            .body(Body::from(
                postcard::to_allocvec(value).expect("serialize request"),
            ))
            .expect("request");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip, 12345)));
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
            device_signing_public_key: test_device_signing_public_key(),
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
    async fn register_start_does_not_reveal_existing_username() {
        let passphrase = b"existing user passphrase";
        let (state, _data_dir) = test_state(passphrase).await;
        let access_key = "second-invite-key-with-entropy";
        let new_passphrase = b"new user private passphrase";
        insert_access_key(&state, access_key).await;

        let (start_req, client_state) =
            registration_start_request(access_key, "alice", new_passphrase);
        let Postcard(start_resp) = register_start(State(state.clone()), validated(start_req))
            .await
            .expect("register start must not reveal that username exists");

        assert!(!start_resp.registration_id.is_empty());
        assert!(!start_resp.user_id.is_empty());
        assert!(!start_resp.registration_response.is_empty());

        let finish_req = registration_finish_request(start_resp, &client_state, new_passphrase);
        let err = register_finish(
            State(state.clone()),
            client_ip(),
            HeaderMap::new(),
            validated(finish_req),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);

        // The one-time invite must NOT be burned by a duplicate-username finish:
        // the legitimate holder keeps it (used_at stays NULL, unbound).
        let key_after_conflict =
            access_keys::Entity::find_by_id(access_key_hash_for_state(&state, access_key).await)
                .one(state.db())
                .await
                .expect("query access key")
                .expect("access key");
        assert!(key_after_conflict.used_at.is_none());
        assert_eq!(key_after_conflict.used_by_user_id, None);

        // ...and the invite is still usable for a fresh username.
        let (retry_start, retry_client_state) =
            registration_start_request(access_key, "bob", new_passphrase);
        let Postcard(retry_resp) = register_start(State(state.clone()), validated(retry_start))
            .await
            .expect("retry register start succeeds with the unburned invite");
        let retry_finish =
            registration_finish_request(retry_resp, &retry_client_state, new_passphrase);
        let Postcard(finish_ok) = register_finish(
            State(state.clone()),
            client_ip(),
            HeaderMap::new(),
            validated(retry_finish),
        )
        .await
        .expect("retry register finish succeeds");
        assert_eq!(finish_ok.username, "bob");

        // Only now is the invite spent and bound to the user that used it.
        let key_after_success =
            access_keys::Entity::find_by_id(access_key_hash_for_state(&state, access_key).await)
                .one(state.db())
                .await
                .expect("query access key")
                .expect("access key");
        assert!(key_after_success.used_at.is_some());
        assert!(key_after_success.used_by_user_id.is_some());
    }

    #[tokio::test]
    async fn register_routes_roundtrip_postcard_auth_blobs() {
        let (state, _data_dir) = empty_state().await;
        let access_key = "invite-key-with-entropy";
        let passphrase = b"user private passphrase";
        insert_access_key(&state, access_key).await;
        let app = auth_route_app(state);

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
            device_signing_public_key: test_device_signing_public_key(),
            device_login_proof_signature: None,
            device_name: Some("test-device".into()),
            platform: Some("test".into()),
        }
    }

    fn login_request_for_device(
        challenge: LoginChallengeResponse,
        client_state: &[u8],
        passphrase: &[u8],
        device_id: Uuid,
        signing_secret_key: [u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES],
        include_proof: bool,
    ) -> LoginRequest {
        let finish = crypto::opaque_client_login_finish(
            client_state,
            passphrase,
            &challenge.credential_response,
        )
        .expect("client login finish");
        let device_id = clipper_core::models::DeviceId::from(device_id);
        let device_signing_public_key =
            crypto::device_signing_public_key(&signing_secret_key).to_vec();
        let device_login_proof_signature = include_proof.then(|| {
            let body = DeviceLoginProofBodyV1 {
                version: DEVICE_LOGIN_PROOF_VERSION,
                challenge_id: challenge.challenge_id.clone(),
                challenge: challenge.device_proof_challenge.clone(),
                username: "alice".into(),
                device_id,
                device_signing_public_key: device_signing_public_key.clone(),
            };
            crypto::sign_device_login_proof_body(&signing_secret_key, &body)
                .expect("sign device login proof")
        });

        LoginRequest {
            challenge_id: challenge.challenge_id,
            credential_finalization: finish.credential_finalization,
            device_id: Some(device_id),
            device_signing_public_key,
            device_login_proof_signature,
            device_name: Some("test-device".into()),
            platform: Some("test".into()),
        }
    }

    fn test_device_signing_secret_key() -> [u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES] {
        [42_u8; crypto::DEVICE_SIGNING_SECRET_KEY_BYTES]
    }

    fn test_device_signing_public_key() -> Vec<u8> {
        crypto::device_signing_public_key(&test_device_signing_secret_key()).to_vec()
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

    // The per-user device cap is the backstop against a credentialed user
    // minting unbounded device rows. It must block only NEW devices: an existing
    // device re-authenticating creates no row and must keep working at the cap.
    #[tokio::test]
    async fn device_cap_blocks_new_devices_but_not_existing_ones() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state_with_config(passphrase, |config| {
            config.limits.max_user_devices = 1;
        })
        .await;

        let device_id = Uuid::now_v7();
        let signing_secret = test_device_signing_secret_key();

        // First login mints the user's one allowed device (new device: the
        // proof is not consulted, so omit it).
        let (challenge_req, client_state) = challenge_request("alice", passphrase);
        let Postcard(login_challenge) = challenge(State(state.clone()), validated(challenge_req))
            .await
            .expect("challenge");
        let req = login_request_for_device(
            login_challenge,
            &client_state,
            passphrase,
            device_id,
            signing_secret,
            false,
        );
        login(
            State(state.clone()),
            client_ip(),
            HeaderMap::new(),
            validated(req),
        )
        .await
        .expect("first device within cap");

        // Re-authenticating that same device reuses its row, so the cap does not
        // apply even though the user already sits at the limit.
        let (challenge_req, client_state) = challenge_request("alice", passphrase);
        let Postcard(login_challenge) = challenge(State(state.clone()), validated(challenge_req))
            .await
            .expect("challenge");
        let req = login_request_for_device(
            login_challenge,
            &client_state,
            passphrase,
            device_id,
            signing_secret,
            true,
        );
        login(
            State(state.clone()),
            client_ip(),
            HeaderMap::new(),
            validated(req),
        )
        .await
        .expect("existing device re-auth is not blocked at cap");

        // A second, distinct device would mint a new row and is rejected.
        let (challenge_req, client_state) = challenge_request("alice", passphrase);
        let Postcard(login_challenge) = challenge(State(state.clone()), validated(challenge_req))
            .await
            .expect("challenge");
        let req = login_request(login_challenge, &client_state, passphrase);
        let err = login(State(state), client_ip(), HeaderMap::new(), validated(req))
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }

    fn auth_info(user_id: Uuid) -> AuthInfo {
        AuthInfo {
            session_id: Uuid::now_v7(),
            user_id,
            device_id: Uuid::now_v7(),
        }
    }

    async fn alice_user_id(state: &AppState) -> Uuid {
        users::Entity::find()
            .select_only()
            .column(users::Column::Id)
            .into_tuple::<Uuid>()
            .one(state.db())
            .await
            .expect("query user id")
            .expect("user id")
    }

    async fn insert_test_device(
        state: &AppState,
        user_id: Uuid,
        name: &str,
        last_seen_at: &str,
    ) -> Uuid {
        let device_id = Uuid::now_v7();
        let now = Utc::now().to_rfc3339();
        devices::ActiveModel {
            id: Set(device_id),
            user_id: Set(user_id),
            name: Set(name.into()),
            platform: Set("test".into()),
            signing_public_key: Set(test_device_signing_public_key()),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            last_seen_at: Set(last_seen_at.into()),
        }
        .insert(state.db())
        .await
        .expect("insert device");
        device_id
    }

    async fn insert_extra_user(state: &AppState) -> Uuid {
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
            username: Set(format!("user-{}", user_id.as_simple())),
            opaque_password_file: Set(vec![1]),
            encryption_salt: Set(vec![2]),
            access_key_hash: Set(access_key_hash),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            storage_bytes: Set(0),
            object_count: Set(0),
        }
        .insert(state.db())
        .await
        .expect("insert user");
        user_id
    }

    #[tokio::test]
    async fn list_devices_returns_user_devices_most_recent_first() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;
        let user_id = alice_user_id(&state).await;

        let older = insert_test_device(&state, user_id, "laptop", "2026-06-10T00:00:00Z").await;
        let newer = insert_test_device(&state, user_id, "phone", "2026-06-13T00:00:00Z").await;

        let Postcard(list) = list_devices(State(state.clone()), Extension(auth_info(user_id)))
            .await
            .expect("list devices");

        assert_eq!(list.devices.len(), 2);
        // Most recently seen first.
        assert_eq!(list.devices[0].id.into_uuid(), newer);
        assert_eq!(list.devices[0].name, "phone");
        assert_eq!(list.devices[1].id.into_uuid(), older);
    }

    #[tokio::test]
    async fn delete_device_removes_row_and_validates_id() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;
        let user_id = alice_user_id(&state).await;
        let device_id = insert_test_device(&state, user_id, "laptop", "2026-06-10T00:00:00Z").await;

        // Deleting an existing device succeeds and removes the row.
        delete_device(
            State(state.clone()),
            Extension(auth_info(user_id)),
            Path(device_id.to_string()),
        )
        .await
        .expect("delete device");
        let remaining = devices::Entity::find()
            .filter(devices::Column::UserId.eq(user_id))
            .all(state.db())
            .await
            .expect("query devices");
        assert!(remaining.is_empty());

        // Deleting an unknown id is a 404, not a silent success.
        let err = delete_device(
            State(state.clone()),
            Extension(auth_info(user_id)),
            Path(Uuid::now_v7().to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);

        // A malformed id is a 400.
        let err = delete_device(
            State(state),
            Extension(auth_info(user_id)),
            Path("not-a-uuid".into()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    // Device endpoints are scoped to the authenticated user. A user must not be
    // able to enumerate or remove another user's devices (IDOR).
    #[tokio::test]
    async fn delete_device_cannot_remove_another_users_device() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;
        let alice = alice_user_id(&state).await;
        let bob = insert_extra_user(&state).await;
        let bob_device =
            insert_test_device(&state, bob, "bob-laptop", "2026-06-10T00:00:00Z").await;

        // Alice cannot delete Bob's device — the user-scoped delete matches no
        // row, so it is a 404 rather than a cross-account deletion.
        let err = delete_device(
            State(state.clone()),
            Extension(auth_info(alice)),
            Path(bob_device.to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);

        // Bob's device is untouched.
        assert!(
            devices::Entity::find_by_id(bob_device)
                .one(state.db())
                .await
                .expect("query device")
                .is_some()
        );

        // And Alice's listing never includes Bob's device.
        let Postcard(list) = list_devices(State(state), Extension(auth_info(alice)))
            .await
            .expect("list");
        assert!(list.devices.is_empty());
    }

    #[tokio::test]
    async fn device_insert_uniqueness_race_returns_conflict() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;
        let user_id = users::Entity::find()
            .select_only()
            .column(users::Column::Id)
            .into_tuple::<Uuid>()
            .one(state.db())
            .await
            .expect("query user id")
            .expect("user id");
        let device_id = Uuid::now_v7();
        let now = Utc::now().to_rfc3339();

        devices::ActiveModel {
            id: Set(device_id),
            user_id: Set(user_id),
            name: Set("existing-device".into()),
            platform: Set("test".into()),
            signing_public_key: Set(test_device_signing_public_key()),
            created_at: Set(now.clone()),
            updated_at: Set(now.clone()),
            last_seen_at: Set(now.clone()),
        }
        .insert(state.db())
        .await
        .expect("insert existing device");

        let err = devices::ActiveModel {
            id: Set(device_id),
            user_id: Set(user_id),
            name: Set("raced-device".into()),
            platform: Set("test".into()),
            signing_public_key: Set(test_device_signing_public_key()),
            created_at: Set(now.clone()),
            updated_at: Set(now.clone()),
            last_seen_at: Set(now),
        }
        .insert(state.db())
        .await
        .expect_err("duplicate device insert should fail");

        assert_eq!(
            map_device_insert_error(err, device_id, user_id).status(),
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn login_existing_device_requires_device_key_proof() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;
        let user_id = users::Entity::find()
            .select_only()
            .column(users::Column::Id)
            .into_tuple::<Uuid>()
            .one(state.db())
            .await
            .expect("query user id")
            .expect("user id");
        let device_id = Uuid::now_v7();
        let now = Utc::now().to_rfc3339();

        devices::ActiveModel {
            id: Set(device_id),
            user_id: Set(user_id),
            name: Set("existing-device".into()),
            platform: Set("test".into()),
            signing_public_key: Set(test_device_signing_public_key()),
            created_at: Set(now.clone()),
            updated_at: Set(now.clone()),
            last_seen_at: Set(now),
        }
        .insert(state.db())
        .await
        .expect("insert existing device");

        let (challenge_req, client_state) = challenge_request("alice", passphrase);
        let Postcard(login_challenge) = challenge(State(state.clone()), validated(challenge_req))
            .await
            .expect("challenge");
        let missing_proof_req = login_request_for_device(
            login_challenge,
            &client_state,
            passphrase,
            device_id,
            test_device_signing_secret_key(),
            false,
        );
        let missing_proof = login(
            State(state.clone()),
            client_ip(),
            HeaderMap::new(),
            validated(missing_proof_req),
        )
        .await;
        assert_eq!(
            missing_proof.unwrap_err().status(),
            StatusCode::UNAUTHORIZED
        );

        let (challenge_req, client_state) = challenge_request("alice", passphrase);
        let Postcard(login_challenge) = challenge(State(state.clone()), validated(challenge_req))
            .await
            .expect("challenge");
        let mut tampered_proof_req = login_request_for_device(
            login_challenge,
            &client_state,
            passphrase,
            device_id,
            test_device_signing_secret_key(),
            true,
        );
        if let Some(byte) = tampered_proof_req
            .device_login_proof_signature
            .as_mut()
            .expect("proof")
            .last_mut()
        {
            *byte ^= 0x01;
        }
        let tampered_proof = login(
            State(state.clone()),
            client_ip(),
            HeaderMap::new(),
            validated(tampered_proof_req),
        )
        .await;
        assert_eq!(
            tampered_proof.unwrap_err().status(),
            StatusCode::UNAUTHORIZED
        );

        let (challenge_req, client_state) = challenge_request("alice", passphrase);
        let Postcard(login_challenge) = challenge(State(state.clone()), validated(challenge_req))
            .await
            .expect("challenge");
        let valid_req = login_request_for_device(
            login_challenge,
            &client_state,
            passphrase,
            device_id,
            test_device_signing_secret_key(),
            true,
        );
        let Postcard(resp) = login(
            State(state),
            client_ip(),
            HeaderMap::new(),
            validated(valid_req),
        )
        .await
        .expect("login");

        assert_eq!(resp.device_id, device_id.to_string());
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
            device_signing_public_key: test_device_signing_public_key(),
            device_login_proof_signature: None,
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
            device_signing_public_key: test_device_signing_public_key(),
            device_login_proof_signature: None,
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
        let app = auth_route_app(state);

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
                        device_signing_public_key: test_device_signing_public_key(),
                        device_login_proof_signature: None,
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
                    device_signing_public_key: test_device_signing_public_key(),
                    device_login_proof_signature: None,
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
        let app = auth_route_app(state);

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

    // Distributed guessing that rotates source addresses stays under every
    // per-client bucket; the per-username budget must still cap it.
    #[tokio::test]
    async fn challenge_rate_limits_by_username_across_client_ips() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;
        let app = auth_route_app(state);
        let quota = crate::config::ServerConfig::default()
            .rate_limit
            .auth_per_username_per_minute;

        for i in 0..quota {
            let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, (i + 1) as u8));
            let (challenge_req, _client_state) =
                challenge_request("alice", b"candidate passphrase");
            let response = app
                .clone()
                .oneshot(postcard_request_from(
                    "/api/auth/challenge",
                    &challenge_req,
                    ip,
                ))
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::OK);
        }

        let (challenge_req, _client_state) = challenge_request("alice", b"candidate passphrase");
        let response = app
            .clone()
            .oneshot(postcard_request_from(
                "/api/auth/challenge",
                &challenge_req,
                IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)),
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        // A different username from a clean address is unaffected.
        let (challenge_req, _client_state) = challenge_request("bob", b"candidate passphrase");
        let response = app
            .oneshot(postcard_request_from(
                "/api/auth/challenge",
                &challenge_req,
                IpAddr::V4(Ipv4Addr::new(198, 51, 100, 2)),
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
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
        let recovered = secret_storage::unwrap_encryption_salt(
            state.secrets(),
            user_id,
            &stored.encryption_salt,
        )
        .expect("unwrap encryption_salt");
        assert!(recovered.is_empty());

        // The per-user OPAQUE blob must not survive in cleartext and must
        // unwrap with the same pepper. (The server-wide setup lives in
        // server_config, covered by init_server_wraps_access_key_hash_salt.)
        assert!(
            secret_storage::unwrap_opaque_password_file(
                state.secrets(),
                user_id,
                &stored.opaque_password_file,
            )
            .is_ok()
        );
        let other_user_id = Uuid::now_v7();
        assert!(
            secret_storage::unwrap_opaque_password_file(
                state.secrets(),
                other_user_id,
                &stored.opaque_password_file,
            )
            .is_err()
        );
        assert!(
            secret_storage::unwrap_encryption_salt(
                state.secrets(),
                other_user_id,
                &stored.encryption_salt,
            )
            .is_err()
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
            secret_storage::unwrap_opaque_password_file(
                &attacker,
                user.id,
                &user.opaque_password_file
            )
            .is_err()
        );
        assert!(
            secret_storage::unwrap_encryption_salt(&attacker, user.id, &user.encryption_salt)
                .is_err()
        );

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
