use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadCore, KeyInit, generic_array::typenum::Unsigned},
};
pub use clipper_api_types::{
    ARGON2_MAX_M_COST_KIB, ARGON2_MAX_P_COST, ARGON2_MAX_T_COST, ARGON2_MIN_M_COST_KIB,
    ARGON2_MIN_P_COST, ARGON2_MIN_T_COST, Argon2Params, DEVICE_LOGIN_PROOF_CHALLENGE_BYTES,
    DEVICE_LOGIN_PROOF_SIGNATURE_BYTES, DEVICE_LOGIN_PROOF_VERSION,
    DEVICE_SIGNING_PUBLIC_KEY_BYTES, DEVICE_SIGNING_SECRET_KEY_BYTES, DeviceLoginProofBodyV1,
    OBJECT_ENVELOPE_SIGNATURE_BYTES, OBJECT_ENVELOPE_VERSION, ObjectEnvelope, ObjectEnvelopeBody,
    ObjectEnvelopeOperation, ObjectEnvelopePayload, ObjectPayloadId,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
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
const OPAQUE_EXPORT_DEVICE_IDENTITY_WRAP_KEY_LABEL: &[u8] =
    b"clipper:opaque-export:device-identity-wrap-key:v1";

const ACCESS_KEY_HASH_PARAMS_DEFAULT: Argon2Params = Argon2Params {
    m_cost: 19 * 1024,
    t_cost: 2,
    p_cost: 1,
};

/// OPAQUE key-stretching cost, in kibibytes of memory.
pub const OPAQUE_KSF_M_COST_KIB: u32 = 19 * 1024;
/// OPAQUE key-stretching cost, in iterations.
pub const OPAQUE_KSF_T_COST: u32 = 2;
/// OPAQUE key-stretching degree of parallelism.
pub const OPAQUE_KSF_P_COST: u32 = 1;

struct ClipperOpaqueCipherSuite;

impl opaque_ke::CipherSuite for ClipperOpaqueCipherSuite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, sha2::Sha512>;
    type Ksf = opaque_ke::argon2::Argon2<'static>;
}

/// Argon2id parameters for the OPAQUE key stretching function.
///
/// These are the `argon2` crate's current defaults, written out so they are
/// pinned rather than inherited. The cost feeds `rwd`, so a dependency bump
/// that moved the default would silently change every derived key and break
/// logins. Changing these values requires re-registration.
fn opaque_ksf_params() -> Result<opaque_ke::argon2::Params, CryptoError> {
    // The output length stays unset. The KSF hashes the OPRF output, which is
    // 64 bytes under SHA-512; a pinned 32-byte length would reject it.
    opaque_ke::argon2::Params::new(
        OPAQUE_KSF_M_COST_KIB,
        OPAQUE_KSF_T_COST,
        OPAQUE_KSF_P_COST,
        None,
    )
    .map_err(|e| CryptoError::Kdf(e.to_string()))
}

/// Build the OPAQUE key stretching function. Registration and login must pass
/// the same one, or their `rwd` values differ.
fn opaque_ksf() -> Result<opaque_ke::argon2::Argon2<'static>, CryptoError> {
    Ok(opaque_ke::argon2::Argon2::new(
        opaque_ke::argon2::Algorithm::Argon2id,
        opaque_ke::argon2::Version::V0x13,
        opaque_ksf_params()?,
    ))
}

pub struct OpaqueRegistrationFinish {
    pub registration_upload: Vec<u8>,
    pub export_key: Zeroizing<Vec<u8>>,
}

