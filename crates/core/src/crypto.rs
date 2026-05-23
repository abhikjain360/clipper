use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use rand::RngExt;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Argon2id parameters — same structure used for both auth and enc derivation.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Argon2Params {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for Argon2Params {
    fn default() -> Self {
        Self {
            m_cost: 65536, // 64 MiB
            t_cost: 3,
            p_cost: 1,
        }
    }
}

/// Generate a random 16-byte salt.
pub fn generate_salt() -> [u8; 16] {
    rand::rng().random()
}

/// Generate a random 24-byte nonce for XChaCha20-Poly1305.
pub fn generate_nonce() -> [u8; 24] {
    rand::rng().random()
}

/// Generate a random 32-byte session token.
pub fn generate_token() -> [u8; 32] {
    rand::rng().random()
}

/// SHA-256 hash.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

fn build_argon2(params: &Argon2Params) -> Result<Argon2<'static>, CryptoError> {
    let p = Params::new(params.m_cost, params.t_cost, params.p_cost, Some(32))
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, p))
}

/// Derive a 32-byte key from passphrase + salt using Argon2id.
pub fn derive_key(
    passphrase: &[u8],
    salt: &[u8],
    params: &Argon2Params,
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let argon2 = build_argon2(params)?;
    let mut key = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(passphrase, salt, key.as_mut())
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(key)
}

/// Compute the auth hash for storing/verifying the passphrase.
pub fn compute_auth_hash(
    passphrase: &[u8],
    auth_salt: &[u8],
    params: &Argon2Params,
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    derive_key(passphrase, auth_salt, params)
}

/// Verify passphrase against stored auth hash using constant-time comparison.
pub fn verify_auth(
    passphrase: &[u8],
    auth_salt: &[u8],
    params: &Argon2Params,
    stored_hash: &[u8; 32],
) -> Result<bool, CryptoError> {
    let computed = compute_auth_hash(passphrase, auth_salt, params)?;
    Ok(constant_time_eq::constant_time_eq(
        computed.as_ref(),
        stored_hash,
    ))
}

/// Encrypt plaintext with XChaCha20-Poly1305.
/// Returns (nonce, ciphertext).
pub fn encrypt(
    key: &[u8; 32],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let nonce_bytes = generate_nonce();
    let nonce = XNonce::from_slice(&nonce_bytes);
    let cipher = XChaCha20Poly1305::new(key.into());

    use chacha20poly1305::aead::Payload;
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|e| CryptoError::Encrypt(e.to_string()))?;

    Ok((nonce_bytes.to_vec(), ciphertext))
}

/// Decrypt ciphertext with XChaCha20-Poly1305.
pub fn decrypt(
    key: &[u8; 32],
    nonce: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let nonce = XNonce::from_slice(nonce);
    let cipher = XChaCha20Poly1305::new(key.into());

    use chacha20poly1305::aead::Payload;
    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|e| CryptoError::Decrypt(e.to_string()))?;

    Ok(plaintext)
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("KDF error: {0}")]
    Kdf(String),
    #[error("encryption error: {0}")]
    Encrypt(String),
    #[error("decryption error: {0}")]
    Decrypt(String),
}

// ── Associated data constants ──
pub const AAD_CLIPBOARD_V1: &[u8] = b"clipper:clipboard:v1";
pub const AAD_FILE_META_V1: &[u8] = b"clipper:file-meta:v1";
pub const AAD_FILE_BLOB_V1: &[u8] = b"clipper:file-blob:v1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = derive_key(
            b"test-passphrase",
            b"0123456789abcdef",
            &Argon2Params::default(),
        )
        .expect("derive_key");
        let plaintext = b"hello clipper";
        let aad = AAD_CLIPBOARD_V1;

        let (nonce, ciphertext) = encrypt(&key, plaintext, aad).expect("encrypt");
        let decrypted = decrypt(&key, &nonce, &ciphertext, aad).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_decrypt_wrong_key_fails() {
        let key1 = derive_key(b"pass1", b"0123456789abcdef", &Argon2Params::default()).unwrap();
        let key2 = derive_key(b"pass2", b"0123456789abcdef", &Argon2Params::default()).unwrap();
        let (nonce, ct) = encrypt(&key1, b"secret", AAD_CLIPBOARD_V1).unwrap();
        assert!(decrypt(&key2, &nonce, &ct, AAD_CLIPBOARD_V1).is_err());
    }

    #[test]
    fn test_decrypt_wrong_aad_fails() {
        let key = derive_key(b"pass", b"0123456789abcdef", &Argon2Params::default()).unwrap();
        let (nonce, ct) = encrypt(&key, b"secret", AAD_CLIPBOARD_V1).unwrap();
        assert!(decrypt(&key, &nonce, &ct, AAD_FILE_META_V1).is_err());
    }

    #[test]
    fn test_auth_hash_verify() {
        let params = Argon2Params::default();
        let salt = generate_salt();
        let hash = compute_auth_hash(b"my-passphrase", &salt, &params).unwrap();
        assert!(verify_auth(b"my-passphrase", &salt, &params, &hash).unwrap());
        assert!(!verify_auth(b"wrong-passphrase", &salt, &params, &hash).unwrap());
    }

    #[test]
    fn test_sha256() {
        let hash = sha256(b"hello");
        assert_eq!(hash.len(), 32);
        assert_eq!(hash, sha256(b"hello"));
        assert_ne!(hash, sha256(b"world"));
    }

    #[test]
    fn test_separate_salts_produce_different_keys() {
        let params = Argon2Params::default();
        let key1 = derive_key(b"same-pass", b"salt-for-auth0000", &params).unwrap();
        let key2 = derive_key(b"same-pass", b"salt-for-enc00000", &params).unwrap();
        assert_ne!(&*key1, &*key2);
    }
}
