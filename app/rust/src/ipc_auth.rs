//! App-side access to the daemon IPC secret.

const SERVICE: &str = "com.clipper.daemon";
const IPC_SECRET_ACCOUNT: &str = "ipc-secret-v1";
const IPC_SECRET_BYTES: usize = 32;
#[cfg(target_os = "linux")]
const IPC_SECRET_FILE: &str = "ipc-secret-v1";

pub(crate) type IpcSecretResult<T> = Result<T, IpcSecretError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum IpcSecretError {
    #[error("IPC secret read failed: {0}")]
    Read(String),
    #[error("IPC secret has invalid length: expected {expected} bytes, got {actual}")]
    InvalidLength { expected: usize, actual: usize },
    #[cfg(target_os = "linux")]
    #[error("IPC secret path is unavailable")]
    DataDirUnavailable,
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "linux")]
pub(crate) fn load_ipc_secret() -> IpcSecretResult<Vec<u8>> {
    let path = dirs::data_dir()
        .map(|base| base.join("Clipper").join(IPC_SECRET_FILE))
        .ok_or(IpcSecretError::DataDirUnavailable)?;
    let secret = std::fs::read(&path).map_err(|e| IpcSecretError::Read(e.to_string()))?;

    if secret.len() != IPC_SECRET_BYTES {
        return Err(IpcSecretError::InvalidLength {
            expected: IPC_SECRET_BYTES,
            actual: secret.len(),
        });
    }

    Ok(secret)
}
