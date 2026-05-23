//! macOS Keychain integration for credential persistence.

use serde::{Deserialize, Serialize};

const SERVICE: &str = "com.clipper.daemon";
const ACCOUNT: &str = "credentials";

#[derive(Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub device_name: String,
    pub server_url: String,
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
