use crate::runtime::RuntimeError;

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct AppError(#[from] AppErrorKind);

#[derive(Debug, thiserror::Error)]
pub(crate) enum AppErrorKind {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("daemon response missing {field}")]
    MissingDaemonResult { field: &'static str },
    #[error("daemon response decode failed: {0}")]
    ResponseDecode(#[from] serde_json::Error),
}

impl AppError {
    pub(crate) fn missing_daemon_result(field: &'static str) -> Self {
        AppErrorKind::MissingDaemonResult { field }.into()
    }

    pub(crate) fn into_error_response(self) -> clipper_daemon_types::ErrorResponse {
        match self.0 {
            AppErrorKind::Runtime(error) => error.error_response(),
            AppErrorKind::MissingDaemonResult { field } => {
                clipper_daemon_types::ErrorResponse::new(
                    clipper_daemon_types::ApiErrorCode::Unknown,
                    format!("daemon response missing {field}"),
                )
            }
            AppErrorKind::ResponseDecode(error) => clipper_daemon_types::ErrorResponse::new(
                clipper_daemon_types::ApiErrorCode::Unknown,
                format!("daemon response decode failed: {error}"),
            ),
        }
    }
}

impl From<RuntimeError> for AppError {
    fn from(error: RuntimeError) -> Self {
        AppErrorKind::Runtime(error).into()
    }
}

impl From<serde_json::Error> for AppError {
    fn from(error: serde_json::Error) -> Self {
        AppErrorKind::ResponseDecode(error).into()
    }
}
