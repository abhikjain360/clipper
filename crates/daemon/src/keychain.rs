//! Platform credential persistence for the daemon.

use std::path::Path;

use rand::RngExt;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

#[cfg(target_os = "macos")]
const SERVICE: &str = "com.clipper.daemon";
#[cfg(target_os = "macos")]
const ACCOUNT: &str = "credentials";
#[cfg(target_os = "macos")]
const IPC_SECRET_ACCOUNT: &str = "ipc-secret-v1";
const IPC_SECRET_BYTES: usize = 32;
#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
#[cfg(target_os = "linux")]
const CREDENTIALS_FILE: &str = "profile.json";
#[cfg(target_os = "linux")]
const IPC_SECRET_FILE: &str = "ipc-secret-v1";
#[cfg(target_os = "linux")]
const PRIVATE_DIR_MODE: u32 = 0o700;
#[cfg(target_os = "linux")]
const PRIVATE_FILE_MODE: u32 = 0o600;

#[derive(Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub device_name: String,
    pub server_url: String,
    pub username: String,
}

pub type KeychainResult<T> = Result<T, KeychainError>;

#[derive(Debug, thiserror::Error)]
pub enum KeychainError {
    #[error("keychain entry encode failed: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("keychain entry decode failed: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("keychain entry is not valid UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[cfg(target_os = "macos")]
    #[error("keychain store failed: {0}")]
    Store(String),
    #[cfg(target_os = "macos")]
    #[error("keychain read failed: {0}")]
    Read(String),
    #[cfg(target_os = "linux")]
    #[error("credential store path is unavailable")]
    DataDirUnavailable,
    #[cfg(target_os = "linux")]
    #[error("credential store I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(target_os = "macos")]
pub fn store_credentials(creds: &Credentials) -> KeychainResult<()> {
    let json = serde_json::to_string(creds).map_err(KeychainError::Encode)?;
    // Delete existing entry first (ignore error if it doesn't exist)
    _ = security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT);
    security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, json.as_bytes())
        .map_err(|e| KeychainError::Store(e.to_string()))?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn load_credentials() -> KeychainResult<Option<Credentials>> {
    match security_framework::passwords::get_generic_password(SERVICE, ACCOUNT) {
        Ok(data) => {
            let json = String::from_utf8(data)?;
            let value: serde_json::Value =
                serde_json::from_str(&json).map_err(KeychainError::Decode)?;
            let had_legacy_passphrase = value.get("passphrase").is_some();
            let creds: Credentials =
                serde_json::from_value(value).map_err(KeychainError::Decode)?;
            if had_legacy_passphrase {
                store_credentials(&creds)?;
            }
            Ok(Some(creds))
        }
        Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
        Err(e) => Err(KeychainError::Read(e.to_string())),
    }
}

#[cfg(target_os = "macos")]
pub fn clear_credentials() -> KeychainResult<()> {
    _ = security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT);
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn load_or_create_ipc_secret(_data_dir: &Path) -> KeychainResult<Zeroizing<Vec<u8>>> {
    match security_framework::passwords::get_generic_password(SERVICE, IPC_SECRET_ACCOUNT) {
        Ok(secret) if secret.len() == IPC_SECRET_BYTES => Ok(Zeroizing::new(secret)),
        Ok(mut secret) => {
            let actual = secret.len();
            secret.zeroize();
            let secret = new_ipc_secret();
            _ = security_framework::passwords::delete_generic_password(SERVICE, IPC_SECRET_ACCOUNT);
            security_framework::passwords::set_generic_password(
                SERVICE,
                IPC_SECRET_ACCOUNT,
                &secret,
            )
            .map_err(|e| KeychainError::Store(e.to_string()))?;
            if actual != 0 {
                tracing::warn!(
                    expected = IPC_SECRET_BYTES,
                    actual,
                    "replaced invalid IPC secret"
                );
            }
            Ok(secret)
        }
        Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => {
            let secret = new_ipc_secret();
            security_framework::passwords::set_generic_password(
                SERVICE,
                IPC_SECRET_ACCOUNT,
                &secret,
            )
            .map_err(|e| KeychainError::Store(e.to_string()))?;
            Ok(secret)
        }
        Err(e) => Err(KeychainError::Read(e.to_string())),
    }
}

#[cfg(target_os = "linux")]
pub fn store_credentials(creds: &Credentials) -> KeychainResult<()> {
    let json = serde_json::to_vec(creds).map_err(KeychainError::Encode)?;
    write_private_file(&credentials_path()?, &json)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn load_credentials() -> KeychainResult<Option<Credentials>> {
    let Some(bytes) = read_optional_file(&credentials_path()?)? else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(KeychainError::Decode)?;
    let had_legacy_passphrase = value.get("passphrase").is_some();
    let creds: Credentials = serde_json::from_value(value).map_err(KeychainError::Decode)?;
    if had_legacy_passphrase {
        store_credentials(&creds)?;
    }
    Ok(Some(creds))
}

#[cfg(target_os = "linux")]
pub fn clear_credentials() -> KeychainResult<()> {
    match std::fs::remove_file(credentials_path()?) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn load_or_create_ipc_secret(data_dir: &Path) -> KeychainResult<Zeroizing<Vec<u8>>> {
    ensure_private_dir(data_dir)?;
    let path = data_dir.join(IPC_SECRET_FILE);

    match read_optional_file(&path)?.map(Zeroizing::new) {
        Some(secret) if secret.len() == IPC_SECRET_BYTES => Ok(secret),
        Some(secret) => {
            let actual = secret.len();
            drop(secret);
            let secret = new_ipc_secret();
            write_private_file(&path, &secret)?;
            if actual != 0 {
                tracing::warn!(
                    expected = IPC_SECRET_BYTES,
                    actual,
                    "replaced invalid IPC secret"
                );
            }
            Ok(secret)
        }
        None => {
            let secret = new_ipc_secret();
            write_private_file(&path, &secret)?;
            Ok(secret)
        }
    }
}

#[cfg(target_os = "linux")]
fn credentials_path() -> KeychainResult<std::path::PathBuf> {
    let dir = dirs::data_dir()
        .map(|base| base.join("Clipper"))
        .ok_or(KeychainError::DataDirUnavailable)?;
    ensure_private_dir(&dir)?;
    Ok(dir.join(CREDENTIALS_FILE))
}

#[cfg(target_os = "linux")]
fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    // Create the leaf with restrictive permissions at creation time so there is
    // no group/other-traversable window between mkdir and the chmod below.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new()
        .mode(PRIVATE_DIR_MODE)
        .create(path)
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }

    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a directory", path.display()),
        ));
    }

    // Fail closed if the directory is owned by another user: a foreign-owned
    // directory must never be adopted to hold the IPC secret or credentials,
    // since chmod changes the mode but not the owner. Mirrors
    // ipc_path::ensure_private_socket_dir.
    let current_uid = current_euid();
    if metadata.uid() != current_uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is owned by uid {}, expected {}",
                path.display(),
                metadata.uid(),
                current_uid
            ),
        ));
    }

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn current_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() as u32 }
}

#[cfg(target_os = "linux")]
fn read_optional_file(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    reject_non_regular_existing_file(path)?;
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::{
        io::Write,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };

    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    reject_non_regular_existing_file(path)?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn reject_non_regular_existing_file(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn new_ipc_secret() -> Zeroizing<Vec<u8>> {
    let mut bytes = random_bytes::<IPC_SECRET_BYTES>();
    let secret = Zeroizing::new(bytes.to_vec());
    bytes.zeroize();
    secret
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    rand::rng().fill(&mut bytes);
    bytes
}
