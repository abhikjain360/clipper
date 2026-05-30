//! Thin field-specific wrappers around `clipper_core::crypto::wrap_with_key`
//! / `unwrap_with_key`. Each pair binds a column to its dedicated subkey
//! and AAD so call sites cannot accidentally use the wrong purpose.

use clipper_core::crypto::{
    self, AAD_WRAP_ACCESS_KEY_HASH_SALT_V1, AAD_WRAP_ENCRYPTION_SALT_V1,
    AAD_WRAP_OPAQUE_PASSWORD_FILE_V1, AAD_WRAP_OPAQUE_SERVER_SETUP_V1, CryptoError,
};

use crate::secret::ServerSecrets;

pub fn wrap_opaque_server_setup(
    secrets: &ServerSecrets,
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    crypto::wrap_with_key(
        &secrets.opaque_server_setup,
        plaintext,
        AAD_WRAP_OPAQUE_SERVER_SETUP_V1,
    )
}

pub fn unwrap_opaque_server_setup(
    secrets: &ServerSecrets,
    blob: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    crypto::unwrap_with_key(
        &secrets.opaque_server_setup,
        blob,
        AAD_WRAP_OPAQUE_SERVER_SETUP_V1,
    )
}

pub fn wrap_opaque_password_file(
    secrets: &ServerSecrets,
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    crypto::wrap_with_key(
        &secrets.opaque_password_file,
        plaintext,
        AAD_WRAP_OPAQUE_PASSWORD_FILE_V1,
    )
}

pub fn unwrap_opaque_password_file(
    secrets: &ServerSecrets,
    blob: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    crypto::unwrap_with_key(
        &secrets.opaque_password_file,
        blob,
        AAD_WRAP_OPAQUE_PASSWORD_FILE_V1,
    )
}

pub fn wrap_encryption_salt(
    secrets: &ServerSecrets,
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    crypto::wrap_with_key(
        &secrets.encryption_salt,
        plaintext,
        AAD_WRAP_ENCRYPTION_SALT_V1,
    )
}

#[cfg(test)]
pub fn unwrap_encryption_salt(
    secrets: &ServerSecrets,
    blob: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    crypto::unwrap_with_key(&secrets.encryption_salt, blob, AAD_WRAP_ENCRYPTION_SALT_V1)
}

pub fn wrap_access_key_hash_salt(
    secrets: &ServerSecrets,
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    crypto::wrap_with_key(
        &secrets.access_key_hash_salt,
        plaintext,
        AAD_WRAP_ACCESS_KEY_HASH_SALT_V1,
    )
}

pub fn unwrap_access_key_hash_salt(
    secrets: &ServerSecrets,
    blob: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    crypto::unwrap_with_key(
        &secrets.access_key_hash_salt,
        blob,
        AAD_WRAP_ACCESS_KEY_HASH_SALT_V1,
    )
}
