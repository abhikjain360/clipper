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
use crate::state::AppState;
use clipper_core::crypto;
use clipper_core::models::{ErrorResponse, LoginRequest, LoginResponse, OkResponse, ServerInfo};

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

    // Verify passphrase
    let auth_params = crypto::Argon2Params::default();
    let auth_hash: [u8; 32] = config.auth_hash.try_into().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Invalid auth hash".into(),
            }),
        )
    })?;

    let valid = crypto::verify_auth(
        req.passphrase.as_bytes(),
        &config.auth_salt,
        &auth_params,
        &auth_hash,
    )
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Auth error".into(),
            }),
        )
    })?;

    if !valid {
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
