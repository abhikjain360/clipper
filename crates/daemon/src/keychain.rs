//! macOS Keychain integration for credential persistence.

use serde::{Deserialize, Serialize};

const SERVICE: &str = "com.clipper.daemon";
const ACCOUNT: &str = "credentials";

#[derive(Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub device_name: String,
    pub server_url: String,
}

#[cfg(target_os = "macos")]
pub fn store_credentials(creds: &Credentials) -> anyhow::Result<()> {
    let json = serde_json::to_string(creds)?;
    // Delete existing entry first (ignore error if it doesn't exist)
    let _ = security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT);
    security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, json.as_bytes())
        .map_err(|e| anyhow::anyhow!("Keychain store failed: {}", e))?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn load_credentials() -> anyhow::Result<Option<Credentials>> {
    match security_framework::passwords::get_generic_password(SERVICE, ACCOUNT) {
        Ok(data) => {
            let json = String::from_utf8(data)?;
            let value: serde_json::Value = serde_json::from_str(&json)?;
            let had_legacy_passphrase = value.get("passphrase").is_some();
            let creds: Credentials = serde_json::from_value(value)?;
            if had_legacy_passphrase {
                store_credentials(&creds)?;
            }
            Ok(Some(creds))
        }
        Err(_) => Ok(None),
    }
}

#[cfg(target_os = "macos")]
pub fn clear_credentials() -> anyhow::Result<()> {
    let _ = security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn store_credentials(_creds: &Credentials) -> anyhow::Result<()> {
    anyhow::bail!("Keychain not supported on this platform")
}

#[cfg(not(target_os = "macos"))]
pub fn load_credentials() -> anyhow::Result<Option<Credentials>> {
    Ok(None)
}

#[cfg(not(target_os = "macos"))]
pub fn clear_credentials() -> anyhow::Result<()> {
    Ok(())
}
