use clipper_core::{
    crypto::{self, CryptoError},
    models::{AppDocumentMeta, ObjectEnvelopeBody, ObjectPayloadId},
};
use serde_json::Value;
use zeroize::Zeroizing;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppDocument {
    pub id: String,
    pub collection: String,
    pub revision: u64,
    pub written_at: String,
    pub value: Value,
}

pub(crate) fn encrypt_meta(
    meta: &AppDocumentMeta,
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBody,
) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let plaintext = Zeroizing::new(
        serde_json::to_vec(meta).map_err(|error| CryptoError::Encrypt(format!("json: {error}")))?,
    );
    let aad = crypto::object_meta_aad(envelope_body)?;
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, &plaintext, &aad)?;
    Ok((nonce.to_vec(), ciphertext))
}

pub(crate) fn decrypt_meta(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBody,
) -> Result<AppDocumentMeta, CryptoError> {
    let aad = crypto::object_meta_aad(envelope_body)?;
    let plaintext = Zeroizing::new(crypto::decrypt(encryption_key, nonce, ciphertext, &aad)?);
    let meta: AppDocumentMeta = serde_json::from_slice(&plaintext)
        .map_err(|error| CryptoError::Decrypt(format!("json: {error}")))?;
    if meta.document_id != envelope_body.object_id {
        return Err(CryptoError::Decrypt(
            "the document metadata names a different object".into(),
        ));
    }
    Ok(meta)
}

pub(crate) fn encrypt_value(
    value: &Value,
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBody,
    payload_id: ObjectPayloadId,
) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let plaintext = Zeroizing::new(
        serde_json::to_vec(value)
            .map_err(|error| CryptoError::Encrypt(format!("json: {error}")))?,
    );
    let aad = crypto::object_payload_aad(envelope_body, payload_id)?;
    let (nonce, ciphertext) = crypto::encrypt(encryption_key, &plaintext, &aad)?;
    Ok((nonce.to_vec(), ciphertext))
}

pub(crate) fn decrypt_value(
    nonce: &[u8],
    ciphertext: &[u8],
    encryption_key: &[u8; 32],
    envelope_body: &ObjectEnvelopeBody,
    payload_id: ObjectPayloadId,
) -> Result<Value, CryptoError> {
    let aad = crypto::object_payload_aad(envelope_body, payload_id)?;
    let plaintext = Zeroizing::new(crypto::decrypt(encryption_key, nonce, ciphertext, &aad)?);
    serde_json::from_slice(&plaintext)
        .map_err(|error| CryptoError::Decrypt(format!("json: {error}")))
}
