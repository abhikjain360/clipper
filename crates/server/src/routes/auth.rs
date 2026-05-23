use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
};
use base64::Engine;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use tracing::info;
use uuid::Uuid;

use crate::auth::AuthInfo;
use crate::entity::{device, server_config, session};
use crate::rate_limit::RateLimiter;
use crate::routes::error_response;
use crate::state::AppState;
use clipper_core::crypto;
use clipper_core::models::{
    ErrorResponse, LoginChallengeResponse, LoginRequest, LoginResponse, OkResponse, ServerInfo,
};

pub async fn challenge(
    State(state): State<AppState>,
) -> Result<Json<LoginChallengeResponse>, (StatusCode, Json<ErrorResponse>)> {
    let config = server_config::Entity::find_by_id(1)
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server not initialized")
        })?;

    let (challenge_id, challenge_nonce) = state.create_auth_challenge();
    let b64 = &base64::engine::general_purpose::STANDARD;
    let auth_params = crypto::Argon2Params::default();

    Ok(Json(LoginChallengeResponse {
        challenge_id,
        challenge_nonce_b64: b64.encode(challenge_nonce),
        auth_salt_b64: b64.encode(&config.auth_salt),
        server: ServerInfo {
            enc_salt_b64: b64.encode(&config.enc_salt),
            auth_params,
            enc_params: crypto::Argon2Params::default(),
        },
    }))
}

pub async fn login(
    State(state): State<AppState>,
    Extension(limiter): Extension<std::sync::Arc<RateLimiter>>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Rate limit by IP
    let ip = headers
        .get("cf-connecting-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    if !limiter.check(&ip) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse {
                error: "Too many requests".into(),
            }),
        ));
    }

    // Get server config
    let config = server_config::Entity::find_by_id(1)
        .one(state.db())
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Database error".into(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Server not initialized".into(),
                }),
            )
        })?;

    // Verify challenge proof without receiving the raw passphrase or reusable auth hash.
    let auth_params = crypto::Argon2Params::default();
    let auth_hash: [u8; 32] = config.auth_hash.try_into().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Invalid auth hash".into(),
            }),
        )
    })?;

    let challenge_nonce = state
        .take_auth_challenge(&req.challenge_id)
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Invalid challenge".into(),
                }),
            )
        })?;
    let provided_proof = base64::engine::general_purpose::STANDARD
        .decode(&req.auth_proof_b64)
        .map_err(|_| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Invalid auth proof".into(),
                }),
            )
        })?;
    let expected_proof =
        crypto::compute_login_proof(&auth_hash, &challenge_nonce).map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Auth error".into(),
                }),
            )
        })?;

    if !constant_time_eq::constant_time_eq(&expected_proof, &provided_proof) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Invalid passphrase".into(),
            }),
        ));
    }

    // Create or update device
    let now = Utc::now().to_rfc3339();
    let device_id = req.device_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let device_name = req.device_name.unwrap_or_else(|| "Unknown Device".into());
    let platform = req.platform.unwrap_or_else(|| "unknown".into());

    let existing_device = device::Entity::find_by_id(&device_id)
        .one(state.db())
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Database error".into(),
                }),
            )
        })?;

    if existing_device.is_some() {
        // Update only timestamps — use raw update
        device::Entity::update_many()
            .col_expr(
                device::Column::LastSeenAt,
                sea_orm::sea_query::Expr::value(&now),
            )
            .col_expr(
                device::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(&now),
            )
            .filter(device::Column::Id.eq(&device_id))
            .exec(state.db())
            .await
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "Database error".into(),
                    }),
                )
            })?;
    } else {
        let new_device = device::ActiveModel {
            id: Set(device_id.clone()),
            name: Set(device_name),
            platform: Set(platform),
            created_at: Set(now.clone()),
            updated_at: Set(now.clone()),
            last_seen_at: Set(now.clone()),
        };
        new_device.insert(state.db()).await.map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Database error".into(),
                }),
            )
        })?;
    }

    // Generate session token
    let token_raw = crypto::generate_token();
    let token_hash = crypto::sha256(&token_raw);
    let token_b64 = base64::engine::general_purpose::STANDARD.encode(token_raw);

    let session_id = Uuid::new_v4().to_string();
    let expires_at = (Utc::now() + Duration::days(30)).to_rfc3339();
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let new_session = session::ActiveModel {
        id: Set(session_id),
        token_hash: Set(token_hash.to_vec()),
        device_id: Set(device_id.clone()),
        created_at: Set(now.clone()),
        expires_at: Set(expires_at),
        last_seen_at: Set(now),
        user_agent: Set(user_agent),
        ip_addr: Set(Some(ip)),
    };
    new_session.insert(state.db()).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Database error".into(),
            }),
        )
    })?;

    let enc_salt_b64 = base64::engine::general_purpose::STANDARD.encode(&config.enc_salt);

    info!(device_id = %device_id, "Login successful");

    Ok(Json(LoginResponse {
        token: token_b64,
        device_id,
        server: ServerInfo {
            enc_salt_b64,
            auth_params,
            enc_params: crypto::Argon2Params::default(),
        },
    }))
}

