//! Platform credential persistence for the daemon.

use std::path::{Path, PathBuf};

use clipper_client::engine::{SavedProfile, SessionResumeMaterial};
use clipper_daemon_types::ipc_secret_cache::{IpcSecretCache, cached_secret, empty_cache};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

#[cfg(any(target_os = "macos", test))]
mod session_store;

const IPC_SECRET_BYTES: usize = 32;
#[cfg(all(target_os = "macos", not(test)))]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
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
    pub session_id: Option<[u8; 16]>,
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

    pub fn stored_profile(&self, store: SessionLocation) -> StoredProfile {
        StoredProfile {
            profile: self.profile(),
            signed_out: false,
            resume: self
                .session_id
                .map(|session_id| ResumeRecord { session_id, store }),
        }
    }

    pub fn matches(&self, profile: &StoredProfile, store: SessionLocation) -> bool {
        !profile.signed_out
            && self.session.is_some()
            && self.username == profile.profile.username
            && self.server_url == profile.profile.server_url
            && self.device_name == profile.profile.device_name
            && profile.resume.as_ref().is_some_and(|resume| {
                resume.store == store && Some(resume.session_id) == self.session_id
            })
    }
}

impl From<SavedProfile> for Credentials {
    fn from(profile: SavedProfile) -> Self {
        Self {
            device_name: profile.device_name,
            server_url: profile.server_url,
            username: profile.username,
            session: None,
            session_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLocation {
    Protected,
    Login,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeRecord {
    pub session_id: [u8; 16],
    pub store: SessionLocation,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredProfile {
    #[serde(flatten)]
    pub profile: SavedProfile,
    #[serde(default)]
    pub signed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeRecord>,
}

impl From<SavedProfile> for StoredProfile {
    fn from(profile: SavedProfile) -> Self {
        Self {
            profile,
            signed_out: false,
            resume: None,
        }
    }
}

pub trait CredentialStore: Send + Sync {
    fn supports_resume(&self) -> bool;
    fn load(&self, profile: &StoredProfile) -> KeychainResult<Option<Credentials>>;
    fn store(&self, credentials: &Credentials) -> KeychainResult<SessionLocation>;
    fn clear(&self) -> KeychainResult<()>;
}

pub struct PlatformStore {
    data_dir: PathBuf,
}

impl PlatformStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}

impl CredentialStore for PlatformStore {
    fn supports_resume(&self) -> bool {
        cfg!(all(target_os = "macos", not(test)))
    }

    fn load(&self, profile: &StoredProfile) -> KeychainResult<Option<Credentials>> {
        load_credentials(&self.data_dir, profile)
    }

    fn store(&self, credentials: &Credentials) -> KeychainResult<SessionLocation> {
        store_credentials(&self.data_dir, credentials)
    }

    fn clear(&self) -> KeychainResult<()> {
        clear_credentials(&self.data_dir)
    }
}

pub type KeychainResult<T> = Result<T, KeychainError>;

#[derive(Debug, thiserror::Error)]
pub enum KeychainError {
    #[cfg(any(target_os = "linux", test))]
    #[error("session resume requires a platform secret store")]
    ResumeUnavailable,
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
    #[cfg(any(target_os = "macos", test))]
    #[error("keychain operation failed ({status}): {message}")]
    Platform { status: i32, message: String },
    #[error("credential store I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(all(target_os = "macos", not(test)))]
fn store_credentials(data_dir: &Path, creds: &Credentials) -> KeychainResult<SessionLocation> {
    let names = clipper_daemon_client::data_dir::KeychainNames::for_data_dir(data_dir)?;
    session_store::store(
        &session_store::MacKeychain::Protected,
        &session_store::MacKeychain::Login,
        &names,
        creds,
    )
}

#[cfg(all(target_os = "macos", not(test)))]
fn load_credentials(
    data_dir: &Path,
    profile: &StoredProfile,
) -> KeychainResult<Option<Credentials>> {
    let names = clipper_daemon_client::data_dir::KeychainNames::for_data_dir(data_dir)?;
    session_store::load(
        &session_store::MacKeychain::Protected,
        &session_store::MacKeychain::Login,
        &names,
        profile,
    )
}

#[cfg(all(target_os = "macos", not(test)))]
fn clear_credentials(data_dir: &Path) -> KeychainResult<()> {
    let names = clipper_daemon_client::data_dir::KeychainNames::for_data_dir(data_dir)?;
    session_store::clear(
        &session_store::MacKeychain::Protected,
        &session_store::MacKeychain::Login,
        &names,
    )
}

#[cfg(all(target_os = "macos", not(test)))]
fn load_or_create_ipc_secret_uncached(data_dir: &Path) -> KeychainResult<Zeroizing<Vec<u8>>> {
    let names = clipper_daemon_client::data_dir::KeychainNames::for_data_dir(data_dir)?;
    match security_framework::passwords::get_generic_password(
        &names.service,
        &names.ipc_secret_account,
    ) {
        Ok(secret) if secret.len() == IPC_SECRET_BYTES => Ok(Zeroizing::new(secret)),
        Ok(mut secret) => {
            let actual = secret.len();
            secret.zeroize();
            let secret = new_ipc_secret();
            _ = security_framework::passwords::delete_generic_password(
                &names.service,
                &names.ipc_secret_account,
            );
            security_framework::passwords::set_generic_password(
                &names.service,
                &names.ipc_secret_account,
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
                &names.service,
                &names.ipc_secret_account,
                &secret,
            )
            .map_err(|e| KeychainError::Store(e.to_string()))?;
            Ok(secret)
        }
        Err(e) => Err(KeychainError::Read(e.to_string())),
    }
}

#[cfg(any(target_os = "linux", test))]
fn store_credentials(_data_dir: &Path, _creds: &Credentials) -> KeychainResult<SessionLocation> {
    Err(KeychainError::ResumeUnavailable)
}

#[cfg(any(target_os = "linux", test))]
fn load_credentials(
    _data_dir: &Path,
    _profile: &StoredProfile,
) -> KeychainResult<Option<Credentials>> {
    Ok(None)
}

#[cfg(any(target_os = "linux", test))]
fn clear_credentials(_data_dir: &Path) -> KeychainResult<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn remove_plaintext_credentials(data_dir: &Path) -> KeychainResult<()> {
    let path = data_dir.join("credentials.json");
    reject_non_regular_existing_file(&path)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
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

pub fn store_profile(data_dir: &Path, profile: &StoredProfile) -> KeychainResult<()> {
    let json = serde_json::to_vec(profile).map_err(KeychainError::Encode)?;
    write_private_file(&data_dir.join(PROFILE_FILE), &json)?;
    Ok(())
}

pub fn load_profile(data_dir: &Path) -> KeychainResult<Option<StoredProfile>> {
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
    use std::{io::Write, os::unix::fs::PermissionsExt};

    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    reject_non_regular_existing_file(path)?;

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "file has no parent")
    })?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    std::fs::File::open(parent)?.sync_all()?;
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

pub fn load_or_create_ipc_secret(data_dir: &Path) -> KeychainResult<Zeroizing<Vec<u8>>> {
    ensure_private_dir(data_dir)?;
    let canonical = std::fs::canonicalize(data_dir)?;
    cached_secret(&IPC_SECRET, &canonical, || {
        tracing::debug!("Reading IPC secret from the platform credential store");
        load_or_create_ipc_secret_uncached(&canonical)
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

pub fn new_session_id() -> [u8; 16] {
    random_bytes()
}

#[cfg(test)]
#[derive(Default)]
pub struct TestStore {
    pub credentials: std::sync::Mutex<Option<Credentials>>,
    pub fail_reads: std::sync::atomic::AtomicBool,
    pub fail_writes: std::sync::atomic::AtomicBool,
    pub fail_deletes: std::sync::atomic::AtomicBool,
    pub writes: std::sync::atomic::AtomicUsize,
    pub reads: std::sync::atomic::AtomicUsize,
    pub deletes: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl CredentialStore for TestStore {
    fn supports_resume(&self) -> bool {
        true
    }

    fn load(&self, profile: &StoredProfile) -> KeychainResult<Option<Credentials>> {
        let credentials = self.load()?;
        if credentials
            .as_ref()
            .is_some_and(|credentials| !credentials.matches(profile, SessionLocation::Test))
        {
            let _ = self.clear();
            return Ok(None);
        }
        Ok(credentials)
    }

    fn store(&self, credentials: &Credentials) -> KeychainResult<SessionLocation> {
        self.store(credentials)?;
        Ok(SessionLocation::Test)
    }

    fn clear(&self) -> KeychainResult<()> {
        self.deletes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_deletes.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "test credential deletion denied",
            )
            .into());
        }
        *self.credentials.lock().unwrap() = None;
        Ok(())
    }
}

#[cfg(test)]
impl TestStore {
    pub fn load(&self) -> KeychainResult<Option<Credentials>> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_reads.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "test credential read denied",
            )
            .into());
        }
        Ok(self.credentials.lock().unwrap().clone())
    }

    pub fn store(&self, credentials: &Credentials) -> KeychainResult<()> {
        self.writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_writes.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "test credential write denied",
            )
            .into());
        }
        *self.credentials.lock().unwrap() = Some(credentials.clone());
        Ok(())
    }
}
