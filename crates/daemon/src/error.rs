pub type DaemonResult<T> = Result<T, DaemonError>;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("daemon I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("keychain error: {0}")]
    Keychain(#[from] crate::keychain::KeychainError),
}

impl DaemonError {
    pub fn exit_code(&self) -> i32 {
        match self {
            DaemonError::Io(_) | DaemonError::Keychain(_) => 1,
        }
    }
}
