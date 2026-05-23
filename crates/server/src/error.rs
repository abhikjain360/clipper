pub type ServerResult<T> = Result<T, ServerError>;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("server I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("crypto error: {0}")]
    Crypto(#[from] clipper_core::crypto::CryptoError),
    #[error("passphrase cannot be empty")]
    EmptyPassphrase,
    #[error("server not initialized; run `clipper-server init` first")]
    NotInitialized,
}

impl ServerError {
    pub fn exit_code(&self) -> i32 {
        match self {
            ServerError::EmptyPassphrase | ServerError::NotInitialized => 2,
            ServerError::Io(_) | ServerError::Database(_) | ServerError::Crypto(_) => 1,
        }
    }
}
