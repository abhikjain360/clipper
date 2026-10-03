use std::path::Path;

use clipper_daemon_types::ipc_secret_cache::{IpcSecretCache, cached_secret, empty_cache};
use zeroize::Zeroizing;

const IPC_SECRET_BYTES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum IpcSecretError {
    #[error("IPC secret not found; open Clipper to start the daemon")]
    NotFound,
    #[error("IPC secret has wrong length: expected {IPC_SECRET_BYTES}, got {0}")]
    WrongLength(usize),
    #[cfg(target_os = "macos")]
    #[error("keychain read failed: {0}; allow Clipper access to its IPC secret")]
    Keychain(String),
    #[error("IPC secret file I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[error("unsupported platform")]
    UnsupportedPlatform,
}

#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

#[cfg(target_os = "linux")]
const IPC_SECRET_FILE: &str = "ipc-secret-v1";

#[cfg(target_os = "macos")]
fn load_ipc_secret_uncached(data_dir: &Path) -> Result<Zeroizing<Vec<u8>>, IpcSecretError> {
    let names = crate::data_dir::KeychainNames::for_data_dir(data_dir)?;
    match security_framework::passwords::get_generic_password(
        &names.service,
        &names.ipc_secret_account,
    ) {
        Ok(secret) if secret.len() == IPC_SECRET_BYTES => Ok(Zeroizing::new(secret)),
        Ok(secret) => Err(IpcSecretError::WrongLength(secret.len())),
        Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Err(IpcSecretError::NotFound),
        Err(e) => Err(IpcSecretError::Keychain(e.to_string())),
    }
}

#[cfg(target_os = "linux")]
fn load_ipc_secret_uncached(data_dir: &Path) -> Result<Zeroizing<Vec<u8>>, IpcSecretError> {
    let bytes = match std::fs::read(data_dir.join(IPC_SECRET_FILE)) {
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

static IPC_SECRET: IpcSecretCache = empty_cache();

pub fn load_ipc_secret(data_dir: &Path) -> Result<Zeroizing<Vec<u8>>, IpcSecretError> {
    load_with_store(data_dir, &IPC_SECRET, load_ipc_secret_uncached)
}

fn load_with_store(
    data_dir: &Path,
    cache: &IpcSecretCache,
    load: impl FnOnce(&Path) -> Result<Zeroizing<Vec<u8>>, IpcSecretError>,
) -> Result<Zeroizing<Vec<u8>>, IpcSecretError> {
    let canonical = std::fs::canonicalize(data_dir)?;
    cached_secret(cache, &canonical, || {
        tracing::debug!("Reading IPC secret from the platform credential store");
        load(&canonical)
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex};

    use super::*;
    use crate::data_dir::KeychainNames;

    #[test]
    fn ipc_secret_reads_and_cached_reconnects_stay_with_their_data_directory() {
        let root = tempfile::tempdir().unwrap();
        let default = root.path().join("default");
        let qa = root.path().join("qa");
        let mut store = HashMap::new();
        for (path, byte) in [(&default, 7), (&qa, 9)] {
            std::fs::create_dir(path).unwrap();
            let names = KeychainNames::new(path, &default).unwrap();
            store.insert(
                (names.service, names.ipc_secret_account),
                vec![byte; IPC_SECRET_BYTES],
            );
        }
        let reads = Mutex::new(Vec::new());
        let cache = empty_cache();
        let load = |path: &Path| {
            let names = KeychainNames::new(path, &default)?;
            let key = (names.service, names.ipc_secret_account);
            reads.lock().unwrap().push(key.clone());
            Ok(Zeroizing::new(store.get(&key).unwrap().clone()))
        };
        for _ in 0..2 {
            assert_eq!(
                &*load_with_store(&default, &cache, load).unwrap(),
                &[7; IPC_SECRET_BYTES]
            );
            assert_eq!(
                &*load_with_store(&qa, &cache, load).unwrap(),
                &[9; IPC_SECRET_BYTES]
            );
        }
        assert_eq!(reads.lock().unwrap().len(), 2);
        assert_eq!(
            reads.lock().unwrap()[0],
            ("com.clipper.daemon".into(), "ipc-secret-v1".into())
        );
        #[cfg(unix)]
        {
            let alias = root.path().join("alias");
            std::os::unix::fs::symlink(&qa, &alias).unwrap();
            assert_eq!(
                &*load_with_store(&alias, &cache, load).unwrap(),
                &[9; IPC_SECRET_BYTES]
            );
            assert_eq!(reads.lock().unwrap().len(), 2);
        }
    }
}
