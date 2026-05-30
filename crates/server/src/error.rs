pub type ServerResult<T> = Result<T, ServerError>;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("server I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("crypto error: {0}")]
    Crypto(#[from] clipper_core::crypto::CryptoError),
    #[error("server not initialized; run `clipper-server init` first")]
    NotInitialized,
    #[error(transparent)]
    SecretLoad(#[from] crate::secret::SecretLoadError),
    #[error("configuration error: {0}")]
    Config(String),
}

impl ServerError {
    pub fn exit_code(&self) -> i32 {
        match self {
            ServerError::NotInitialized | ServerError::Config(_) | ServerError::SecretLoad(_) => 2,
            ServerError::Io(_) | ServerError::Database(_) | ServerError::Crypto(_) => 1,
        }
    }
}
