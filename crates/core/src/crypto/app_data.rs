use clipper_api_types::{AppDataChange, AppDataRowId};
use ed25519_dalek::{Signer, SigningKey};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::{CryptoError, decrypt, derive_subkey, encrypt, verify_device_signature};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppDataValueEnvelope {
    pub collection: String,
    pub row_id: AppDataRowId,
    pub schema_version: u64,
    pub written_at: String,
    pub deleted: bool,
    pub value: Option<serde_json::Value>,
}

pub fn derive_app_data_row_key_key(data_key: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    derive_subkey(data_key, b"clipper:app-data:row-key:v1")
}

pub fn derive_app_data_value_key(data_key: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    derive_subkey(data_key, b"clipper:app-data:value:v1")
}

pub fn app_data_row_key(key: &[u8; 32], collection: &str, row_id: AppDataRowId) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 32-byte key");
    mac.update(collection.as_bytes());
    mac.update(&[0]);
    mac.update(row_id.into_uuid().as_bytes());
    mac.finalize().into_bytes().into()
}

fn value_aad(row_key: &[u8; 32], revision: u64) -> [u8; 40] {
    let mut aad = [0; 40];
    aad[..32].copy_from_slice(row_key);
    aad[32..].copy_from_slice(&revision.to_be_bytes());
    aad
}

pub fn encrypt_app_data_value(
    value_key: &[u8; 32],
    row_key: &[u8; 32],
    revision: u64,
    plaintext: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    encrypt(value_key, plaintext, &value_aad(row_key, revision))
}

pub fn decrypt_app_data_value(
    value_key: &[u8; 32],
    row_key: &[u8; 32],
    revision: u64,
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    decrypt(value_key, nonce, ciphertext, &value_aad(row_key, revision))
}

fn change_message(change: &AppDataChange) -> Result<Vec<u8>, CryptoError> {
    if change.row_key.len() != 32 {
        return Err(CryptoError::Signature("invalid row key length".into()));
    }
    if change.nonce.len() != 24 || change.ciphertext.len() < 16 {
        return Err(CryptoError::Signature("invalid change value".into()));
    }
    let mut hasher = Sha256::new();
    hasher.update(&change.nonce);
    hasher.update(&change.ciphertext);
    let hash: [u8; 32] = hasher.finalize().into();
    let mut message = b"clipper:app-data-change:v1".to_vec();
    message.extend_from_slice(&change.row_key);
    message.extend_from_slice(&change.revision.to_be_bytes());
    message.push(u8::from(change.deleted));
    message.extend_from_slice(&hash);
    message.extend_from_slice(change.device_id.into_uuid().as_bytes());
    Ok(message)
}

pub fn sign_app_data_change(
    secret_key: &[u8; 32],
    change: &AppDataChange,
) -> Result<Vec<u8>, CryptoError> {
    Ok(SigningKey::from_bytes(secret_key)
        .sign(&change_message(change)?)
        .to_bytes()
        .to_vec())
}

pub fn verify_app_data_change_signature(
    public_key: &[u8],
    change: &AppDataChange,
) -> Result<(), CryptoError> {
    verify_device_signature(
        public_key,
        &change_message(change)?,
        &change.signature,
        "app data signature",
    )
}
