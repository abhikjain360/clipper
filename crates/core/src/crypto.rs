use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadCore, KeyInit, generic_array::typenum::Unsigned},
};
pub use clipper_api_types::Argon2Params;
use rand::RngExt;
use sha2::{Digest, Sha256, digest::OutputSizeUser};
use zeroize::Zeroizing;

const OPAQUE_CREDENTIAL_IDENTIFIER: &[u8] = b"clipper:passphrase:v1";
pub const XCHACHA20_NONCE_BYTES: usize =
    <<XChaCha20Poly1305 as AeadCore>::NonceSize as Unsigned>::USIZE;
pub const SHA256_BYTES: usize = <<Sha256 as OutputSizeUser>::OutputSize as Unsigned>::USIZE;
pub const ACCESS_KEY_HASH_SALT_BYTES: usize = 16;
pub const ACCESS_KEY_HASH_BYTES: usize = 32;

const ACCESS_KEY_HASH_PARAMS: Argon2Params = Argon2Params {
    m_cost: 19 * 1024,
    t_cost: 2,
    p_cost: 1,
};

struct ClipperOpaqueCipherSuite;

impl opaque_ke::CipherSuite for ClipperOpaqueCipherSuite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, sha2::Sha512>;
    type Ksf = opaque_ke::argon2::Argon2<'static>;
}

/// Generate a random 16-byte salt for client-side encryption key derivation.
pub fn generate_encryption_salt() -> [u8; 16] {
    rand::rng().random()
}

/// Generate a random 16-byte salt for server-side access-key hashing.
pub fn generate_access_key_hash_salt() -> [u8; ACCESS_KEY_HASH_SALT_BYTES] {
    rand::rng().random()
}

/// Generate a random 24-byte nonce for XChaCha20-Poly1305.
pub fn generate_nonce() -> [u8; XCHACHA20_NONCE_BYTES] {
    rand::rng().random()
}

/// Generate a random 32-byte session token.
pub fn generate_token() -> [u8; 32] {
    rand::rng().random()
}

