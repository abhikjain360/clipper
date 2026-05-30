use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadCore, KeyInit, generic_array::typenum::Unsigned},
};
pub use clipper_api_types::Argon2Params;
use hkdf::Hkdf;
use rand::Rng;
use sha2::{Digest, Sha256, digest::OutputSizeUser};
use zeroize::Zeroizing;

pub const XCHACHA20_NONCE_BYTES: usize =
    <<XChaCha20Poly1305 as AeadCore>::NonceSize as Unsigned>::USIZE;
pub const SHA256_BYTES: usize = <<Sha256 as OutputSizeUser>::OutputSize as Unsigned>::USIZE;
pub const ACCESS_KEY_HASH_SALT_BYTES: usize = 16;
pub const ACCESS_KEY_HASH_BYTES: usize = 32;
pub const SERVER_SECRET_BYTES: usize = 32;
const OPAQUE_EXPORT_DATA_KEY_LABEL: &[u8] = b"clipper:opaque-export:data-key:v1";

const ACCESS_KEY_HASH_PARAMS_DEFAULT: Argon2Params = Argon2Params {
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

pub struct OpaqueRegistrationFinish {
    pub registration_upload: Vec<u8>,
    pub export_key: Zeroizing<Vec<u8>>,
}

pub struct OpaqueLoginFinish {
    pub credential_finalization: Vec<u8>,
    pub session_key: Vec<u8>,
    pub export_key: Zeroizing<Vec<u8>>,
}

/// Generate a random 16-byte salt for the legacy passphrase+salt data-key KDF.
pub fn generate_encryption_salt() -> [u8; 16] {
    generate_bytes::<16>()
}

/// Generate a random 16-byte salt for server-side access-key hashing.
pub fn generate_access_key_hash_salt() -> [u8; ACCESS_KEY_HASH_SALT_BYTES] {
    generate_bytes::<ACCESS_KEY_HASH_SALT_BYTES>()
}

/// Generate `count` random bytes.
pub fn generate_random_bytes(length: usize) -> Vec<u8> {
    let mut bytes = vec![0_u8; length];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

fn generate_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

/// Generate a random 24-byte nonce for XChaCha20-Poly1305.
pub fn generate_nonce() -> [u8; XCHACHA20_NONCE_BYTES] {
    generate_bytes::<XCHACHA20_NONCE_BYTES>()
}

/// Generate a random 32-byte session token.
pub fn generate_token() -> [u8; 32] {
    generate_bytes::<32>()
}

/// Generate a random session token with the requested size.
pub fn generate_token_with_length(length: usize) -> Vec<u8> {
    generate_random_bytes(length)
}

/// SHA-256 hash.
pub fn sha256(data: &[u8]) -> [u8; SHA256_BYTES] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Derive the stored verifier for a registration access key using Argon2id.
/// `secret` is the server-side pepper mixed into Argon2 — pass `None` only
/// for tests that don't care about pepper isolation.
pub fn access_key_hash(
    access_key: &[u8],
    salt: &[u8],
    secret: Option<&[u8]>,
) -> Result<[u8; ACCESS_KEY_HASH_BYTES], CryptoError> {
    access_key_hash_with_params(access_key, salt, secret, &ACCESS_KEY_HASH_PARAMS_DEFAULT)
}

/// Derive the stored verifier for a registration access key using configurable
/// Argon2id parameters and an optional server-side pepper.
pub fn access_key_hash_with_params(
    access_key: &[u8],
    salt: &[u8],
    secret: Option<&[u8]>,
    params: &Argon2Params,
) -> Result<[u8; ACCESS_KEY_HASH_BYTES], CryptoError> {
    let argon2 = build_argon2(secret, params)?;
    let mut hash = [0u8; ACCESS_KEY_HASH_BYTES];
    argon2
        .hash_password_into(access_key, salt, &mut hash)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(hash)
}

/// Default access key hash params for backwards-compatible startup.
pub fn default_access_key_hash_params() -> Argon2Params {
    ACCESS_KEY_HASH_PARAMS_DEFAULT
}

fn build_argon2<'a>(
    secret: Option<&'a [u8]>,
    params: &Argon2Params,
) -> Result<Argon2<'a>, CryptoError> {
    let p = Params::new(params.m_cost, params.t_cost, params.p_cost, Some(32))
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    match secret {
        Some(secret) => Argon2::new_with_secret(secret, Algorithm::Argon2id, Version::V0x13, p)
            .map_err(|e| CryptoError::Kdf(e.to_string())),
        None => Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, p)),
    }
}