pub async fn logout(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthInfo>,
) -> Result<Json<OkResponse>, StatusCode> {
    session::Entity::delete_by_id(&auth.session_id)
        .exec(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    info!(device_id = %auth.device_id, "Logout");

    Ok(Json(OkResponse { ok: true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Database, Set};
    use sea_orm_migration::MigratorTrait;
    use tempfile::TempDir;

    use crate::entity::server_config;
    use crate::migration;

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    async fn test_state(passphrase: &[u8]) -> (AppState, TempDir) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        migration::Migrator::up(&db, None).await.expect("migrate");

        let params = crypto::Argon2Params::default();
        let auth_salt = crypto::generate_salt();
        let enc_salt = crypto::generate_salt();
        let auth_hash =
            crypto::compute_auth_hash(passphrase, &auth_salt, &params).expect("auth hash");
        let now = Utc::now().to_rfc3339();

        server_config::ActiveModel {
            id: Set(1),
            auth_salt: Set(auth_salt.to_vec()),
            auth_hash: Set(auth_hash.to_vec()),
            enc_salt: Set(enc_salt.to_vec()),
            created_at: Set(now.clone()),
            updated_at: Set(now),
        }
        .insert(&db)
        .await
        .expect("insert config");

        (AppState::new(db, data_dir.path().to_path_buf()), data_dir)
    }

    fn login_request(challenge: LoginChallengeResponse, passphrase: &[u8]) -> LoginRequest {
        let auth_salt = B64.decode(challenge.auth_salt_b64).expect("auth salt");
        let challenge_nonce = B64
            .decode(challenge.challenge_nonce_b64)
            .expect("challenge nonce");
        let auth_hash =
            crypto::compute_auth_hash(passphrase, &auth_salt, &challenge.server.auth_params)
                .expect("auth hash");
        let proof = crypto::compute_login_proof(&auth_hash, &challenge_nonce).expect("proof");

        LoginRequest {
            challenge_id: challenge.challenge_id,
            auth_proof_b64: B64.encode(proof),
            device_id: None,
            device_name: Some("test-device".into()),
            platform: Some("test".into()),
        }
    }

    // This exercises the happy path for challenge/proof login. We test it
    // because clients must prove knowledge of the passphrase-derived auth hash
    // without sending the raw passphrase or a reusable hash to the server.
    #[tokio::test]
    async fn login_accepts_challenge_proof() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;

        let Json(challenge) = challenge(State(state.clone())).await.expect("challenge");
        let req = login_request(challenge, passphrase);

        let Json(resp) = login(
            State(state),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            HeaderMap::new(),
            Json(req),
        )
        .await
        .expect("login");

        assert!(!resp.token.is_empty());
        assert!(!resp.device_id.is_empty());
    }

    // This reuses a captured login proof against the same challenge. We test it
    // because challenge proofs must be single-use; replayable proofs would let a
    // network observer or log leak mint fresh sessions.
    #[tokio::test]
    async fn login_challenge_is_single_use() {
        let passphrase = b"correct horse battery staple";
        let (state, _data_dir) = test_state(passphrase).await;

        let Json(challenge) = challenge(State(state.clone())).await.expect("challenge");
        let req = login_request(challenge, passphrase);
        let reused_req = LoginRequest {
            challenge_id: req.challenge_id.clone(),
            auth_proof_b64: req.auth_proof_b64.clone(),
            device_id: None,
            device_name: Some("test-device".into()),
            platform: Some("test".into()),
        };

        let _ = login(
            State(state.clone()),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            HeaderMap::new(),
            Json(req),
        )
        .await
        .expect("first login");

        let result = login(
            State(state),
            Extension(std::sync::Arc::new(RateLimiter::new())),
            HeaderMap::new(),
            Json(reused_req),
        )
        .await;

        assert_eq!(result.unwrap_err().0, StatusCode::UNAUTHORIZED);
    }
}
