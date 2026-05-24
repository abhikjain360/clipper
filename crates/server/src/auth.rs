use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

use crate::entity::sessions;
use crate::state::AppState;
use clipper_core::crypto::sha256;

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
) -> Result<Response, StatusCode> {
    let token = extract_bearer(&req).ok_or(StatusCode::UNAUTHORIZED)?;
    let token_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &token)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    let token_hash = sha256(&token_bytes);
    let now = Utc::now().to_rfc3339();

    let sess = sessions::Entity::find()
        .filter(sessions::Column::TokenHash.eq(token_hash.to_vec()))
        .one(state.db())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // Check expiry
    if sess.expires_at < now {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Update last_seen_at
    let mut active: sessions::ActiveModel = sess.clone().into();
    active.last_seen_at = Set(now);
    let _ = active.update(state.db()).await;

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
