use axum::{
    Json,
    body::{Body, Bytes},
    extract::FromRequest,
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use clipper_core::models::{ApiErrorCode, ErrorResponse, POSTCARD_CONTENT_TYPE};
use garde::Validate;
use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, error, trace};
use uuid::Uuid;

pub mod auth;
pub mod health;
pub mod objects;
pub mod sync;

#[derive(Debug)]
pub struct Postcard<T>(pub T);

pub(crate) type RouteResult<T> = Result<T, ApiError>;

#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    body: ErrorResponse,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorResponse::new(code, message),
        }
    }

    pub(crate) fn from_code(status: StatusCode, code: ApiErrorCode) -> Self {
        Self::new(status, code, code.default_message())
    }

    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }

    pub(crate) fn body(&self) -> &ErrorResponse {
        &self.body
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl<S, T> FromRequest<S> for Postcard<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Validate,
    T::Context: Default,
{
    type Rejection = ApiError;

    async fn from_request(req: Request<Body>, state: &S) -> Result<Self, Self::Rejection> {
        let method = req.method().clone();
        let uri = req.uri().clone();
        let content_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let type_name = std::any::type_name::<T>();

        if !is_postcard_content_type(req.headers().get(header::CONTENT_TYPE)) {
            debug!(
                method = %method,
                uri = %uri,
                content_type = content_type.as_deref().unwrap_or("<missing>"),
                expected = POSTCARD_CONTENT_TYPE,
                type_name,
                "Rejected postcard request with unexpected content type",
            );
            return Err(error_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Expected postcard body",
            ));
        }

        let bytes = Bytes::from_request(req, state).await.map_err(|e| {
            debug!(
                method = %method,
                uri = %uri,
                type_name,
                error = %e,
                "Failed to read postcard request body",
            );
            error_response(StatusCode::BAD_REQUEST, "Invalid request body")
        })?;
        let value = postcard::from_bytes(&bytes).map_err(|e| {
            debug!(
                method = %method,
                uri = %uri,
                type_name,
                bytes = bytes.len(),
                error = %e,
                "Failed to decode postcard body",
            );
            error_response(StatusCode::BAD_REQUEST, "Invalid postcard body")
        })?;
        if let Err(err) = validate_request(&value) {
            debug!(
                method = %method,
                uri = %uri,
                type_name,
                bytes = bytes.len(),
                status = %err.status(),
                error_code = %err.body().code,
                error = %err.body().message,
                "Rejected invalid postcard request",
            );
            return Err(err);
        }
        trace!(
            method = %method,
            uri = %uri,
            type_name,
            bytes = bytes.len(),
            "Decoded postcard request",
        );
        Ok(Self(value))
    }
}

impl<T> Postcard<T>
where
    T: Validate,
    T::Context: Default,
{
    #[cfg(test)]
    pub(crate) fn validated(value: T) -> RouteResult<Self> {
        validate_request(&value)?;
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
            Ok(bytes) => {
                trace!(
                    type_name = std::any::type_name::<T>(),
                    bytes = bytes.len(),
                    "Serialized postcard response",
                );
                ([(header::CONTENT_TYPE, POSTCARD_CONTENT_TYPE)], bytes).into_response()
            }
            Err(e) => {
                error!(
                    type_name = std::any::type_name::<T>(),
                    error = %e,
                    "Failed to serialize postcard response",
                );
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

pub(crate) fn error_response(status: StatusCode, message: impl Into<String>) -> ApiError {
    ApiError::new(
        status,
        ApiErrorCode::from_http_status(status.as_u16()),
        message,
    )
}

fn validate_request<T>(value: &T) -> RouteResult<()>
where
    T: Validate,
    T::Context: Default,
{
    value.validate().map_err(|report| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            ApiErrorCode::ValidationFailed,
            report.to_string(),
        )
    })
}

pub(crate) fn validate_client_id(id: &str) -> RouteResult<Uuid> {
    if id.len() != 36 {
        return Err(ApiError::from_code(
            StatusCode::BAD_REQUEST,
            ApiErrorCode::InvalidId,
        ));
    }

    Uuid::parse_str(id)
        .map_err(|_| ApiError::from_code(StatusCode::BAD_REQUEST, ApiErrorCode::InvalidId))
}

pub(crate) fn validate_max_byte_len(value: &[u8], max: usize, message: &str) -> RouteResult<()> {
    if value.len() > max {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ApiErrorCode::PayloadTooLarge,
            message,
        ));
    }

    Ok(())
}
