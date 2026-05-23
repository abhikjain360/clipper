use axum::{
    Json,
    extract::{ConnectInfo, Extension, State},
    http::{HeaderMap, StatusCode},
};
use base64::Engine;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::net::SocketAddr;
use tracing::info;
use uuid::Uuid;

use crate::auth::AuthInfo;
use crate::entity::{devices, server_config, sessions};
use crate::rate_limit::RateLimiter;
use crate::routes::{error_response, validate_client_id};
use crate::state::AppState;
use clipper_core::crypto;
use clipper_core::models::{
    ErrorResponse, LoginChallengeRequest, LoginChallengeResponse, LoginRequest, LoginResponse,
    OkResponse, ServerInfo,
};

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

    let config = server_config::Entity::find_by_id(1)
        .one(state.db())
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error"))?
        .ok_or_else(|| {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server not initialized")
        })?;

    let b64 = &base64::engine::general_purpose::STANDARD;
    let credential_request = b64
        .decode(&req.credential_request_b64)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid credential_request_b64"))?;
    let (credential_response, server_login_state) = crypto::opaque_server_login_start(
        &config.auth_salt,
        &config.auth_hash,
        &credential_request,
    )
    .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid login request"))?;
    let challenge_id = state.create_auth_challenge(server_login_state);
    let auth_params = crypto::Argon2Params::default();

    Ok(Json(LoginChallengeResponse {
        challenge_id,
        credential_response_b64: b64.encode(credential_response),
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

    // Finish the OPAQUE login without receiving the raw passphrase or a DB-reusable secret.
    let auth_params = crypto::Argon2Params::default();
    let server_login_state = state
        .take_auth_challenge(&req.challenge_id)
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Invalid challenge".into(),
                }),
            )
        })?;
    let credential_finalization = base64::engine::general_purpose::STANDARD
        .decode(&req.credential_finalization_b64)
        .map_err(|_| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Invalid credential finalization".into(),
                }),
            )
        })?;

    crypto::opaque_server_login_finish(&server_login_state, &credential_finalization).map_err(
        |_| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Invalid passphrase".into(),
                }),
            )
        },
    )?;

    // Create or update device
    let now = Utc::now().to_rfc3339();
    let device_id = match req.device_id {
        Some(id) => validate_client_id(&id)?,
        None => Uuid::new_v4(),
    };
    let device_id_for_response = device_id.to_string();
    let device_name = req.device_name.unwrap_or_else(|| "Unknown Device".into());
    let platform = req.platform.unwrap_or_else(|| "unknown".into());

    let existing_device = devices::Entity::find_by_id(device_id)
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
        let new_device = devices::ActiveModel {
            id: Set(device_id),
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

    let session_id = Uuid::new_v4();
    let expires_at = (Utc::now() + Duration::days(30)).to_rfc3339();
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let new_session = sessions::ActiveModel {
        id: Set(session_id),
        token_hash: Set(token_hash.to_vec()),
        device_id: Set(device_id),
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
        device_id: device_id_for_response,
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
    sessions::Entity::delete_by_id(auth.session_id)
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
    use std::net::{IpAddr, Ipv4Addr};
    use tempfile::TempDir;

    use crate::entity::server_config;
    use crate::migration;

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    async fn test_state(passphrase: &[u8]) -> (AppState, TempDir) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Database::connect("sqlite::memory:").await.expect("db");
        migration::Migrator::up(&db, None).await.expect("migrate");

        let (opaque_server_setup, opaque_password_file) =
            crypto::opaque_register(passphrase).expect("opaque register");
        let enc_salt = crypto::generate_salt();
        let now = Utc::now().to_rfc3339();

        server_config::ActiveModel {
            id: Set(1),
            auth_salt: Set(opaque_server_setup),
            auth_hash: Set(opaque_password_file),
            enc_salt: Set(enc_salt.to_vec()),
            created_at: Set(now.clone()),
            updated_at: Set(now),
        }
        .insert(&db)
        .await
        .expect("insert config");

        (AppState::new(db, data_dir.path().to_path_buf()), data_dir)
    }

    fn peer() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345))
    }

    fn challenge_request(passphrase: &[u8]) -> (LoginChallengeRequest, Vec<u8>) {
        let (credential_request, client_state) =
            crypto::opaque_client_login_start(passphrase).expect("client login start");
        (
            LoginChallengeRequest {
                credential_request_b64: B64.encode(credential_request),
            },
            client_state,
        )
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
