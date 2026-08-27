use std::path::Path;

use clipper_daemon_types::ipc_secret_cache::{IpcSecretCache, cached_secret, empty_cache};
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
fn load_ipc_secret_uncached(_data_dir: &Path) -> Result<Zeroizing<Vec<u8>>, IpcSecretError> {
    match security_framework::passwords::get_generic_password(SERVICE, IPC_SECRET_ACCOUNT) {
        Ok(secret) if secret.len() == IPC_SECRET_BYTES => Ok(Zeroizing::new(secret)),
        Ok(secret) => Err(IpcSecretError::WrongLength(secret.len())),
        Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Err(IpcSecretError::NotFound),
        Err(e) => Err(IpcSecretError::Keychain(e.to_string())),
    }
}

#[cfg(target_os = "linux")]
fn load_ipc_secret_uncached(data_dir: &Path) -> Result<Zeroizing<Vec<u8>>, IpcSecretError> {
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
fn load_ipc_secret_uncached(_data_dir: &Path) -> Result<Zeroizing<Vec<u8>>, IpcSecretError> {
    Err(IpcSecretError::UnsupportedPlatform)
}

/// This process's cached copy of the IPC secret. See
/// [`clipper_daemon_types::ipc_secret_cache`] for why it is cached:
/// `daemon_client::connection_loop` retries the handshake on a backoff, and the
/// app dials before the daemon it just spawned has bound its socket, so at least
/// one retry — and so at least one extra store read — happens on every launch.
static IPC_SECRET: IpcSecretCache = empty_cache();

/// The shared IPC secret used to authenticate to the daemon. Read from the
/// platform store once per process; never created here — the daemon owns
/// creation.
pub fn load_ipc_secret(data_dir: &Path) -> Result<Zeroizing<Vec<u8>>, IpcSecretError> {
    cached_secret(&IPC_SECRET, || {
        tracing::debug!("Reading IPC secret from the platform credential store");
        load_ipc_secret_uncached(data_dir)
    })
}
