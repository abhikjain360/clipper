use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use base64::Engine;
use chrono::Utc;
use clipper_core::crypto::{self, sha256};
use sea_orm::{ColumnTrait, DerivePartialModel, EntityTrait, QueryFilter};
use tracing::{debug, error};
use uuid::Uuid;

use crate::{
    entity::sessions,
    routes::{ApiError, error_response},
    state::AppState,
};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub fn hash_access_key(
    access_key: &str,
    salt: &[u8],
    secret: &[u8],
    params: &crypto::Argon2Params,
) -> Result<String, clipper_core::crypto::CryptoError> {
    Ok(B64.encode(crypto::access_key_hash_with_params(
        access_key.as_bytes(),
        salt,
        Some(secret),
        params,
    )?))
}

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "sessions::Entity", from_query_result)]
struct SessionAuthRow {
    id: Uuid,
    user_id: Uuid,
    device_id: Uuid,
    expires_at: String,
}

/// Extract bearer token from Authorization header.
fn extract_bearer(req: &Request) -> Option<String> {
    let header = req.headers().get("authorization")?.to_str().ok()?;
    let token = header.strip_prefix("Bearer ")?;
    Some(token.to_string())
}

/// Auth middleware — validates session token and injects device_id as extension.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = extract_bearer(&req).ok_or_else(|| {
        debug!("Rejected authenticated request without bearer token");
        error_response(StatusCode::UNAUTHORIZED, "Unauthorized")
    })?;
    let token_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &token)
        .map_err(|_| {
            debug!("Rejected authenticated request with invalid bearer token encoding");
            error_response(StatusCode::UNAUTHORIZED, "Unauthorized")
        })?;

    let token_hash = sha256(&token_bytes);
    let now = Utc::now().to_rfc3339();

    let sess = sessions::Entity::find()
        .filter(sessions::Column::TokenHash.eq(token_hash.to_vec()))
        .into_partial_model::<SessionAuthRow>()
        .one(state.db())
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to look up session in auth middleware");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        })?
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "Unauthorized"))?;

    // Check expiry
    if sess.expires_at < now {
        return Err(error_response(StatusCode::UNAUTHORIZED, "Unauthorized"));
    }

    // Update last_seen_at
    let _ = sessions::Entity::update_many()
        .col_expr(
            sessions::Column::LastSeenAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(sessions::Column::Id.eq(sess.id))
        .exec(state.db())
        .await;

    // Inject device_id and session_id into request extensions
    req.extensions_mut().insert(AuthInfo {
        session_id: sess.id,
        user_id: sess.user_id,
        device_id: sess.device_id,
    });

    Ok(next.run(req).await)
}

#[derive(Clone, Debug)]
pub struct AuthInfo {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub device_id: Uuid,
}
