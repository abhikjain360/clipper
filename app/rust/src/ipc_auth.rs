//! App-side access to the daemon IPC secret.

const SERVICE: &str = "com.clipper.daemon";
const IPC_SECRET_ACCOUNT: &str = "ipc-secret-v1";
const IPC_SECRET_BYTES: usize = 32;

pub(crate) type IpcSecretResult<T> = Result<T, IpcSecretError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum IpcSecretError {
    #[error("IPC secret read failed: {0}")]
    Read(String),
    #[error("IPC secret has invalid length: expected {expected} bytes, got {actual}")]
    InvalidLength { expected: usize, actual: usize },
}

pub(crate) fn load_ipc_secret() -> IpcSecretResult<Vec<u8>> {
    let secret = security_framework::passwords::get_generic_password(SERVICE, IPC_SECRET_ACCOUNT)
        .map_err(|e| IpcSecretError::Read(e.to_string()))?;

    if secret.len() != IPC_SECRET_BYTES {
        return Err(IpcSecretError::InvalidLength {
            expected: IPC_SECRET_BYTES,
            actual: secret.len(),
        });
    }

    Ok(secret)
}
