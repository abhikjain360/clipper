use axum::{
    Json,
    body::{Body, Bytes},
    extract::FromRequest,
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use clipper_core::models::{ApiErrorCode, ErrorResponse, POSTCARD_CONTENT_TYPE};
use garde::Validate;
use sea_orm::{DatabaseConnection, DatabaseTransaction, DbErr, TransactionTrait};
use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, error, trace, warn};

pub mod auth;
pub mod health;
pub mod objects;

#[derive(Debug)]
pub struct Postcard<T>(pub T);

pub(crate) type RouteResult<T> = Result<T, ApiError>;

#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    body: ErrorResponse,
}

impl ApiError {
    pub(crate) fn from_code(code: ApiErrorCode) -> Self {
        Self::from_code_with_message(code, code.default_message())
    }

    pub(crate) fn from_code_with_message(code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::from_u16(code.http_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            body: ErrorResponse::new(code, message),
        }
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

        let is_postcard_content_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case(POSTCARD_CONTENT_TYPE));

        if !is_postcard_content_type {
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
    ApiError::from_code_with_message(ApiErrorCode::from_http_status(status.as_u16()), message)
}

/// Run `f` inside a database transaction, committing on `Ok` and rolling back
/// on `Err`. `operation` labels the transaction in failure logs.
///
/// Begin and commit failures are logged and mapped to a generic database
/// `ApiError`; every other error is whatever `f` returns. The rollback is
/// awaited and logged on failure, so callers never roll back by hand — an early
/// return inside `f` rolls back automatically.
pub(crate) async fn with_txn<F, T>(
    db: &DatabaseConnection,
    operation: &str,
    f: F,
) -> Result<T, ApiError>
where
    F: AsyncFnOnce(&DatabaseTransaction) -> Result<T, ApiError>,
{
    let txn = db
        .begin()
        .await
        .map_err(|e| txn_db_error(operation, "begin", e))?;
    match f(&txn).await {
        Ok(value) => {
            txn.commit()
                .await
                .map_err(|e| txn_db_error(operation, "commit", e))?;
            Ok(value)
        }
        Err(err) => {
            if let Err(rollback_err) = txn.rollback().await {
                warn!(operation, error = %rollback_err, "Failed to roll back transaction");
            }
            Err(err)
        }
    }
}

fn txn_db_error(operation: &str, phase: &str, error: DbErr) -> ApiError {
    error!(operation, phase, error = %error, "Database transaction error");
    ApiError::from_code_with_message(ApiErrorCode::Database, "Database error")
}

fn validate_request<T>(value: &T) -> RouteResult<()>
where
    T: Validate,
    T::Context: Default,
{
    value.validate().map_err(|report| {
        ApiError::from_code_with_message(ApiErrorCode::ValidationFailed, report.to_string())
    })
}
