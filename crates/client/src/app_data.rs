pub(crate) mod collections;
pub(crate) mod tables;

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI64, Ordering},
};

use clipper_core::{
    crypto::app_data::{
        AppDataValueEnvelope, app_data_row_key, decrypt_app_data_value,
        derive_app_data_row_key_key, derive_app_data_value_key, encrypt_app_data_value,
        verify_app_data_change_signature,
    },
    models::{AppDataChange, AppDataRow},
};
use tokio::sync::{Mutex, Notify};
use zeroize::Zeroizing;

use self::tables::AppDataTables;
use crate::api_client::ClientError;

pub(crate) const MAX_ENVELOPE_BYTES: usize = 64 * 1024;

pub(crate) struct AppDataKeys {
    row_key_key: Zeroizing<[u8; 32]>,
    value_key: Zeroizing<[u8; 32]>,
}

impl AppDataKeys {
    pub fn derive(data_key: &[u8; 32]) -> Self {
        Self {
            row_key_key: derive_app_data_row_key_key(data_key),
            value_key: derive_app_data_value_key(data_key),
        }
    }

    pub fn row_key(&self, collection: &str, row_id: uuid::Uuid) -> [u8; 32] {
        app_data_row_key(&self.row_key_key, collection, row_id.into())
    }

    pub fn seal(
        &self,
        row_key: &[u8; 32],
        revision: u64,
        envelope: &AppDataValueEnvelope,
    ) -> Result<(Vec<u8>, Vec<u8>), ClientError> {
        let plaintext = Zeroizing::new(serde_json::to_vec(envelope).map_err(|error| {
            ClientError::InvalidArgument(format!("app-data value does not encode: {error}"))
        })?);
        if plaintext.len() > MAX_ENVELOPE_BYTES {
            return Err(ClientError::PayloadTooLarge {
                size: plaintext.len() as i64,
                limit: MAX_ENVELOPE_BYTES as i64,
            });
        }
        Ok(encrypt_app_data_value(
            &self.value_key,
            row_key,
            revision,
            &plaintext,
        )?)
    }

    pub fn open(
        &self,
        row_key: &[u8; 32],
        revision: u64,
        deleted: bool,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<AppDataValueEnvelope, ClientError> {
        let plaintext = Zeroizing::new(decrypt_app_data_value(
            &self.value_key,
            row_key,
            revision,
            nonce,
            ciphertext,
        )?);
        let envelope: AppDataValueEnvelope = serde_json::from_slice(&plaintext)
            .map_err(|error| rejected(format!("the envelope does not parse: {error}")))?;
        if self.row_key(&envelope.collection, envelope.row_id.into_uuid()) != *row_key {
            return Err(rejected("the envelope names a different row"));
        }
        if envelope.deleted != deleted || envelope.value.is_some() == deleted {
            return Err(rejected("the envelope disagrees with the delete marker"));
        }
        Ok(envelope)
    }

    pub fn verified(&self, row: &AppDataRow) -> Result<AppDataValueEnvelope, ClientError> {
        let row_key: [u8; 32] = row
            .row_key
            .as_slice()
            .try_into()
            .map_err(|_| rejected("the row key is not 32 bytes"))?;
        if row.revision == 0 || row.revision > i64::MAX as u64 {
            return Err(rejected("the revision is out of range"));
        }
        if let Some(public_key) = &row.device_signing_public_key {
            let device_id = row
                .device_id
                .ok_or_else(|| rejected("the row has a signing key but no device"))?;
            let change = AppDataChange {
                row_key: row.row_key.clone(),
                revision: row.revision,
                replaces_revision: row.revision - 1,
                deleted: row.deleted,
                nonce: row.nonce.clone(),
                ciphertext: row.ciphertext.clone(),
                device_id,
                signature: row.signature.clone(),
            };
            verify_app_data_change_signature(public_key, &change)?;
        }
        self.open(
            &row_key,
            row.revision,
            row.deleted,
            &row.nonce,
            &row.ciphertext,
        )
    }
}

fn rejected(reason: impl Into<String>) -> ClientError {
    ClientError::UnexpectedResponse(format!("app-data row rejected: {}", reason.into()))
}

pub(crate) struct AppDataSession {
    pub epoch: u64,
    pub keys: AppDataKeys,
    pub tables: AppDataTables,
    pub pull_error: Option<String>,
    pub push_error: Option<String>,
}

#[derive(Default)]
pub(crate) struct AppDataHandle {
    pub session: Arc<Mutex<Option<AppDataSession>>>,
    pub wake: Notify,
    pull_requested: AtomicBool,
    last_applied: AtomicI64,
}

impl AppDataHandle {
    pub fn request_push(&self) {
        self.wake.notify_one();
    }

    pub fn request_pull(&self) {
        self.mark_pull_needed();
        self.wake.notify_one();
    }

    pub fn mark_pull_needed(&self) {
        self.pull_requested.store(true, Ordering::SeqCst);
    }

    pub fn announced(&self, sequence: i64) {
        if sequence > self.last_applied.load(Ordering::SeqCst) {
            self.request_pull();
        }
    }

    pub fn take_pull_request(&self) -> bool {
        self.pull_requested.swap(false, Ordering::SeqCst)
    }

    pub fn set_last_applied(&self, sequence: i64) {
        self.last_applied.store(sequence, Ordering::SeqCst);
    }

    pub async fn close(&self) {
        *self.session.lock().await = None;
        self.pull_requested.store(false, Ordering::SeqCst);
        self.last_applied.store(0, Ordering::SeqCst);
    }
}