/// Derive a legacy 32-byte key from passphrase + salt using Argon2id.
pub fn derive_key(
    passphrase: &[u8],
    salt: &[u8],
    params: &Argon2Params,
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let argon2 = build_argon2(None, params)?;
    let mut key = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(passphrase, salt, key.as_mut())
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(key)
}

/// Derive the client-side object encryption key from OPAQUE's stable export key.
pub fn derive_data_key_from_opaque_export_key(export_key: &[u8]) -> Zeroizing<[u8; 32]> {
    let hkdf = Hkdf::<Sha256>::new(None, export_key);
    let mut key = Zeroizing::new([0u8; 32]);
    hkdf.expand(OPAQUE_EXPORT_DATA_KEY_LABEL, key.as_mut())
        .expect("HKDF-SHA256 output of 32 bytes is within limits");
    key
}

/// HKDF-SHA256-derive a 32-byte subkey from the root server pepper.
/// `label` provides domain separation between purposes; collisions
/// between labels are a bug.
pub fn derive_subkey(root: &[u8; SERVER_SECRET_BYTES], label: &[u8]) -> Zeroizing<[u8; 32]> {
    let hkdf = Hkdf::<Sha256>::new(None, root);
    let mut okm = Zeroizing::new([0u8; 32]);
    hkdf.expand(label, okm.as_mut())
        .expect("HKDF-SHA256 output of 32 bytes is within limits");
    okm
}

