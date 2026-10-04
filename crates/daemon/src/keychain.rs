//! Platform credential persistence for the daemon.

use std::path::Path;

use clipper_client::engine::{SavedProfile, SessionResumeMaterial};
use clipper_daemon_types::ipc_secret_cache::{IpcSecretCache, cached_secret, empty_cache};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

#[cfg(all(target_os = "macos", not(test)))]
const SERVICE: &str = "com.clipper.daemon";
#[cfg(all(target_os = "macos", not(test)))]
const ACCOUNT: &str = "credentials";
#[cfg(all(target_os = "macos", not(test)))]
const IPC_SECRET_ACCOUNT: &str = "ipc-secret-v1";
const IPC_SECRET_BYTES: usize = 32;
#[cfg(all(target_os = "macos", not(test)))]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
#[cfg(any(target_os = "linux", test))]
const CREDENTIALS_FILE: &str = "credentials.json";
const PROFILE_FILE: &str = "profile.json";
#[cfg(any(target_os = "linux", test))]
const IPC_SECRET_FILE: &str = "ipc-secret-v1";
const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub device_name: String,
    pub server_url: String,
    pub username: String,
    pub session: Option<StoredSession>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSession {
    pub token: Zeroizing<String>,
    pub data_key: Zeroizing<[u8; 32]>,
    pub device_identity_wrapping_key: Zeroizing<[u8; 32]>,
    pub last_confirmed_at: i64,
}

impl From<SessionResumeMaterial> for StoredSession {
    fn from(material: SessionResumeMaterial) -> Self {
        Self {
            token: Zeroizing::new(material.token),
            data_key: material.data_key,
            device_identity_wrapping_key: material.device_identity_wrapping_key,
            last_confirmed_at: material.last_confirmed_at,
        }
    }
}

impl StoredSession {
    pub fn material(&self) -> SessionResumeMaterial {
        SessionResumeMaterial {
            token: self.token.to_string(),
            data_key: self.data_key.clone(),
            device_identity_wrapping_key: self.device_identity_wrapping_key.clone(),
            last_confirmed_at: self.last_confirmed_at,
        }
    }
}

impl Credentials {
    pub fn profile(&self) -> SavedProfile {
        SavedProfile {
            device_name: self.device_name.clone(),
            server_url: self.server_url.clone(),
            username: self.username.clone(),
        }
    }
}

impl From<SavedProfile> for Credentials {
    fn from(profile: SavedProfile) -> Self {
        Self {
            device_name: profile.device_name,
            server_url: profile.server_url,
            username: profile.username,
            session: None,
        }
    }
}

pub type KeychainResult<T> = Result<T, KeychainError>;

#[derive(Debug, thiserror::Error)]
pub enum KeychainError {
    #[error("keychain entry encode failed: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("keychain entry decode failed: {0}")]
    Decode(#[source] serde_json::Error),
    #[cfg(all(target_os = "macos", not(test)))]
    #[error("keychain store failed: {0}")]
    Store(String),
    #[cfg(all(target_os = "macos", not(test)))]
    #[error("keychain read failed: {0}")]
    Read(String),
    #[error("credential store I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(all(target_os = "macos", not(test)))]
pub fn store_credentials(_data_dir: &Path, creds: &Credentials) -> KeychainResult<()> {
    let json = Zeroizing::new(serde_json::to_string(creds).map_err(KeychainError::Encode)?);
    security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, json.as_bytes())
        .map_err(|e| KeychainError::Store(e.to_string()))?;
    Ok(())
}

#[cfg(all(target_os = "macos", not(test)))]
pub fn load_credentials(_data_dir: &Path) -> KeychainResult<Option<Credentials>> {
    match security_framework::passwords::get_generic_password(SERVICE, ACCOUNT) {
        Ok(data) => {
            let data = Zeroizing::new(data);
            let creds = serde_json::from_slice(&data).map_err(KeychainError::Decode)?;
            Ok(Some(creds))
        }
        Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
        Err(e) => Err(KeychainError::Read(e.to_string())),
    }
}

#[cfg(all(target_os = "macos", not(test)))]
pub fn clear_credentials(_data_dir: &Path) -> KeychainResult<()> {
    match security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
        Err(error) => Err(KeychainError::Store(error.to_string())),
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn load_or_create_ipc_secret_uncached(_data_dir: &Path) -> KeychainResult<Zeroizing<Vec<u8>>> {
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

#[cfg(any(target_os = "linux", test))]
pub fn store_credentials(data_dir: &Path, creds: &Credentials) -> KeychainResult<()> {
    let json = Zeroizing::new(serde_json::to_vec(creds).map_err(KeychainError::Encode)?);
    write_private_file(&data_dir.join(CREDENTIALS_FILE), &json)?;
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
pub fn load_credentials(data_dir: &Path) -> KeychainResult<Option<Credentials>> {
    let Some(bytes) = read_optional_file(&data_dir.join(CREDENTIALS_FILE))? else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(bytes);
    let creds = serde_json::from_slice(&bytes).map_err(KeychainError::Decode)?;
    Ok(Some(creds))
}

#[cfg(any(target_os = "linux", test))]
pub fn clear_credentials(data_dir: &Path) -> KeychainResult<()> {
    match std::fs::remove_file(data_dir.join(CREDENTIALS_FILE)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn load_or_create_ipc_secret_uncached(data_dir: &Path) -> KeychainResult<Zeroizing<Vec<u8>>> {
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

pub fn store_profile(data_dir: &Path, profile: &SavedProfile) -> KeychainResult<()> {
    let json = serde_json::to_vec(profile).map_err(KeychainError::Encode)?;
    write_private_file(&data_dir.join(PROFILE_FILE), &json)?;
    Ok(())
}

pub fn load_profile(data_dir: &Path) -> KeychainResult<Option<SavedProfile>> {
    read_optional_file(&data_dir.join(PROFILE_FILE))?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(KeychainError::Decode))
        .transpose()
}

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

fn current_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() as u32 }
}

fn read_optional_file(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    reject_non_regular_existing_file(path)?;
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

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

/// This process's cached copy of the IPC secret. See
/// [`clipper_daemon_types::ipc_secret_cache`] for why it is cached: the daemon
/// authenticates every incoming connection, and on macOS each store read can
/// raise a keychain prompt.
static IPC_SECRET: IpcSecretCache = empty_cache();

/// The shared IPC secret, creating it on first use. Read from the platform
/// store once per process.
pub fn load_or_create_ipc_secret(data_dir: &Path) -> KeychainResult<Zeroizing<Vec<u8>>> {
    cached_secret(&IPC_SECRET, || {
        tracing::debug!("Reading IPC secret from the platform credential store");
        load_or_create_ipc_secret_uncached(data_dir)
    })
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
