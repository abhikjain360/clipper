use axum::{
    Json,
    body::{Body, Bytes},
    extract::FromRequest,
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use clipper_core::models::{ErrorResponse, POSTCARD_CONTENT_TYPE};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

pub mod auth;
pub mod clipboard;
pub mod files;
pub mod health;
pub mod objects;
pub mod sync;

#[derive(Debug)]
pub struct Postcard<T>(pub T);

impl<S, T> FromRequest<S> for Postcard<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = (StatusCode, Json<ErrorResponse>);

    async fn from_request(req: Request<Body>, state: &S) -> Result<Self, Self::Rejection> {
        if !is_postcard_content_type(req.headers().get(header::CONTENT_TYPE)) {
            return Err(error_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Expected postcard body",
            ));
        }

        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid request body"))?;
        let value = postcard::from_bytes(&bytes)
            .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid postcard body"))?;
        Ok(Self(value))
    }
}

fn is_postcard_content_type(value: Option<&header::HeaderValue>) -> bool {
    value
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(POSTCARD_CONTENT_TYPE))
}

impl<T> IntoResponse for Postcard<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        match postcard::to_allocvec(&self.0) {
            Ok(bytes) => ([(header::CONTENT_TYPE, POSTCARD_CONTENT_TYPE)], bytes).into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}

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

pub(crate) fn validate_exact_byte_len(
    value: &[u8],
    expected: usize,
    field_name: &str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if value.len() != expected {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            format!("Invalid {field_name}"),
        ));
    }

    Ok(())
}

pub(crate) fn validate_max_byte_len(
    value: &[u8],
    max: usize,
    message: &str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if value.len() > max {
        return Err(error_response(StatusCode::PAYLOAD_TOO_LARGE, message));
    }

    Ok(())
}