pub struct OpaqueLoginFinish {
    pub credential_finalization: Vec<u8>,
    pub session_key: Zeroizing<Vec<u8>>,
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

/// Generate a random Ed25519 signing secret for this client device.
pub fn generate_device_signing_secret_key() -> [u8; DEVICE_SIGNING_SECRET_KEY_BYTES] {
    generate_bytes::<DEVICE_SIGNING_SECRET_KEY_BYTES>()
}

/// Derive the public Ed25519 verifying key for a device signing secret.
pub fn device_signing_public_key(
    secret_key: &[u8; DEVICE_SIGNING_SECRET_KEY_BYTES],
) -> [u8; DEVICE_SIGNING_PUBLIC_KEY_BYTES] {
    SigningKey::from_bytes(secret_key)
        .verifying_key()
        .to_bytes()
}

/// Canonical bytes signed by device keys for object provenance.
pub fn object_envelope_body_bytes(body: &ObjectEnvelopeBody) -> Result<Vec<u8>, CryptoError> {
    postcard::to_allocvec(body).map_err(|e| CryptoError::Signature(format!("postcard: {e}")))
}

/// SHA-256 of a revision's canonical body bytes, which its child carries as
/// `parent_hash`.
///
/// Taken over the body, not the signed envelope. The body is the canonical
/// form both the signature and the AAD are computed over, and Ed25519 is
/// deterministic, so hashing the signature would add a second representation
/// of the same fact.
pub fn object_envelope_parent_hash(
    parent: &ObjectEnvelopeBody,
) -> Result<[u8; SHA256_BYTES], CryptoError> {
    Ok(sha256(&object_envelope_body_bytes(parent)?))
}

/// Canonical bytes signed by device keys for login proof-of-possession.
pub fn device_login_proof_body_bytes(
    body: &DeviceLoginProofBodyV1,
) -> Result<Vec<u8>, CryptoError> {
    postcard::to_allocvec(body).map_err(|e| CryptoError::Signature(format!("postcard: {e}")))
}

/// Prefix the canonical body bytes with their message-type domain.
///
/// One device key signs both object envelopes and login proofs. The domain
/// makes the two signed messages disjoint, so a signature over one message
/// type can never be presented as a signature over the other.
fn domain_separated(domain: &[u8], body: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(domain.len() + body.len());
    message.extend_from_slice(domain);
    message.extend_from_slice(body);
    message
}

/// Sign a versioned object envelope body with the source device key.
pub fn sign_object_envelope_body(
    secret_key: &[u8; DEVICE_SIGNING_SECRET_KEY_BYTES],
    body: &ObjectEnvelopeBody,
) -> Result<Vec<u8>, CryptoError> {
    let signing_key = SigningKey::from_bytes(secret_key);
    let body = object_envelope_body_bytes(body)?;
    let message = domain_separated(SIGN_DOMAIN_OBJECT_ENVELOPE_V1, &body);
    Ok(signing_key.sign(&message).to_bytes().to_vec())
}

/// Sign a login proof body with the local device key.
pub fn sign_device_login_proof_body(
    secret_key: &[u8; DEVICE_SIGNING_SECRET_KEY_BYTES],
    body: &DeviceLoginProofBodyV1,
) -> Result<Vec<u8>, CryptoError> {
    let signing_key = SigningKey::from_bytes(secret_key);
    let body = device_login_proof_body_bytes(body)?;
    let message = domain_separated(SIGN_DOMAIN_DEVICE_LOGIN_PROOF_V1, &body);
    Ok(signing_key.sign(&message).to_bytes().to_vec())
}

/// Verify the source device signature over an object envelope.
pub fn verify_object_envelope_signature(
    public_key: &[u8],
    envelope: &ObjectEnvelope,
) -> Result<(), CryptoError> {
    let body = object_envelope_body_bytes(&envelope.body)?;
    let message = domain_separated(SIGN_DOMAIN_OBJECT_ENVELOPE_V1, &body);
    verify_device_signature(
        public_key,
        &message,
        &envelope.signature,
        "object signature",
    )
}

/// Verify a device login proof signature.
pub fn verify_device_login_proof_signature(
    public_key: &[u8],
    body: &DeviceLoginProofBodyV1,
    signature: &[u8],
) -> Result<(), CryptoError> {
    let body = device_login_proof_body_bytes(body)?;
    let message = domain_separated(SIGN_DOMAIN_DEVICE_LOGIN_PROOF_V1, &body);
    verify_device_signature(public_key, &message, signature, "device login proof")
}

fn verify_device_signature(
    public_key: &[u8],
    body: &[u8],
    signature: &[u8],
    context: &'static str,
) -> Result<(), CryptoError> {
    let public_key: &[u8; DEVICE_SIGNING_PUBLIC_KEY_BYTES] = public_key
        .try_into()
        .map_err(|_| CryptoError::Signature("invalid device public key length".into()))?;
    let signature: &[u8; DEVICE_LOGIN_PROOF_SIGNATURE_BYTES] = signature
        .try_into()
        .map_err(|_| CryptoError::Signature(format!("invalid {context} length")))?;
    let verifying_key = VerifyingKey::from_bytes(public_key)
        .map_err(|e| CryptoError::Signature(format!("device public key: {e}")))?;
    let signature = Signature::from_bytes(signature);
    verifying_key
        .verify(body, &signature)
        .map_err(|e| CryptoError::Signature(format!("{context}: {e}")))
}

/// Canonical AAD for object metadata encryption.
pub fn object_meta_aad(body: &ObjectEnvelopeBody) -> Result<Vec<u8>, CryptoError> {
    object_aad(body, None)
}

/// Canonical AAD for an object payload encryption.
pub fn object_payload_aad(
    body: &ObjectEnvelopeBody,
    payload_id: ObjectPayloadId,
) -> Result<Vec<u8>, CryptoError> {
    object_aad(body, Some(payload_id))
}

/// Project an envelope body onto the bytes that authenticate its ciphertexts.
///
/// The body is destructured exhaustively on purpose. Leaving a field out of
/// this projection is not a compile error anywhere else, and it is not a
/// decryption error either: the ciphertext still decrypts and the signature
/// still verifies. The only symptom is that a ciphertext becomes replayable
/// into any context differing by exactly the missing field. Adding a field to
/// `ObjectEnvelopeBody` must therefore break this line, so that binding it is
/// a decision rather than an omission. `mod object_aad` in the tests below is
/// the other half of that guard, asserting field by field which ones made it
/// in.
fn object_aad(
    body: &ObjectEnvelopeBody,
    payload_id: Option<ObjectPayloadId>,
) -> Result<Vec<u8>, CryptoError> {
    let ObjectEnvelopeBody {
        object_id,
        object_type,
        envelope_version,
        revision,
        parent_hash,
        source_device_id,
        created_at,
        operation,
        payloads,
        // Unbound, deliberately. A nonce is an AEAD input in its own right and
        // is already covered by the tag, so binding it to the AAD of the very
        // ciphertext it produced adds nothing.
        meta_nonce: _,
        // Unbound of necessity: this is a hash *of* the ciphertext that this
        // AAD is needed to produce, so binding it would be circular. The
        // signature over the body covers it instead.
        sha256_meta_ciphertext: _,
    } = body;

    let aad = ObjectAad {
        domain: match payload_id {
            Some(_) => "clipper:object-payload-aad:v1",
            None => "clipper:object-meta-aad:v1",
        },
        object_id: *object_id,
        object_type: *object_type,
        envelope_version: *envelope_version,
        // Bound so a ciphertext cannot be replayed at a different point in the
        // chain: without this, revision 3's sealed meta would open as revision
        // 9's, which is exactly the rollback that revision authentication must detect.
        revision: *revision,
        parent_hash: *parent_hash,
        source_device_id: *source_device_id,
        created_at: created_at.as_str(),
        operation: *operation,
        // The whole payload *set* is bound, not just the one being encrypted,
        // so a payload cannot be added to or dropped from a signed envelope
        // without invalidating every ciphertext in it.
        payload_ids: payloads
            .iter()
            .map(|payload| {
                // Same rule as above, one level down.
                let ObjectEnvelopePayload {
                    id,
                    // Unbound for the reasons given above: a nonce adds
                    // nothing, and a digest of the ciphertext is circular.
                    nonce: _,
                    sha256_ciphertext: _,
                    // Unbound: a lie about the size cannot survive the tag over
                    // the actual bytes, and the body signature covers it.
                    ciphertext_size: _,
                } = payload;
                *id
            })
            .collect(),
        payload_id,
    };
    postcard::to_allocvec(&aad).map_err(|e| CryptoError::Encrypt(format!("aad: {e}")))
}

#[derive(serde::Serialize)]
struct ObjectAad<'a> {
    domain: &'static str,
    object_id: clipper_api_types::ObjectId,
    object_type: clipper_api_types::ObjectKind,
    envelope_version: u64,
    revision: u64,
    parent_hash: Option<[u8; SHA256_BYTES]>,
    source_device_id: clipper_api_types::DeviceId,
    created_at: &'a str,
    operation: ObjectEnvelopeOperation,
    payload_ids: Vec<ObjectPayloadId>,
    payload_id: Option<ObjectPayloadId>,
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
    derive_opaque_export_key(export_key, OPAQUE_EXPORT_DATA_KEY_LABEL)
}

