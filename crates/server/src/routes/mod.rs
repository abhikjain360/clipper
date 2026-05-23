use axum::{Json, http::StatusCode};
use clipper_core::models::ErrorResponse;
use uuid::Uuid;

pub mod auth;
pub mod clipboard;
pub mod files;
pub mod health;
pub mod sync;

pub(crate) fn error_response(
    status: StatusCode,
    message: impl Into<String>,
) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
}

pub(crate) fn validate_client_id(id: &str) -> Result<Uuid, (StatusCode, Json<ErrorResponse>)> {
    if id.len() != 36 {
        return Err(error_response(StatusCode::BAD_REQUEST, "Invalid id"));
    }

    Uuid::parse_str(id).map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid id"))
}
