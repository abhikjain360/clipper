use std::path::Path;

use zeroize::Zeroizing;

const IPC_SECRET_BYTES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum IpcSecretError {
    #[error("IPC secret not found")]
    NotFound,
    #[error("IPC secret has wrong length: expected {IPC_SECRET_BYTES}, got {0}")]
    WrongLength(usize),
    #[cfg(target_os = "macos")]
    #[error("keychain read failed: {0}")]
    Keychain(String),
    #[cfg(target_os = "linux")]
    #[error("IPC secret file I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[error("unsupported platform")]
    UnsupportedPlatform,
}

#[cfg(target_os = "macos")]
const SERVICE: &str = "com.clipper.daemon";
#[cfg(target_os = "macos")]
const IPC_SECRET_ACCOUNT: &str = "ipc-secret-v1";
#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

#[cfg(target_os = "linux")]
const IPC_SECRET_FILE: &str = "ipc-secret-v1";

#[cfg(target_os = "macos")]
pub fn load_ipc_secret(_data_dir: &Path) -> Result<Zeroizing<Vec<u8>>, IpcSecretError> {
    match security_framework::passwords::get_generic_password(SERVICE, IPC_SECRET_ACCOUNT) {
        Ok(secret) if secret.len() == IPC_SECRET_BYTES => Ok(Zeroizing::new(secret)),
        Ok(secret) => Err(IpcSecretError::WrongLength(secret.len())),
        Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Err(IpcSecretError::NotFound),
        Err(e) => Err(IpcSecretError::Keychain(e.to_string())),
    }
}

#[cfg(target_os = "linux")]
pub fn load_ipc_secret(data_dir: &Path) -> Result<Zeroizing<Vec<u8>>, IpcSecretError> {
    let path = data_dir.join(IPC_SECRET_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(IpcSecretError::NotFound),
        Err(e) => return Err(IpcSecretError::Io(e)),
    };
    if bytes.len() != IPC_SECRET_BYTES {
        return Err(IpcSecretError::WrongLength(bytes.len()));
    }
    Ok(Zeroizing::new(bytes))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn load_ipc_secret(_data_dir: &Path) -> Result<Zeroizing<Vec<u8>>, IpcSecretError> {
    Err(IpcSecretError::UnsupportedPlatform)
}