/// Encrypt a small at-rest secret with a server-managed subkey and return
/// `nonce_24 || ciphertext_with_tag`. `aad` provides cross-field domain
/// separation so a ciphertext cannot be moved between columns.
pub fn wrap_with_key(key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let (nonce, ciphertext) = encrypt(key, plaintext, aad)?;
    let mut blob = Vec::with_capacity(nonce.len() + ciphertext.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}

/// Inverse of `wrap_with_key`.
pub fn unwrap_with_key(key: &[u8; 32], blob: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if blob.len() < XCHACHA20_NONCE_BYTES {
        return Err(CryptoError::Decrypt("wrapped blob too short".into()));
    }
    let (nonce, ciphertext) = blob.split_at(XCHACHA20_NONCE_BYTES);
    decrypt(key, nonce, ciphertext, aad)
}

/// Run all four registration steps (client + server) in-process for a single
/// `pw` and `id_U`, and return `(opaque_server_setup, opaque_password_file)`.
/// Used by tests. See `docs/opaque.md`.
pub fn opaque_register(
    passphrase: &[u8],
    credential_identifier: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let server_setup = opaque_new_server_setup();
    let (registration_request, client_state) = opaque_client_register_start(passphrase)?;
    let registration_response =
        opaque_server_register_start(&server_setup, &registration_request, credential_identifier)?;
    let finish = opaque_client_register_finish(&client_state, passphrase, &registration_response)?;
    let password_file = opaque_server_register_finish(&finish.registration_upload)?;

    Ok((server_setup, password_file))
}

/// Sample a fresh `opaque_server_setup = oprf_seed ‖ sk_S ‖ fake_sk` for one
/// user and return it serialized. See `docs/opaque.md`.
pub fn opaque_new_server_setup() -> Vec<u8> {
    let mut rng = opaque_ke::rand::rngs::OsRng;
    opaque_ke::ServerSetup::<ClipperOpaqueCipherSuite>::new(&mut rng)
        .serialize()
        .to_vec()
}

/// OPAQUE registration round 1, client side.
///
/// Input: `pw = passphrase`.
/// Picks blind `r ← Z_q`, computes `M = r · H(pw)`, returns
/// `(RegistrationRequest = M, state_C)`. `state_C` carries `(r, pw, ...)`
/// and must be held until `opaque_client_register_finish`.
/// See `docs/opaque.md`.
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

/// OPAQUE registration round 2, client side.
///
/// Inputs: `state_C`, `pw`, `RegistrationResponse = (N, pk_S)`.
/// Computes `Y = r⁻¹ · N`, `rwd = KSF(Y)`, derives
/// `masking_key, auth_key, sk_C, env_nonce` from `rwd`, sets
/// `pk_C = sk_C · B`, builds `env = env_nonce ‖ MAC(auth_key, env_nonce ‖ pk_S)`,
/// and returns `RegistrationUpload = env ‖ masking_key ‖ pk_C`.
/// See `docs/opaque.md`.
pub fn opaque_client_register_finish(
    client_state: &[u8],
    passphrase: &[u8],
    registration_response: &[u8],
) -> Result<OpaqueRegistrationFinish, CryptoError> {
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

    Ok(OpaqueRegistrationFinish {
        registration_upload: finish.message.serialize().to_vec(),
        export_key: Zeroizing::new(finish.export_key.to_vec()),
    })
}

/// OPAQUE registration round 1, server side.
///
/// Inputs: `opaque_server_setup` (= `oprf_seed ‖ sk_S ‖ fake_sk`),
/// `registration_request = M`, `credential_identifier = id_U`.
/// Derives `k_U = Expand(oprf_seed, id_U)`, computes `N = k_U · M`,
/// returns `RegistrationResponse = (N, pk_S)`. Stateless on the server.
/// See `docs/opaque.md`.
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

/// OPAQUE registration round 2, server side.
///
/// Input: `RegistrationUpload = env ‖ masking_key ‖ pk_C`.
/// Re-serializes it as `opaque_password_file` to store on the user row.
/// No cryptographic check is performed here.
/// See `docs/opaque.md`.
pub fn opaque_server_register_finish(registration_upload: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let upload =
        opaque_ke::RegistrationUpload::<ClipperOpaqueCipherSuite>::deserialize(registration_upload)
            .map_err(opaque_error)?;
    let password_file = opaque_ke::ServerRegistration::<ClipperOpaqueCipherSuite>::finish(upload);

    Ok(password_file.serialize().to_vec())
}

/// OPAQUE login round 1, client side.
///
/// Input: `pw = passphrase`.
/// Picks blind `r ← Z_q`, client AKE ephemeral `(x_C, X_C = x_C · B)`, and
/// `nonce_C ← random`; returns
/// `CredentialRequest = M ‖ ke1` where `M = r · H(pw)` and
/// `ke1 = nonce_C ‖ X_C`, plus `state_C = (r, pw, x_C, nonce_C, ke1)`.
/// See `docs/opaque.md`.
pub fn opaque_client_login_start(passphrase: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let mut rng = opaque_ke::rand::rngs::OsRng;
    let start = opaque_ke::ClientLogin::<ClipperOpaqueCipherSuite>::start(&mut rng, passphrase)
        .map_err(opaque_error)?;

    Ok((
        start.message.serialize().to_vec(),
        start.state.serialize().to_vec(),
    ))
}

/// OPAQUE login round 2, client side.
///
/// Inputs: `state_C`, `pw`,
/// `CredentialResponse = (N, nonce_M, masked, nonce_S, X_S, server_mac)`.
/// Computes `Y = r⁻¹ · N`, `rwd = KSF(Y)`, re-derives `masking_key` and
/// unmasks `(pk_S ‖ env) = masked XOR Expand(masking_key, nonce_M ‖ ·)`,
/// recovers `sk_C` and checks `auth_tag = MAC(auth_key, env_nonce ‖ pk_S)`.
/// Performs 3DH (`dh1 = x_C · X_S`, `dh2 = sk_C · X_S`, `dh3 = x_C · pk_S`),
/// derives `(server_mac_key, client_mac_key, session_key)`, verifies
/// `server_mac`, and returns `(CredentialFinalization = client_mac,
/// session_key, export_key)`. Clipper's HTTP API only sends `client_mac`;
/// the client derives its data-encryption key from `export_key`, and
/// `session_key` is exposed so tests can assert both sides agreed.
/// See `docs/opaque.md`.
pub fn opaque_client_login_finish(
    client_state: &[u8],
    passphrase: &[u8],
    credential_response: &[u8],
) -> Result<OpaqueLoginFinish, CryptoError> {
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

    Ok(OpaqueLoginFinish {
        credential_finalization: finish.message.serialize().to_vec(),
        session_key: finish.session_key.to_vec(),
        export_key: Zeroizing::new(finish.export_key.to_vec()),
    })
}

/// OPAQUE login round 1, server side.
///
/// Inputs: `opaque_server_setup`,
/// `opaque_password_file = env ‖ masking_key ‖ pk_C`,
/// `CredentialRequest = M ‖ ke1` where `ke1 = nonce_C ‖ X_C`,
/// `credential_identifier = id_U`.
///
/// Derives `k_U = Expand(oprf_seed, id_U)`, computes `N = k_U · M`,
/// samples `nonce_M ← random` and produces
/// `masked = (pk_S ‖ env) XOR Expand(masking_key, nonce_M ‖ ·)`.
/// Samples server AKE ephemeral `(x_S, X_S = x_S · B)` and `nonce_S`,
/// runs 3DH (`dh1 = x_S · X_C`, `dh2 = x_S · pk_C`, `dh3 = sk_S · X_C`),
/// derives `(server_mac_key, client_mac_key, session_key)` from
/// `ikm = dh1 ‖ dh2 ‖ dh3` and the transcript, and returns
/// `(CredentialResponse = (N, nonce_M, masked, nonce_S, X_S, server_mac),
///   state_S)`.
/// `state_S` holds `client_mac_key` and the expected MAC base; it must be
/// kept until `opaque_server_login_finish`. See `docs/opaque.md`.
pub fn opaque_server_login_start(
    server_setup: &[u8],
    password_file: Option<&[u8]>,
    credential_request: &[u8],
    credential_identifier: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let mut rng = opaque_ke::rand::rngs::OsRng;
    let server_setup =
        opaque_ke::ServerSetup::<ClipperOpaqueCipherSuite>::deserialize(server_setup)
            .map_err(opaque_error)?;
    // `None` selects opaque-ke's fake-record path: it fabricates a credential
    // response indistinguishable from a real one — and stable for a given
    // `credential_identifier` — so an unknown username cannot be enumerated.
    let password_file = password_file
        .map(opaque_ke::ServerRegistration::<ClipperOpaqueCipherSuite>::deserialize)
        .transpose()
        .map_err(opaque_error)?;
    let request =
        opaque_ke::CredentialRequest::<ClipperOpaqueCipherSuite>::deserialize(credential_request)
            .map_err(opaque_error)?;
    let start = opaque_ke::ServerLogin::<ClipperOpaqueCipherSuite>::start(
        &mut rng,
        &server_setup,
        password_file,
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

/// OPAQUE login round 2, server side.
///
/// Inputs: `state_S`, `CredentialFinalization = client_mac`.
/// Re-derives `expected_mac = MAC(client_mac_key, transcript_pre ‖ server_mac)`
/// and verifies it equals `client_mac`; on success returns `session_key`.
/// Clipper discards `session_key` and authenticates the session via a fresh
/// random bearer token instead. See `docs/opaque.md`.
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

// ── Server-pepper at-rest wrapping ──
//
// AAD strings bind a wrapped ciphertext to the column it lives in.
// Subkey labels feed HKDF for per-purpose key separation. The two
// MUST stay in sync: changing one without the other invalidates only
// some fields and creates silent migration bugs.
pub const AAD_WRAP_OPAQUE_SERVER_SETUP_V1: &[u8] = b"clipper:wrap:opaque-server-setup:v1";
pub const AAD_WRAP_OPAQUE_PASSWORD_FILE_V1: &[u8] = b"clipper:wrap:opaque-password-file:v1";
pub const AAD_WRAP_ENCRYPTION_SALT_V1: &[u8] = b"clipper:wrap:encryption-salt:v1";
pub const AAD_WRAP_ACCESS_KEY_HASH_SALT_V1: &[u8] = b"clipper:wrap:access-key-hash-salt:v1";

pub const HKDF_LABEL_OPAQUE_SERVER_SETUP_V1: &[u8] = b"clipper:hkdf:opaque-server-setup:v1";
pub const HKDF_LABEL_OPAQUE_PASSWORD_FILE_V1: &[u8] = b"clipper:hkdf:opaque-password-file:v1";
pub const HKDF_LABEL_ENCRYPTION_SALT_V1: &[u8] = b"clipper:hkdf:encryption-salt:v1";
pub const HKDF_LABEL_ACCESS_KEY_HASH_SALT_V1: &[u8] = b"clipper:hkdf:access-key-hash-salt:v1";
pub const HKDF_LABEL_ACCESS_KEY_PEPPER_V1: &[u8] = b"clipper:hkdf:access-key-pepper:v1";

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
        let hash1 = access_key_hash(b"invite", &salt1, None).unwrap();
        let hash1_again = access_key_hash(b"invite", &salt1, None).unwrap();
        let hash2 = access_key_hash(b"invite", &salt2, None).unwrap();

        assert_eq!(hash1.len(), ACCESS_KEY_HASH_BYTES);
        assert_eq!(hash1, hash1_again);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_access_key_hash_secret_changes_output() {
        let salt = [1_u8; ACCESS_KEY_HASH_SALT_BYTES];
        let no_secret = access_key_hash(b"invite", &salt, None).unwrap();
        let pepper_a = access_key_hash(b"invite", &salt, Some(&[7_u8; 32])).unwrap();
        let pepper_a_again = access_key_hash(b"invite", &salt, Some(&[7_u8; 32])).unwrap();
        let pepper_b = access_key_hash(b"invite", &salt, Some(&[8_u8; 32])).unwrap();

        assert_ne!(no_secret, pepper_a);
        assert_ne!(pepper_a, pepper_b);
        assert_eq!(pepper_a, pepper_a_again);
    }

    #[test]
    fn test_derive_subkey_is_deterministic_and_label_separated() {
        let root = [0x11_u8; SERVER_SECRET_BYTES];
        let a1 = derive_subkey(&root, b"label-a");
        let a2 = derive_subkey(&root, b"label-a");
        let b = derive_subkey(&root, b"label-b");

        assert_eq!(*a1, *a2);
        assert_ne!(*a1, *b);
    }

    #[test]
    fn test_derive_subkey_changes_with_root() {
        let root_a = [0x11_u8; SERVER_SECRET_BYTES];
        let root_b = [0x22_u8; SERVER_SECRET_BYTES];
        assert_ne!(*derive_subkey(&root_a, b"x"), *derive_subkey(&root_b, b"x"));
    }

    #[test]
    fn test_data_key_from_opaque_export_key_is_stable_and_separated() {
        let export_key = b"opaque export key material";
        let key1 = derive_data_key_from_opaque_export_key(export_key);
        let key2 = derive_data_key_from_opaque_export_key(export_key);
        let key3 = derive_data_key_from_opaque_export_key(b"different export key material");

        assert_eq!(*key1, *key2);
        assert_ne!(*key1, *key3);
    }

    #[test]
    fn test_wrap_unwrap_roundtrip() {
        let key = [0x42_u8; 32];
        let aad = AAD_WRAP_OPAQUE_SERVER_SETUP_V1;
        let plaintext = b"opaque server setup bytes";
        let blob = wrap_with_key(&key, plaintext, aad).expect("wrap");
        assert!(blob.len() >= XCHACHA20_NONCE_BYTES + plaintext.len());

        let recovered = unwrap_with_key(&key, &blob, aad).expect("unwrap");
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn test_unwrap_rejects_wrong_key() {
        let aad = AAD_WRAP_ENCRYPTION_SALT_V1;
        let blob = wrap_with_key(&[0x01_u8; 32], b"salt", aad).expect("wrap");
        assert!(unwrap_with_key(&[0x02_u8; 32], &blob, aad).is_err());
    }

    #[test]
    fn test_unwrap_rejects_wrong_aad() {
        let key = [0x33_u8; 32];
        let blob = wrap_with_key(&key, b"salt", AAD_WRAP_ENCRYPTION_SALT_V1).expect("wrap");
        assert!(unwrap_with_key(&key, &blob, AAD_WRAP_OPAQUE_SERVER_SETUP_V1).is_err());
    }

    #[test]
    fn test_unwrap_rejects_tampered_ciphertext() {
        let key = [0x55_u8; 32];
        let aad = AAD_WRAP_OPAQUE_PASSWORD_FILE_V1;
        let mut blob = wrap_with_key(&key, b"envelope", aad).expect("wrap");
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(unwrap_with_key(&key, &blob, aad).is_err());
    }

    #[test]
    fn test_unwrap_rejects_truncated_blob() {
        let key = [0x77_u8; 32];
        assert!(unwrap_with_key(&key, &[0_u8; 4], AAD_WRAP_ENCRYPTION_SALT_V1).is_err());
    }

    #[test]
    fn test_separate_salts_produce_different_keys() {
        let params = Argon2Params::default();
        let key1 = derive_key(b"same-pass", b"salt-for-profile1", &params).unwrap();
        let key2 = derive_key(b"same-pass", b"salt-for-profile2", &params).unwrap();
        assert_ne!(&*key1, &*key2);
    }

    const TEST_CREDENTIAL_IDENTIFIER: &[u8] = b"clipper:test:user";

    #[test]
    fn test_opaque_login_roundtrip() {
        let password = b"correct horse battery staple";
        let (server_setup, password_file) =
            opaque_register(password, TEST_CREDENTIAL_IDENTIFIER).unwrap();
        let (request, client_state) = opaque_client_login_start(password).unwrap();
        let (response, server_state) = opaque_server_login_start(
            &server_setup,
            Some(&password_file),
            &request,
            TEST_CREDENTIAL_IDENTIFIER,
        )
        .unwrap();
        let finish = opaque_client_login_finish(&client_state, password, &response).unwrap();
        let server_session_key =
            opaque_server_login_finish(&server_state, &finish.credential_finalization).unwrap();

        assert_eq!(finish.session_key, server_session_key);
    }

    #[test]
    fn test_opaque_export_key_matches_registration_and_login() {
        let password = b"correct horse battery staple";
        let server_setup = opaque_new_server_setup();
        let (registration_request, registration_state) =
            opaque_client_register_start(password).unwrap();
        let registration_response = opaque_server_register_start(
            &server_setup,
            &registration_request,
            TEST_CREDENTIAL_IDENTIFIER,
        )
        .unwrap();
        let registration_finish =
            opaque_client_register_finish(&registration_state, password, &registration_response)
                .unwrap();
        let password_file =
            opaque_server_register_finish(&registration_finish.registration_upload).unwrap();

        let (request, client_state) = opaque_client_login_start(password).unwrap();
        let (response, _server_state) = opaque_server_login_start(
            &server_setup,
            Some(&password_file),
            &request,
            TEST_CREDENTIAL_IDENTIFIER,
        )
        .unwrap();
        let login_finish = opaque_client_login_finish(&client_state, password, &response).unwrap();

        assert_eq!(
            registration_finish.export_key.as_slice(),
            login_finish.export_key.as_slice()
        );
        assert_eq!(
            *derive_data_key_from_opaque_export_key(&registration_finish.export_key),
            *derive_data_key_from_opaque_export_key(&login_finish.export_key)
        );
    }

    #[test]
    fn test_opaque_rejects_wrong_password() {
        let (server_setup, password_file) =
            opaque_register(b"correct password", TEST_CREDENTIAL_IDENTIFIER).unwrap();
        let (request, client_state) = opaque_client_login_start(b"wrong password").unwrap();
        let (response, _server_state) = opaque_server_login_start(
            &server_setup,
            Some(&password_file),
            &request,
            TEST_CREDENTIAL_IDENTIFIER,
        )
        .unwrap();

        assert!(opaque_client_login_finish(&client_state, b"wrong password", &response).is_err());
    }
}