/// SHA-256 hash.
pub fn sha256(data: &[u8]) -> [u8; SHA256_BYTES] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Derive the stored verifier for a registration access key using Argon2id.
pub fn access_key_hash(
    access_key: &[u8],
    salt: &[u8],
) -> Result<[u8; ACCESS_KEY_HASH_BYTES], CryptoError> {
    let argon2 = build_argon2(&ACCESS_KEY_HASH_PARAMS)?;
    let mut hash = [0u8; ACCESS_KEY_HASH_BYTES];
    argon2
        .hash_password_into(access_key, salt, &mut hash)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(hash)
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

/// Create the OPAQUE server setup and password file stored by the server.
///
/// The password file is a verifier: stealing it allows offline guessing, but
/// does not give an attacker the material needed to complete a login directly.
pub fn opaque_register(passphrase: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let server_setup = opaque_new_server_setup();
    let (registration_request, client_state) = opaque_client_register_start(passphrase)?;
    let registration_response = opaque_server_register_start(
        &server_setup,
        &registration_request,
        OPAQUE_CREDENTIAL_IDENTIFIER,
    )?;
    let registration_upload =
        opaque_client_register_finish(&client_state, passphrase, &registration_response)?;
    let password_file = opaque_server_register_finish(&registration_upload)?;

    Ok((server_setup, password_file))
}

/// Generate a new serialized OPAQUE server setup.
pub fn opaque_new_server_setup() -> Vec<u8> {
    let mut rng = opaque_ke::rand::rngs::OsRng;
    opaque_ke::ServerSetup::<ClipperOpaqueCipherSuite>::new(&mut rng)
        .serialize()
        .to_vec()
}

/// Start an OPAQUE registration on the client.
///
/// Returns the registration request to send to the server and serialized client
/// state that must be kept only until registration is finished.
pub fn opaque_client_register_start(passphrase: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let mut rng = opaque_ke::rand::rngs::OsRng;
    let start =
        opaque_ke::ClientRegistration::<ClipperOpaqueCipherSuite>::start(&mut rng, passphrase)
            .map_err(opaque_error)?;

    Ok((
        start.message.serialize().to_vec(),
        start.state.serialize().to_vec(),
    ))
}

/// Finish an OPAQUE registration on the client.
///
/// Returns the registration upload to send to the server. The upload is a
/// password-derived verifier payload, not the raw password.
pub fn opaque_client_register_finish(
    client_state: &[u8],
    passphrase: &[u8],
    registration_response: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let mut rng = opaque_ke::rand::rngs::OsRng;
    let client_registration =
        opaque_ke::ClientRegistration::<ClipperOpaqueCipherSuite>::deserialize(client_state)
            .map_err(opaque_error)?;
    let response = opaque_ke::RegistrationResponse::<ClipperOpaqueCipherSuite>::deserialize(
        registration_response,
    )
    .map_err(opaque_error)?;
    let finish = client_registration
        .finish(
            &mut rng,
            passphrase,
            response,
            opaque_ke::ClientRegistrationFinishParameters::default(),
        )
        .map_err(opaque_error)?;

    Ok(finish.message.serialize().to_vec())
}

/// Start an OPAQUE registration on the server.
pub fn opaque_server_register_start(
    server_setup: &[u8],
    registration_request: &[u8],
    credential_identifier: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let server_setup =
        opaque_ke::ServerSetup::<ClipperOpaqueCipherSuite>::deserialize(server_setup)
            .map_err(opaque_error)?;
    let request = opaque_ke::RegistrationRequest::<ClipperOpaqueCipherSuite>::deserialize(
        registration_request,
    )
    .map_err(opaque_error)?;
    let start = opaque_ke::ServerRegistration::<ClipperOpaqueCipherSuite>::start(
        &server_setup,
        request,
        credential_identifier,
    )
    .map_err(opaque_error)?;

    Ok(start.message.serialize().to_vec())
}

/// Finish an OPAQUE registration on the server and return the serialized
/// password file/verifier to store for future logins.
pub fn opaque_server_register_finish(registration_upload: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let upload =
        opaque_ke::RegistrationUpload::<ClipperOpaqueCipherSuite>::deserialize(registration_upload)
            .map_err(opaque_error)?;
    let password_file = opaque_ke::ServerRegistration::<ClipperOpaqueCipherSuite>::finish(upload);

    Ok(password_file.serialize().to_vec())
}

/// Start an OPAQUE login on the client.
///
/// Returns the credential request to send to the server and serialized client
/// state that must be kept only until the login is finished.
pub fn opaque_client_login_start(passphrase: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let mut rng = opaque_ke::rand::rngs::OsRng;
    let start = opaque_ke::ClientLogin::<ClipperOpaqueCipherSuite>::start(&mut rng, passphrase)
        .map_err(opaque_error)?;

    Ok((
        start.message.serialize().to_vec(),
        start.state.serialize().to_vec(),
    ))
}

/// Finish an OPAQUE login on the client.
///
/// Returns the credential finalization message to send to the server and the
/// negotiated session key. The current HTTP API only needs the finalization
/// message, but returning the key lets tests verify both sides agree.
pub fn opaque_client_login_finish(
    client_state: &[u8],
    passphrase: &[u8],
    credential_response: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let mut rng = opaque_ke::rand::rngs::OsRng;
    let client_login =
        opaque_ke::ClientLogin::<ClipperOpaqueCipherSuite>::deserialize(client_state)
            .map_err(opaque_error)?;
    let response =
        opaque_ke::CredentialResponse::<ClipperOpaqueCipherSuite>::deserialize(credential_response)
            .map_err(opaque_error)?;
    let finish = client_login
        .finish(
            &mut rng,
            passphrase,
            response,
            opaque_ke::ClientLoginFinishParameters::default(),
        )
        .map_err(opaque_error)?;

    Ok((
        finish.message.serialize().to_vec(),
        finish.session_key.to_vec(),
    ))
}

/// Start an OPAQUE login on the server.
///
/// Returns the credential response for the client and serialized server state
/// that must be retained until the finalization request arrives.
pub fn opaque_server_login_start(
    server_setup: &[u8],
    password_file: &[u8],
    credential_request: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    opaque_server_login_start_with_identifier(
        server_setup,
        password_file,
        credential_request,
        OPAQUE_CREDENTIAL_IDENTIFIER,
    )
}

/// Start an OPAQUE login on the server with an explicit credential identifier.
pub fn opaque_server_login_start_with_identifier(
    server_setup: &[u8],
    password_file: &[u8],
    credential_request: &[u8],
    credential_identifier: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let mut rng = opaque_ke::rand::rngs::OsRng;
    let server_setup =
        opaque_ke::ServerSetup::<ClipperOpaqueCipherSuite>::deserialize(server_setup)
            .map_err(opaque_error)?;
    let password_file =
        opaque_ke::ServerRegistration::<ClipperOpaqueCipherSuite>::deserialize(password_file)
            .map_err(opaque_error)?;
    let request =
        opaque_ke::CredentialRequest::<ClipperOpaqueCipherSuite>::deserialize(credential_request)
            .map_err(opaque_error)?;
    let start = opaque_ke::ServerLogin::<ClipperOpaqueCipherSuite>::start(
        &mut rng,
        &server_setup,
        Some(password_file),
        request,
        credential_identifier,
        opaque_ke::ServerLoginParameters::default(),
    )
    .map_err(opaque_error)?;

    Ok((
        start.message.serialize().to_vec(),
        start.state.serialize().to_vec(),
    ))
}

/// Finish an OPAQUE login on the server.
///
/// Returns the negotiated session key. The caller can ignore it when using a
/// random bearer token after successful password authentication.
pub fn opaque_server_login_finish(
    server_state: &[u8],
    credential_finalization: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let server_login =
        opaque_ke::ServerLogin::<ClipperOpaqueCipherSuite>::deserialize(server_state)
            .map_err(opaque_error)?;
    let finalization = opaque_ke::CredentialFinalization::<ClipperOpaqueCipherSuite>::deserialize(
        credential_finalization,
    )
    .map_err(opaque_error)?;
    let finish = server_login
        .finish(finalization, opaque_ke::ServerLoginParameters::default())
        .map_err(opaque_error)?;

    Ok(finish.session_key.to_vec())
}

fn opaque_error(error: impl std::fmt::Display) -> CryptoError {
    CryptoError::Opaque(error.to_string())
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
    if nonce.len() != XCHACHA20_NONCE_BYTES {
        return Err(CryptoError::Decrypt(format!(
            "invalid nonce length: expected {} bytes, got {}",
            XCHACHA20_NONCE_BYTES,
            nonce.len()
        )));
    }

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
    #[error("OPAQUE error: {0}")]
    Opaque(String),
}

// ── Associated data constants ──
pub const AAD_CLIPBOARD_V1: &[u8] = b"clipper:clipboard:v1";
pub const AAD_CLIPBOARD_META_V1: &[u8] = b"clipper:clipboard-meta:v1";
pub const AAD_CLIPBOARD_PAYLOAD_V1: &[u8] = b"clipper:clipboard-payload:v1";
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
    fn test_decrypt_rejects_malformed_nonce_length() {
        let key = derive_key(b"pass", b"0123456789abcdef", &Argon2Params::default()).unwrap();
        let (_, ct) = encrypt(&key, b"secret", AAD_CLIPBOARD_V1).unwrap();
        let err = decrypt(&key, &[0_u8; 12], &ct, AAD_CLIPBOARD_V1).unwrap_err();

        assert!(matches!(
            err,
            CryptoError::Decrypt(message) if message.contains("invalid nonce length")
        ));
    }

    #[test]
    fn test_sha256() {
        let hash = sha256(b"hello");
        assert_eq!(hash.len(), 32);
        assert_eq!(hash, sha256(b"hello"));
        assert_ne!(hash, sha256(b"world"));
    }

    #[test]
    fn test_access_key_hash_uses_salt() {
        let salt1 = [1_u8; ACCESS_KEY_HASH_SALT_BYTES];
        let salt2 = [2_u8; ACCESS_KEY_HASH_SALT_BYTES];
        let hash1 = access_key_hash(b"invite", &salt1).unwrap();
        let hash1_again = access_key_hash(b"invite", &salt1).unwrap();
        let hash2 = access_key_hash(b"invite", &salt2).unwrap();

        assert_eq!(hash1.len(), ACCESS_KEY_HASH_BYTES);
        assert_eq!(hash1, hash1_again);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_separate_salts_produce_different_keys() {
        let params = Argon2Params::default();
        let key1 = derive_key(b"same-pass", b"salt-for-profile1", &params).unwrap();
        let key2 = derive_key(b"same-pass", b"salt-for-profile2", &params).unwrap();
        assert_ne!(&*key1, &*key2);
    }

    #[test]
    fn test_opaque_login_roundtrip() {
        let password = b"correct horse battery staple";
        let (server_setup, password_file) = opaque_register(password).unwrap();
        let (request, client_state) = opaque_client_login_start(password).unwrap();
        let (response, server_state) =
            opaque_server_login_start(&server_setup, &password_file, &request).unwrap();
        let (finalization, client_session_key) =
            opaque_client_login_finish(&client_state, password, &response).unwrap();
        let server_session_key = opaque_server_login_finish(&server_state, &finalization).unwrap();

        assert_eq!(client_session_key, server_session_key);
    }

    #[test]
    fn test_opaque_rejects_wrong_password() {
        let (server_setup, password_file) = opaque_register(b"correct password").unwrap();
        let (request, client_state) = opaque_client_login_start(b"wrong password").unwrap();
        let (response, _server_state) =
            opaque_server_login_start(&server_setup, &password_file, &request).unwrap();

        assert!(opaque_client_login_finish(&client_state, b"wrong password", &response).is_err());
    }
}
