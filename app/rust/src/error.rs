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