/// Derive the client-side wrapping key for the persisted device signing secret.
pub fn derive_device_identity_wrapping_key_from_opaque_export_key(
    export_key: &[u8],
) -> Zeroizing<[u8; 32]> {
    derive_opaque_export_key(export_key, OPAQUE_EXPORT_DEVICE_IDENTITY_WRAP_KEY_LABEL)
}

fn derive_opaque_export_key(export_key: &[u8], label: &[u8]) -> Zeroizing<[u8; 32]> {
    let hkdf = Hkdf::<Sha256>::new(None, export_key);
    let mut key = Zeroizing::new([0u8; 32]);
    hkdf.expand(label, key.as_mut())
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
    let mut rng = opaque_rand::rngs::OsRng;
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
///
/// The serialized state is secret. It holds both `r` and `M`, so anyone who
/// reads it recovers `H(pw) = r⁻¹ · M` and can run an offline dictionary
/// attack with no Argon2 cost per guess. It is returned in `Zeroizing` so the
/// copy is wiped when the caller drops it.
/// See `docs/opaque.md`.
pub fn opaque_client_register_start(
    passphrase: &[u8],
) -> Result<(Vec<u8>, Zeroizing<Vec<u8>>), CryptoError> {
    let mut rng = opaque_rand::rngs::OsRng;
    let start =
        opaque_ke::ClientRegistration::<ClipperOpaqueCipherSuite>::start(&mut rng, passphrase)
            .map_err(opaque_error)?;

    Ok((
        start.message.serialize().to_vec(),
        Zeroizing::new(start.state.serialize().to_vec()),
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
    let mut rng = opaque_rand::rngs::OsRng;
    let client_registration =
        opaque_ke::ClientRegistration::<ClipperOpaqueCipherSuite>::deserialize(client_state)
            .map_err(opaque_error)?;
    let response = opaque_ke::RegistrationResponse::<ClipperOpaqueCipherSuite>::deserialize(
        registration_response,
    )
    .map_err(opaque_error)?;
    let ksf = opaque_ksf()?;
    let finish = client_registration
        .finish(
            &mut rng,
            passphrase,
            response,
            opaque_ke::ClientRegistrationFinishParameters {
                ksf: Some(&ksf),
                ..Default::default()
            },
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
///
/// The serialized state is secret, for the same reason as in
/// `opaque_client_register_start`: it holds `r` and `M`, which together give
/// `H(pw)` and an Argon2-free offline dictionary oracle. It is returned in
/// `Zeroizing` so the copy is wiped when the caller drops it.
/// See `docs/opaque.md`.
pub fn opaque_client_login_start(
    passphrase: &[u8],
) -> Result<(Vec<u8>, Zeroizing<Vec<u8>>), CryptoError> {
    let mut rng = opaque_rand::rngs::OsRng;
    let start = opaque_ke::ClientLogin::<ClipperOpaqueCipherSuite>::start(&mut rng, passphrase)
        .map_err(opaque_error)?;

    Ok((
        start.message.serialize().to_vec(),
        Zeroizing::new(start.state.serialize().to_vec()),
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
    let mut rng = opaque_rand::rngs::OsRng;
    let client_login =
        opaque_ke::ClientLogin::<ClipperOpaqueCipherSuite>::deserialize(client_state)
            .map_err(opaque_error)?;
    let response =
        opaque_ke::CredentialResponse::<ClipperOpaqueCipherSuite>::deserialize(credential_response)
            .map_err(opaque_error)?;
    let ksf = opaque_ksf()?;
    let finish = client_login
        .finish(
            &mut rng,
            passphrase,
            response,
            opaque_ke::ClientLoginFinishParameters {
                ksf: Some(&ksf),
                ..Default::default()
            },
        )
        .map_err(opaque_error)?;

    Ok(OpaqueLoginFinish {
        credential_finalization: finish.message.serialize().to_vec(),
        session_key: Zeroizing::new(finish.session_key.to_vec()),
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
    let mut rng = opaque_rand::rngs::OsRng;
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
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
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

    Ok(Zeroizing::new(finish.session_key.to_vec()))
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
    #[error("signature error: {0}")]
    Signature(String),
    #[error("OPAQUE error: {0}")]
    Opaque(String),
}

// ── Device signature domains ──
//
// One Ed25519 device key signs two message types. Each signed message is the
// domain string followed by the canonical body bytes, so the two sets of
// signed messages cannot overlap. The strings differ from their first
// distinguishing byte, so no prefix of one is a prefix of the other.
pub const SIGN_DOMAIN_OBJECT_ENVELOPE_V1: &[u8] = b"clipper:object-envelope:v1";
pub const SIGN_DOMAIN_DEVICE_LOGIN_PROOF_V1: &[u8] = b"clipper:device-login-proof:v1";

// ── Associated data constants ──
pub const AAD_CLIPBOARD_V1: &[u8] = b"clipper:clipboard:v1";

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
pub const AAD_WRAP_DEVICE_SIGNING_SECRET_V1: &[u8] = b"clipper:wrap:device-signing-secret:v1";

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
        assert!(decrypt(&key, &nonce, &ct, b"clipper:test:wrong-aad:v1").is_err());
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
    fn test_device_identity_wrap_key_is_stable_and_separated() {
        let export_key = b"opaque export key material";
        let key1 = derive_device_identity_wrapping_key_from_opaque_export_key(export_key);
        let key2 = derive_device_identity_wrapping_key_from_opaque_export_key(export_key);
        let key3 = derive_device_identity_wrapping_key_from_opaque_export_key(
            b"different export key material",
        );
        let data_key = derive_data_key_from_opaque_export_key(export_key);

        assert_eq!(*key1, *key2);
        assert_ne!(*key1, *key3);
        assert_ne!(*key1, *data_key);
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

    /// The OPAQUE key stretching cost is part of the credential. Registration
    /// and login must use the same values, and changing them invalidates every
    /// stored password file, so they are asserted rather than inherited from a
    /// dependency default.
    #[test]
    fn test_opaque_ksf_parameters_are_pinned() {
        assert_eq!(OPAQUE_KSF_M_COST_KIB, 19 * 1024);
        assert_eq!(OPAQUE_KSF_T_COST, 2);
        assert_eq!(OPAQUE_KSF_P_COST, 1);

        let params = opaque_ksf_params().expect("ksf params");
        assert_eq!(params.m_cost(), OPAQUE_KSF_M_COST_KIB);
        assert_eq!(params.t_cost(), OPAQUE_KSF_T_COST);
        assert_eq!(params.p_cost(), OPAQUE_KSF_P_COST);
        // The KSF hashes the 64-byte OPRF output, so no output length is set.
        assert_eq!(params.output_len(), None);
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

        assert_eq!(*finish.session_key, *server_session_key);
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

/// Guards for the envelope AAD projection.
///
/// `object_aad` decides which parts of an envelope body authenticate its
/// ciphertexts. That decision has no other enforcement: a field left out still
/// encrypts, still decrypts, and still verifies, and the only consequence is
/// that a ciphertext can be lifted into a context differing by exactly the
/// missing field. Nothing turns red.
///
/// So it is made to turn red here. Two tables below name every field and which
/// side of the line it falls on, and the tests exercise both directions —
/// bound fields must break decryption, unbound fields must leave the AAD
/// byte-identical. Between them and the exhaustive destructure in
/// `object_aad`, adding a field to the envelope cannot silently skip the
/// question.
#[cfg(test)]
mod object_aad {
    use std::str::FromStr;

    use clipper_api_types::{DeviceId, ObjectId, ObjectKind};

    use super::*;

    const KEY: [u8; 32] = [7u8; 32];

    fn uuid_str(tag: u64) -> String {
        format!("00000000-0000-4000-8000-{tag:012x}")
    }

    fn payload(tag: u64) -> ObjectEnvelopePayload {
        ObjectEnvelopePayload {
            id: ObjectPayloadId::from_str(&uuid_str(tag)).expect("payload id"),
            nonce: vec![tag as u8; XCHACHA20_NONCE_BYTES],
            ciphertext_size: 64,
            sha256_ciphertext: vec![tag as u8; SHA256_BYTES],
        }
    }

    /// Written as a full struct literal, without `..`, for the same reason
    /// `object_aad` destructures: a new field on the body has to be given a
    /// value here, which lands whoever added it in this file, in front of the
    /// two tables below.
    fn body() -> ObjectEnvelopeBody {
        ObjectEnvelopeBody {
            object_id: ObjectId::from_str(&uuid_str(1)).expect("object id"),
            object_type: ObjectKind::Schedule,
            envelope_version: OBJECT_ENVELOPE_VERSION,
            revision: 4,
            parent_hash: Some([5; SHA256_BYTES]),
            source_device_id: DeviceId::from_str(&uuid_str(2)).expect("device id"),
            created_at: "2026-09-08T10:00:00Z".to_string(),
            operation: ObjectEnvelopeOperation::Revise,
            meta_nonce: vec![3; XCHACHA20_NONCE_BYTES],
            sha256_meta_ciphertext: vec![4; SHA256_BYTES],
            payloads: vec![payload(0x10), payload(0x11)],
        }
    }

    type Mutation = (&'static str, fn(&mut ObjectEnvelopeBody));

    /// Fields the AAD must bind, each with a change to it. A ciphertext sealed
    /// under the original body must not open under any of these.
    fn bound_fields() -> Vec<Mutation> {
        vec![
            ("object_id", |body| {
                body.object_id = ObjectId::from_str(&uuid_str(0xdead)).expect("object id");
            }),
            ("object_type", |body| body.object_type = ObjectKind::File),
            ("envelope_version", |body| body.envelope_version += 1),
            ("source_device_id", |body| {
                body.source_device_id = DeviceId::from_str(&uuid_str(0xbeef)).expect("device id");
            }),
            ("created_at", |body| {
                body.created_at = "2026-09-08T10:00:01Z".to_string();
            }),
            ("revision", |body| body.revision += 1),
            ("parent_hash", |body| {
                body.parent_hash = Some([0xdd; SHA256_BYTES]);
            }),
            ("parent_hash presence", |body| {
                body.revision = 1;
                body.parent_hash = None;
                body.operation = ObjectEnvelopeOperation::Create;
            }),
            ("operation", |body| {
                body.operation = ObjectEnvelopeOperation::Delete;
            }),
            ("payloads[..].id", |body| {
                body.payloads[0].id =
                    ObjectPayloadId::from_str(&uuid_str(0xfeed)).expect("payload id");
            }),
            ("payloads.len()", |body| body.payloads.push(payload(0x12))),
        ]
    }

    /// Fields deliberately left out of the projection; the reasoning is on
    /// `object_aad`. Changing one must leave the AAD byte-identical. A
    /// failure here means someone bound a field — possibly correctly, but it
    /// should be a decision, and the comment explaining it belongs next to the
    /// destructure.
    fn unbound_fields() -> Vec<Mutation> {
        vec![
            ("meta_nonce", |body| {
                body.meta_nonce = vec![0xaa; XCHACHA20_NONCE_BYTES];
            }),
            ("sha256_meta_ciphertext", |body| {
                body.sha256_meta_ciphertext = vec![0xaa; SHA256_BYTES];
            }),
            ("payloads[..].nonce", |body| {
                body.payloads[0].nonce = vec![0xaa; XCHACHA20_NONCE_BYTES];
            }),
            ("payloads[..].ciphertext_size", |body| {
                body.payloads[0].ciphertext_size += 1;
            }),
            ("payloads[..].sha256_ciphertext", |body| {
                body.payloads[0].sha256_ciphertext = vec![0xaa; SHA256_BYTES];
            }),
        ]
    }

    #[test]
    fn meta_ciphertext_will_not_open_under_a_changed_bound_field() {
        let aad = object_meta_aad(&body()).expect("meta aad");
        let (nonce, ciphertext) = encrypt(&KEY, b"encrypted meta", &aad).expect("encrypt");

        for (field, mutate) in bound_fields() {
            let mut altered = body();
            mutate(&mut altered);
            let altered_aad = object_meta_aad(&altered).expect("meta aad");

            assert_ne!(aad, altered_aad, "{field} is missing from the meta AAD");
            assert!(
                decrypt(&KEY, &nonce, &ciphertext, &altered_aad).is_err(),
                "meta ciphertext opened after {field} changed, so it is replayable across it",
            );
        }
    }

    #[test]
    fn payload_ciphertext_will_not_open_under_a_changed_bound_field() {
        let target = body().payloads[0].id;
        let aad = object_payload_aad(&body(), target).expect("payload aad");
        let (nonce, ciphertext) = encrypt(&KEY, b"encrypted payload", &aad).expect("encrypt");

        for (field, mutate) in bound_fields() {
            let mut altered = body();
            mutate(&mut altered);
            // Mutating the id under test moves the target too; the point is
            // that the surrounding envelope changed, so re-derive against the
            // payload the altered body actually has in that slot.
            let altered_target = altered.payloads[0].id;
            let altered_aad = object_payload_aad(&altered, altered_target).expect("payload aad");

            assert_ne!(aad, altered_aad, "{field} is missing from the payload AAD");
            assert!(
                decrypt(&KEY, &nonce, &ciphertext, &altered_aad).is_err(),
                "payload ciphertext opened after {field} changed, so it is replayable across it",
            );
        }
    }

    #[test]
    fn unbound_fields_leave_the_aad_byte_identical() {
        let target = body().payloads[0].id;
        let meta = object_meta_aad(&body()).expect("meta aad");
        let payload = object_payload_aad(&body(), target).expect("payload aad");

        for (field, mutate) in unbound_fields() {
            let mut altered = body();
            mutate(&mut altered);

            assert_eq!(
                meta,
                object_meta_aad(&altered).expect("meta aad"),
                "{field} is now bound in the meta AAD; see the destructure in object_aad",
            );
            assert_eq!(
                payload,
                object_payload_aad(&altered, target).expect("payload aad"),
                "{field} is now bound in the payload AAD; see the destructure in object_aad",
            );
        }
    }

    #[test]
    fn the_body_signature_covers_what_the_aad_omits() {
        // The two mechanisms split the work: the AAD binds a ciphertext to its
        // context, the signature covers the whole body. An unbound field is
        // not an unprotected one, and this is what says so.
        let secret = [9u8; DEVICE_SIGNING_SECRET_KEY_BYTES];
        let public = device_signing_public_key(&secret);
        let signature = sign_object_envelope_body(&secret, &body()).expect("sign");

        verify_object_envelope_signature(
            &public,
            &ObjectEnvelope {
                body: body(),
                signature: signature.clone(),
            },
        )
        .expect("the unmodified body verifies");

        for (field, mutate) in unbound_fields() {
            let mut altered = body();
            mutate(&mut altered);
            assert!(
                verify_object_envelope_signature(
                    &public,
                    &ObjectEnvelope {
                        body: altered,
                        signature: signature.clone(),
                    },
                )
                .is_err(),
                "{field} is neither bound in the AAD nor covered by the signature",
            );
        }
    }

    #[test]
    fn meta_and_payload_aads_are_domain_separated() {
        let body = body();
        let meta = object_meta_aad(&body).expect("meta aad");
        let payload = object_payload_aad(&body, body.payloads[0].id).expect("payload aad");
        assert_ne!(meta, payload);

        let (nonce, ciphertext) = encrypt(&KEY, b"encrypted meta", &meta).expect("encrypt");
        assert!(
            decrypt(&KEY, &nonce, &ciphertext, &payload).is_err(),
            "encrypted meta opened as a payload",
        );
    }

    #[test]
    fn a_payload_ciphertext_cannot_be_moved_to_a_sibling() {
        let body = body();
        let first = object_payload_aad(&body, body.payloads[0].id).expect("payload aad");
        let second = object_payload_aad(&body, body.payloads[1].id).expect("payload aad");

        let (nonce, ciphertext) = encrypt(&KEY, b"payload one", &first).expect("encrypt");
        assert!(
            decrypt(&KEY, &nonce, &ciphertext, &second).is_err(),
            "one payload's ciphertext opened in another payload's slot",
        );
    }

    /// Every `operation` variant must be reachable from a bound-field mutation,
    /// or a new one could be added without anyone checking it is bound. The
    /// match is the trigger: a fourth variant fails to compile here.
    #[test]
    fn every_operation_variant_is_covered_by_a_mutation() {
        let covered: Vec<ObjectEnvelopeOperation> = std::iter::once(body().operation)
            .chain(bound_fields().into_iter().map(|(_, mutate)| {
                let mut altered = body();
                mutate(&mut altered);
                altered.operation
            }))
            .collect();

        for variant in [
            ObjectEnvelopeOperation::Create,
            ObjectEnvelopeOperation::Revise,
            ObjectEnvelopeOperation::Delete,
        ] {
            match variant {
                ObjectEnvelopeOperation::Create
                | ObjectEnvelopeOperation::Revise
                | ObjectEnvelopeOperation::Delete => {}
            }
            assert!(
                covered.contains(&variant),
                "no bound-field mutation produces {variant:?}, so it is never checked",
            );
        }
    }

    /// The chain hash has one definition, and it has to be the canonical body
    /// bytes — the same form the signature covers. A parent that differs in any
    /// field must hash differently, or a swapped parent would go unnoticed.
    #[test]
    fn parent_hash_changes_with_every_field_of_the_parent() {
        let baseline = object_envelope_parent_hash(&body()).expect("parent hash");

        for (field, mutate) in bound_fields().into_iter().chain(unbound_fields()) {
            let mut altered = body();
            mutate(&mut altered);
            assert_ne!(
                baseline,
                object_envelope_parent_hash(&altered).expect("parent hash"),
                "a parent differing in {field} hashes the same, so it can be swapped in",
            );
        }
    }
}

/// Guards for device signature domain separation.
///
/// One device key signs object envelopes and login proofs. Signing raw
/// canonical bytes would leave the two message spaces adjacent; the domain
/// prefix keeps them disjoint. These tests hold that line: a signature made
/// under the wrong domain, or under no domain at all, must not verify.
#[cfg(test)]
mod signature_domains {
    use std::str::FromStr;

    use clipper_api_types::{DeviceId, ObjectId, ObjectKind};

    use super::*;

    const SECRET: [u8; DEVICE_SIGNING_SECRET_KEY_BYTES] = [9; DEVICE_SIGNING_SECRET_KEY_BYTES];

    fn uuid_str(tag: u64) -> String {
        format!("00000000-0000-4000-8000-{tag:012x}")
    }

    fn envelope_body() -> ObjectEnvelopeBody {
        ObjectEnvelopeBody {
            object_id: ObjectId::from_str(&uuid_str(1)).expect("object id"),
            object_type: ObjectKind::Clipboard,
            envelope_version: OBJECT_ENVELOPE_VERSION,
            revision: 1,
            parent_hash: None,
            source_device_id: DeviceId::from_str(&uuid_str(2)).expect("device id"),
            created_at: "2026-09-08T10:00:00Z".to_string(),
            operation: ObjectEnvelopeOperation::Create,
            meta_nonce: vec![3; XCHACHA20_NONCE_BYTES],
            sha256_meta_ciphertext: vec![4; SHA256_BYTES],
            payloads: Vec::new(),
        }
    }

    fn proof_body() -> DeviceLoginProofBodyV1 {
        DeviceLoginProofBodyV1 {
            version: DEVICE_LOGIN_PROOF_VERSION,
            challenge_id: "challenge".to_string(),
            challenge: vec![5; DEVICE_LOGIN_PROOF_CHALLENGE_BYTES],
            username: "user".to_string(),
            device_id: DeviceId::from_str(&uuid_str(2)).expect("device id"),
            device_signing_public_key: device_signing_public_key(&SECRET).to_vec(),
        }
    }

    /// Sign arbitrary bytes with the test device key, bypassing the helpers.
    fn sign_raw(message: &[u8]) -> Vec<u8> {
        SigningKey::from_bytes(&SECRET)
            .sign(message)
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn an_envelope_signature_needs_the_envelope_domain() {
        let public = device_signing_public_key(&SECRET);
        let body = envelope_body();
        let canon = object_envelope_body_bytes(&body).expect("canonical body");

        let verify = |signature: Vec<u8>| {
            verify_object_envelope_signature(
                &public,
                &ObjectEnvelope {
                    body: body.clone(),
                    signature,
                },
            )
        };

        verify(sign_object_envelope_body(&SECRET, &body).expect("sign")).expect("own domain");
        assert!(
            verify(sign_raw(&domain_separated(
                SIGN_DOMAIN_DEVICE_LOGIN_PROOF_V1,
                &canon
            )))
            .is_err(),
            "the login-proof domain verified as an envelope",
        );
        assert!(
            verify(sign_raw(&canon)).is_err(),
            "undomained canonical bytes verified as an envelope",
        );
    }

    #[test]
    fn a_login_proof_signature_needs_the_login_proof_domain() {
        let public = device_signing_public_key(&SECRET);
        let body = proof_body();
        let canon = device_login_proof_body_bytes(&body).expect("canonical body");

        verify_device_login_proof_signature(
            &public,
            &body,
            &sign_device_login_proof_body(&SECRET, &body).expect("sign"),
        )
        .expect("own domain");
        assert!(
            verify_device_login_proof_signature(
                &public,
                &body,
                &sign_raw(&domain_separated(SIGN_DOMAIN_OBJECT_ENVELOPE_V1, &canon)),
            )
            .is_err(),
            "the envelope domain verified as a login proof",
        );
        assert!(
            verify_device_login_proof_signature(&public, &body, &sign_raw(&canon)).is_err(),
            "undomained canonical bytes verified as a login proof",
        );
    }

    /// The signed bytes are the domain followed by the canonical body, and the
    /// canonical body itself is unchanged — the parent hash is taken over it.
    #[test]
    fn the_domain_is_a_prefix_of_the_canonical_body() {
        let canon = object_envelope_body_bytes(&envelope_body()).expect("canonical body");
        let message = domain_separated(SIGN_DOMAIN_OBJECT_ENVELOPE_V1, &canon);

        assert!(message.starts_with(SIGN_DOMAIN_OBJECT_ENVELOPE_V1));
        assert_eq!(&message[SIGN_DOMAIN_OBJECT_ENVELOPE_V1.len()..], &canon[..]);
        assert!(!SIGN_DOMAIN_OBJECT_ENVELOPE_V1.starts_with(SIGN_DOMAIN_DEVICE_LOGIN_PROOF_V1));
        assert!(!SIGN_DOMAIN_DEVICE_LOGIN_PROOF_V1.starts_with(SIGN_DOMAIN_OBJECT_ENVELOPE_V1));
    }
}
