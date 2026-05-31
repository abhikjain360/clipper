pub type DaemonResult<T> = Result<T, DaemonError>;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("daemon I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("client error: {0}")]
    Client(#[from] clipper_client::api_client::ClientError),
    #[error("keychain error: {0}")]
    Keychain(#[from] crate::keychain::KeychainError),
    #[error("daemon data directory is unavailable")]
    DataDirUnavailable,
}

impl DaemonError {
    pub fn exit_code(&self) -> i32 {
        match self {
            DaemonError::Io(_)
            | DaemonError::Client(_)
            | DaemonError::Keychain(_)
            | DaemonError::DataDirUnavailable => 1,
        }
    }
}
