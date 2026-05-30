//! macOS Keychain integration for credential persistence.

use std::path::Path;

use rand::RngExt;
use serde::{Deserialize, Serialize};

const SERVICE: &str = "com.clipper.daemon";
const ACCOUNT: &str = "credentials";
const IPC_SECRET_ACCOUNT: &str = "ipc-secret-v1";
const IPC_SECRET_BYTES: usize = 32;

#[derive(Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub device_name: String,
    pub server_url: String,
    #[serde(default)]
    pub user_id: Option<String>,
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
    #[error("keychain store failed: {0}")]
    Store(String),
    #[error("keychain read failed: {0}")]
    Read(String),
    #[error("IPC secret has invalid length: expected {expected} bytes, got {actual}")]
    InvalidIpcSecretLength { expected: usize, actual: usize },
    #[cfg(not(target_os = "macos"))]
    #[error("keychain is not supported on this platform")]
    UnsupportedPlatform,
}

#[cfg(target_os = "macos")]
pub fn store_credentials(creds: &Credentials) -> KeychainResult<()> {
    let json = serde_json::to_string(creds).map_err(KeychainError::Encode)?;
    // Delete existing entry first (ignore error if it doesn't exist)
    let _ = security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT);
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
        Err(_) => Ok(None),
    }
}

#[cfg(target_os = "macos")]
pub fn clear_credentials() -> KeychainResult<()> {
    let _ = security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT);
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn load_or_create_ipc_secret(_data_dir: &Path) -> KeychainResult<Vec<u8>> {
    match security_framework::passwords::get_generic_password(SERVICE, IPC_SECRET_ACCOUNT) {
        Ok(secret) if secret.len() == IPC_SECRET_BYTES => Ok(secret),
        Ok(secret) => {
            let actual = secret.len();
            let secret = new_ipc_secret();
            let _ =
                security_framework::passwords::delete_generic_password(SERVICE, IPC_SECRET_ACCOUNT);
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
        Err(_) => {
            let secret = new_ipc_secret();
            security_framework::passwords::set_generic_password(
                SERVICE,
                IPC_SECRET_ACCOUNT,
                &secret,
            )
            .map_err(|e| KeychainError::Store(e.to_string()))?;
            Ok(secret)
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn store_credentials(_creds: &Credentials) -> KeychainResult<()> {
    Err(KeychainError::UnsupportedPlatform)
}

#[cfg(not(target_os = "macos"))]
pub fn load_credentials() -> KeychainResult<Option<Credentials>> {
    Ok(None)
}

#[cfg(not(target_os = "macos"))]
pub fn clear_credentials() -> KeychainResult<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn load_or_create_ipc_secret(_data_dir: &Path) -> KeychainResult<Vec<u8>> {
    Err(KeychainError::UnsupportedPlatform)
}

fn new_ipc_secret() -> Vec<u8> {
    random_bytes::<IPC_SECRET_BYTES>().to_vec()
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    rand::rng().fill(&mut bytes);
    bytes
}
