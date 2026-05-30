//! Server-managed pepper used to AEAD-wrap auth-side blobs at rest and
//! to pepper Argon2 access-key hashes. The root secret must live outside
//! the database — a DB-only leak should not enable offline brute force.
//!
//! The secret is loaded once at startup from `CLIPPER_SERVER_SECRET`
//! (base64) or `CLIPPER_SERVER_SECRET_FILE` (path to a file whose
//! trimmed contents are base64). Subkeys are derived per purpose via
//! HKDF-SHA256; call sites take a `ServerSecrets` reference and reach
//! for the field that matches the column they touch.

use std::path::PathBuf;

use base64::Engine;
use clipper_core::crypto::{
    HKDF_LABEL_ACCESS_KEY_HASH_SALT_V1, HKDF_LABEL_ACCESS_KEY_PEPPER_V1,
    HKDF_LABEL_ENCRYPTION_SALT_V1, HKDF_LABEL_OPAQUE_PASSWORD_FILE_V1,
    HKDF_LABEL_OPAQUE_SERVER_SETUP_V1, SERVER_SECRET_BYTES, derive_subkey,
};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::error::ServerResult;

#[derive(Debug, thiserror::Error)]
pub enum SecretLoadError {
    #[error("set only one of CLIPPER_SERVER_SECRET or CLIPPER_SERVER_SECRET_FILE, not both")]
    BothEnvAndFileSet,
    #[error("failed to read CLIPPER_SERVER_SECRET_FILE ({}): {source}", path.display())]
    FileRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "CLIPPER_SERVER_SECRET or CLIPPER_SERVER_SECRET_FILE must be set; run `clipper-server generate-secret` to mint one"
    )]
    NotSet,
    #[error("server secret is not valid base64: {0}")]
    InvalidBase64(#[from] base64::DecodeError),
    #[error("server secret must decode to exactly {expected} bytes, got {actual}")]
    WrongLength { expected: usize, actual: usize },
}

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub const ENV_SECRET: &str = "CLIPPER_SERVER_SECRET";
pub const ENV_SECRET_FILE: &str = "CLIPPER_SERVER_SECRET_FILE";

/// Precomputed at-rest wrapping subkeys plus the pepper byte string used
/// as Argon2's `secret` for access-key hashing.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ServerSecrets {
    pub opaque_server_setup: [u8; 32],
    pub opaque_password_file: [u8; 32],
    pub encryption_salt: [u8; 32],
    pub access_key_hash_salt: [u8; 32],
    pub access_key_pepper: [u8; 32],
}

impl ServerSecrets {
    pub fn from_root(root: &[u8; SERVER_SECRET_BYTES]) -> Self {
        Self {
            opaque_server_setup: *derive_subkey(root, HKDF_LABEL_OPAQUE_SERVER_SETUP_V1),
            opaque_password_file: *derive_subkey(root, HKDF_LABEL_OPAQUE_PASSWORD_FILE_V1),
            encryption_salt: *derive_subkey(root, HKDF_LABEL_ENCRYPTION_SALT_V1),
            access_key_hash_salt: *derive_subkey(root, HKDF_LABEL_ACCESS_KEY_HASH_SALT_V1),
            access_key_pepper: *derive_subkey(root, HKDF_LABEL_ACCESS_KEY_PEPPER_V1),
        }
    }

    /// Load the root pepper from `CLIPPER_SERVER_SECRET` or
    /// `CLIPPER_SERVER_SECRET_FILE` and expand it into per-purpose
    /// subkeys. Fails closed if neither is set or the input is invalid.
    pub fn load_from_env() -> ServerResult<Self> {
        let root = load_root_from_env()?;
        Ok(Self::from_root(&root))
    }

    /// Used by tests that don't want to plumb env vars through `AppState`.
    #[cfg(test)]
    pub fn test_fixture() -> Self {
        Self::from_root(&[0x42_u8; SERVER_SECRET_BYTES])
    }
}

fn load_root_from_env() -> Result<Zeroizing<[u8; SERVER_SECRET_BYTES]>, SecretLoadError> {
    let env_value = std::env::var(ENV_SECRET).ok();
    let file_path = std::env::var(ENV_SECRET_FILE).ok().map(PathBuf::from);

    let raw = match (env_value, file_path) {
        (Some(_), Some(_)) => return Err(SecretLoadError::BothEnvAndFileSet),
        (Some(v), None) => v,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map_err(|source| SecretLoadError::FileRead { path, source })?,
        (None, None) => return Err(SecretLoadError::NotSet),
    };

    decode_root(raw.trim())
}

fn decode_root(s: &str) -> Result<Zeroizing<[u8; SERVER_SECRET_BYTES]>, SecretLoadError> {
    let mut decoded = B64.decode(s)?;
    if decoded.len() != SERVER_SECRET_BYTES {
        let actual = decoded.len();
        decoded.zeroize();
        return Err(SecretLoadError::WrongLength {
            expected: SERVER_SECRET_BYTES,
            actual,
        });
    }
    let mut root = Zeroizing::new([0_u8; SERVER_SECRET_BYTES]);
    root.copy_from_slice(&decoded);
    decoded.zeroize();
    Ok(root)
}

/// Mint a fresh 32-byte secret and return it base64-encoded. Stdout for
/// the `generate-secret` CLI. Caller is responsible for storing it.
pub fn generate_root_base64() -> String {
    let bytes = clipper_core::crypto::generate_random_bytes(SERVER_SECRET_BYTES);
    let encoded = B64.encode(&bytes);
    // Don't leak the plaintext through the Vec drop path.
    let mut bytes = bytes;
    bytes.zeroize();
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_root_produces_label_separated_subkeys() {
        let secrets = ServerSecrets::from_root(&[0x11_u8; SERVER_SECRET_BYTES]);
        // All subkeys derived from the same root must still be distinct
        // because their HKDF labels differ.
        let all = [
            secrets.opaque_server_setup,
            secrets.opaque_password_file,
            secrets.encryption_salt,
            secrets.access_key_hash_salt,
            secrets.access_key_pepper,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "subkey collision between {i} and {j}");
            }
        }
    }

    #[test]
    fn from_root_is_deterministic() {
        let a = ServerSecrets::from_root(&[0x22_u8; SERVER_SECRET_BYTES]);
        let b = ServerSecrets::from_root(&[0x22_u8; SERVER_SECRET_BYTES]);
        assert_eq!(a.opaque_server_setup, b.opaque_server_setup);
        assert_eq!(a.access_key_pepper, b.access_key_pepper);
    }

    #[test]
    fn decode_root_accepts_valid_base64() {
        let bytes = [0xAB_u8; SERVER_SECRET_BYTES];
        let encoded = B64.encode(bytes);
        let decoded = decode_root(&encoded).expect("decode");
        assert_eq!(*decoded, bytes);
    }

    #[test]
    fn decode_root_rejects_wrong_length() {
        let encoded = B64.encode([0_u8; SERVER_SECRET_BYTES - 1]);
        assert!(decode_root(&encoded).is_err());
    }

    #[test]
    fn decode_root_rejects_garbage() {
        assert!(decode_root("not-base64-!!!").is_err());
    }

    #[test]
    fn generate_root_base64_roundtrips() {
        let encoded = generate_root_base64();
        decode_root(&encoded).expect("generated secret decodes");
    }
}
