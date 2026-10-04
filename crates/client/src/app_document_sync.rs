use clipper_app_types::AppDocumentRevision;
use serde_json::Value;

use super::*;
use crate::{
    app_data::{
        collections::{self, Collection, Storage},
        documents::{self, AppDocument},
    },
    local_store::{HeldAppDocument, LocalAppDocumentRecord},
};

const MAX_DOCUMENT_CIPHERTEXT_BYTES: i64 = 256 * 1024;
const HISTORY_FETCH_CONCURRENCY: usize = 8;

impl SyncEngine {
    pub(super) async fn write_app_document(
        &self,
        collection_name: &str,
        document_id: Option<&str>,
        write: AppDataWrite,
    ) -> Result<String, ClientError> {
        let collection = document_collection(collection_name)?;
        let requested = document_id.map(parse_document_id).transpose()?;
        let value = match write {
            AppDataWrite::Value(value) => Some(value),
            AppDataWrite::Delete => None,
        };
        let object_id = match (requested, &value) {
            (Some(id), _) => id,
            (None, Some(_)) => uuid::Uuid::now_v7(),
            (None, None) => {
                return Err(ClientError::InvalidArgument(
                    "a delete needs a document id".into(),
                ));
            }
        };
        let object_id_text = object_id.to_string();
        let held = self.local_store.held_app_document(&object_id_text).await?;
        let head = match &held {
            Some(HeldAppDocument::Present {
                collection: held,
                head,
            }) if held == collection.name => Some(*head),
            Some(HeldAppDocument::Present {
                collection: held, ..
            }) => {
                return Err(ClientError::InvalidArgument(format!(
                    "document {object_id} belongs to {held}, not {}",
                    collection.name
                )));
            }
            Some(HeldAppDocument::Deleted) => {
                return Err(ClientError::InvalidArgument(format!(
                    "document {object_id} of {} was deleted",
                    collection.name
                )));
            }
            None => None,
        };
        let written = match (value, head) {
            (Some(value), head) => {
                let placement = head.map_or(EnvelopePlacement::Create, EnvelopePlacement::Revise);
                self.write_document_revision(collection, &object_id_text, value, placement)
                    .await
            }
            (None, Some(head)) => self.delete_app_document(&object_id_text, head).await,
            (None, None) => Err(ClientError::ItemNotFound {
                id: object_id_text.clone(),
            }),
        };
        match written {
            Ok(()) => Ok(object_id_text),
            Err(error) => Err(self
                .document_write_error(collection, &object_id_text, held, error)
                .await),
        }
    }

    async fn document_write_error(
        &self,
        collection: &Collection,
        object_id: &str,
        held_before: Option<HeldAppDocument>,
        error: ClientError,
    ) -> ClientError {
        let overtaken = match &error {
            ClientError::Api { status: 409, .. } => true,
            error if ambiguous_write_error(error) => self
                .local_store
                .held_app_document(object_id)
                .await
                .is_ok_and(|held| held != held_before),
            _ => false,
        };
        if overtaken {
            return ClientError::InvalidArgument(format!(
                "{} document {object_id} was changed by another write; read it again and reapply the change",
                collection.name
            ));
        }
        match error {
            ClientError::Http(error) if error.is_connect() => ClientError::Offline,
            error => error,
        }
    }

    async fn delete_app_document(
        &self,
        object_id: &str,
        head: LocalHead,
    ) -> Result<(), ClientError> {
        let (deleted_seq, tombstone) = self
            .write_tombstone_at(object_id, ObjectKind::AppDocument, Some(head))
            .await?;
        let visible = self
            .local_store
            .apply_local_tombstone(
                ObjectKind::AppDocument,
                object_id,
                deleted_seq,
                &tombstone,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?;
        self.publish_visible_state(visible).await;
        Ok(())
    }

    async fn write_document_revision(
        &self,
        collection: &Collection,
        object_id: &str,
        value: Value,
        placement: EnvelopePlacement,
    ) -> Result<(), ClientError> {
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let SessionCredentials {
            api,
            encryption_key,
            device_id,
            device_id_typed,
            signing_key,
        } = self.credentials_for_session(epoch).await?;
        let object_id_typed: ObjectId = parse_document_id(object_id)?.into();
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        let payload_uuid = uuid::Uuid::now_v7();
        let payload_id_typed: ObjectPayloadId = payload_uuid.into();
        let aad_body = object_envelope_body_for_aad(
            object_id_typed,
            ObjectKind::AppDocument,
            placement,
            device_id_typed,
            created_at.clone(),
            vec![payload_id_typed],
        );
        let meta = AppDocumentMeta {
            collection: collection.name.to_string(),
            document_id: object_id_typed,
            schema_version: collection.schema_version,
        };
        let (meta_nonce, meta_ciphertext) =
            documents::encrypt_meta(&meta, &encryption_key, &aad_body)?;
        let (payload_nonce, encrypted_payload) =
            documents::encrypt_value(&value, &encryption_key, &aad_body, payload_id_typed)?;
        let payload_size = encrypted_payload.len() as i64;
        if payload_size > MAX_DOCUMENT_CIPHERTEXT_BYTES {
            return Err(ClientError::PayloadTooLarge {
                size: payload_size,
                limit: MAX_DOCUMENT_CIPHERTEXT_BYTES,
            });
        }
        let payload_hash = crypto::sha256(&encrypted_payload).to_vec();
        let envelope_body = object_envelope_body(
            object_id_typed,
            ObjectKind::AppDocument,
            placement,
            device_id_typed,
            created_at.clone(),
            meta_nonce.clone(),
            crypto::sha256(&meta_ciphertext).to_vec(),
            vec![ObjectEnvelopePayload {
                id: payload_id_typed,
                nonce: payload_nonce.clone(),
                ciphertext_size: payload_size,
                sha256_ciphertext: payload_hash.clone(),
            }],
        );
        let envelope = ObjectEnvelope {
            signature: crypto::sign_object_envelope_body(&signing_key, &envelope_body)?,
            body: envelope_body,
        };
        let payloads = vec![ObjectPayloadInit {
            id: payload_id_typed,
            nonce: payload_nonce,
            ciphertext_size: payload_size,
            sha256_ciphertext: payload_hash.clone(),
            inline_ciphertext: inline_ciphertext(&encrypted_payload),
        }];
        if !self.session_is_current(epoch) {
            return Err(ClientError::NotAuthenticated);
        }
        let (encrypted_object, started) = match placement {
            EnvelopePlacement::Create => {
                let request = ObjectInitRequest {
                    id: object_id_typed,
                    kind: ObjectKind::AppDocument,
                    meta_nonce,
                    meta_ciphertext,
                    payloads,
                    envelope,
                };
                (
                    encrypted_object_from_init(&request),
                    api.object_init(&request).await,
                )
            }
            EnvelopePlacement::Revise(_) | EnvelopePlacement::Delete(_) => {
                let request = ObjectReviseRequest {
                    meta_nonce,
                    meta_ciphertext,
                    payloads,
                    envelope,
                };
                (
                    encrypted_object_from_revise(&request),
                    api.object_revise(object_id, &request).await,
                )
            }
        };
        let encrypted = EncryptedInlineObject {
            object: encrypted_object,
            payload_ciphertext: encrypted_payload.clone(),
        };
        let document = LocalAppDocumentRecord {
            collection: collection.name.to_string(),
            revision: placement.revision(),
            value,
        };
        let published = match started {
            Ok(response) => {
                Self::finish_single_payload_object(
                    &api,
                    object_id,
                    &payload_uuid.to_string(),
                    response,
                    encrypted_payload,
                    payload_size,
                    payload_hash,
                )
                .await
            }
            Err(error) => Err(error),
        };
        let created_seq = match published {
            Ok(seq) => seq,
            Err(error) => {
                if ambiguous_write_error(&error)
                    || matches!(error, ClientError::Api { status: 409, .. })
                {
                    match self
                        .recover_document_write(epoch, &api, &encryption_key, &encrypted, &document)
                        .await
                    {
                        Ok(true) => return Ok(()),
                        Ok(false) => {}
                        Err(recovery_error) => {
                            warn!(
                                object_id,
                                "Failed to recover a document write: {recovery_error}"
                            );
                        }
                    }
                }
                return Err(error);
            }
        };
        let _session = self.hold_session_for_write(epoch).await?;
        let persisted = self
            .local_store
            .persist_local_app_document_present_encrypted(
                StoredObjectIdentity {
                    object_id,
                    created_at: &created_at,
                    source_device_id: &device_id,
                },
                document,
                &encrypted,
                created_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await;
        self.publish_accepted_write(object_id, placement.revision(), persisted)
            .await
    }

    async fn recover_document_write(
        &self,
        epoch: u64,
        api: &ApiClient,
        encryption_key: &[u8; 32],
        sent: &EncryptedInlineObject,
        document: &LocalAppDocumentRecord,
    ) -> Result<bool, ClientError> {
        let sent_body = &sent.object.envelope.body;
        let object_id = sent_body.object_id.to_string();
        let item = match api.get_object_head(&object_id).await {
            Ok(item) => item,
            Err(error) if is_not_found_error(&error) => return Ok(false),
            Err(error) => return Err(error),
        };
        if item.id != sent_body.object_id || item.kind != ObjectKind::AppDocument {
            return Err(ClientError::UnexpectedResponse(format!(
                "document {object_id} returned mismatched identity"
            )));
        }
        if !self.session_is_current(epoch) {
            return Err(ClientError::NotAuthenticated);
        }
        verify_object_head_envelope(&item)?;
        let committed = item.revision == sent_body.revision
            && crypto::object_envelope_parent_hash(&item.envelope.body)?
                == crypto::object_envelope_parent_hash(sent_body)?;
        if !committed {
            self.accept_recovered_head(epoch, api, encryption_key, &item)
                .await?;
            return Ok(false);
        }
        let _session = self.hold_session_for_write(epoch).await?;
        let persisted = self
            .local_store
            .persist_local_app_document_present_encrypted(
                StoredObjectIdentity {
                    object_id: &object_id,
                    created_at: &item.created_at,
                    source_device_id: &item.source_device_id.to_string(),
                },
                document.clone(),
                sent,
                item.created_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await;
        self.publish_accepted_write(&object_id, item.revision, persisted)
            .await?;
        Ok(true)
    }

    pub async fn app_document_history(
        &self,
        collection: &str,
        document_id: &str,
    ) -> Result<Vec<AppDocumentRevision>, ClientError> {
        self.run_work(None, async {
            let (_, _, revisions) = self.document_revisions(collection, document_id).await?;
            Ok(revisions
                .iter()
                .map(|item| AppDocumentRevision {
                    revision: item.revision,
                    written_at: item.created_at.clone(),
                    device_id: item.source_device_id.to_string(),
                    deleted: item.envelope.body.operation == ObjectEnvelopeOperation::Delete,
                })
                .collect())
        })
        .await
    }

    pub async fn app_document_revision(
        &self,
        collection: &str,
        document_id: &str,
        revision: u64,
    ) -> Result<Value, ClientError> {
        self.run_work(None, async {
            let (api, encryption_key, revisions) =
                self.document_revisions(collection, document_id).await?;
            let item = revisions
                .iter()
                .find(|item| item.revision == revision)
                .ok_or_else(|| {
                    ClientError::InvalidArgument(format!(
                        "document {document_id} has no revision {revision}"
                    ))
                })?;
            if item.envelope.body.operation == ObjectEnvelopeOperation::Delete {
                return Err(ClientError::InvalidArgument(format!(
                    "revision {revision} of document {document_id} deleted it"
                )));
            }
            let payload = first_payload(item)?;
            check_payload_ciphertext_size(payload, MAX_DOCUMENT_CIPHERTEXT_BYTES)?;
            let ciphertext = api
                .download_object_revision_payload(
                    document_id,
                    revision,
                    &payload.id.to_string(),
                    payload.ciphertext_size,
                )
                .await?;
            verify_payload_hash(payload, &ciphertext)?;
            Ok(documents::decrypt_value(
                &payload.nonce,
                &ciphertext,
                &encryption_key,
                &item.envelope.body,
                payload.id,
            )?)
        })
        .await
    }

    async fn document_revisions(
        &self,
        collection_name: &str,
        document_id: &str,
    ) -> Result<(ApiClient, Zeroizing<[u8; 32]>, Vec<ObjectListItem>), ClientError> {
        let collection = document_collection(collection_name)?;
        let object_id: ObjectId = parse_document_id(document_id)?.into();
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let SessionCredentials {
            api,
            encryption_key,
            ..
        } = self.credentials_for_session(epoch).await?;
        let head = self.local_head(document_id).await?;
        let api_ref = &api;
        let revisions: Vec<ObjectListItem> = stream::iter(1..=head.revision)
            .map(|revision| async move { api_ref.get_object_revision(document_id, revision).await })
            .buffered(HISTORY_FETCH_CONCURRENCY)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_, _>>()?;
        let mut parent = None;
        for (item, revision) in revisions.iter().zip(1..) {
            verify_object_head_envelope(item)?;
            if item.id != object_id
                || item.kind != ObjectKind::AppDocument
                || item.revision != revision
                || item.envelope.body.parent_hash != parent
            {
                return Err(object_envelope_error(format!(
                    "revision {revision} of document {document_id} does not follow the one before it"
                )));
            }
            parent = Some(crypto::object_envelope_parent_hash(&item.envelope.body)?);
            if item.envelope.body.operation != ObjectEnvelopeOperation::Delete {
                let meta = documents::decrypt_meta(
                    &item.meta_nonce,
                    &item.meta_ciphertext,
                    &encryption_key,
                    &item.envelope.body,
                )?;
                if meta.collection != collection.name {
                    return Err(ClientError::InvalidArgument(format!(
                        "document {document_id} belongs to {}, not {}",
                        meta.collection, collection.name
                    )));
                }
            }
        }
        if parent != Some(head.parent_hash) {
            return Err(object_envelope_error(format!(
                "the history of document {document_id} does not end at the revision this device holds"
            )));
        }
        Ok((api, encryption_key, revisions))
    }

    pub(super) async fn snapshot_app_documents(
        self: &Arc<Self>,
        generation: u64,
        stream_start_seq: i64,
    ) -> Result<(), ClientError> {
        let (api, encryption_key) = self.credentials_for_generation(generation).await?;
        let mut after = None;
        loop {
            let page = api
                .list_objects(
                    Some(ObjectKind::AppDocument),
                    Some(100),
                    Some(stream_start_seq),
                    after,
                )
                .await?;
            validate_snapshot_page(&page, after, stream_start_seq)?;
            for item in page.items {
                if item.kind != ObjectKind::AppDocument {
                    self.local_store
                        .mark_snapshot_seen(&item.id.to_string(), generation)
                        .await?;
                    warn!(id = %item.id, kind = %item.kind, "Skipped an object of another kind in the document snapshot");
                    continue;
                }
                self.persist_listed_app_document(&api, &item, &encryption_key, generation)
                    .await?;
            }
            match page.next_after {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        if let Some(visible) = self
            .local_store
            .sweep_kind(
                ObjectKind::AppDocument,
                generation,
                stream_start_seq,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?
        {
            self.publish_visible_state(visible).await;
        }
        Ok(())
    }

    pub(super) async fn persist_listed_app_document(
        &self,
        api: &ApiClient,
        item: &ObjectListItem,
        encryption_key: &[u8; 32],
        generation: u64,
    ) -> Result<(), ClientError> {
        let (document, encrypted) = match self
            .decrypt_app_document_item(api, item, encryption_key)
            .await
        {
            Ok(decrypted) => decrypted,
            Err(error) => {
                if let Err(error) = self.keep_held_revision(item, generation, error).await {
                    warn!(id = %item.id, "Failed to load a document: {error}");
                }
                return Ok(());
            }
        };
        if let Some(visible) = self
            .local_store
            .persist_snapshot_app_document_present_encrypted(
                StoredObjectIdentity {
                    object_id: &item.id.to_string(),
                    created_at: &item.created_at,
                    source_device_id: &item.source_device_id.to_string(),
                },
                document,
                &encrypted,
                item.created_seq,
                generation,
                RECENT_CLIPBOARD_LIMIT,
            )
            .await?
        {
            self.publish_visible_state(visible).await;
        }
        Ok(())
    }

    pub(super) async fn decrypt_app_document_item(
        &self,
        api: &ApiClient,
        item: &ObjectListItem,
        encryption_key: &[u8; 32],
    ) -> Result<(LocalAppDocumentRecord, EncryptedInlineObject), ClientError> {
        verify_object_list_item_envelope(item)?;
        self.check_revision_advance(item).await?;
        let meta = documents::decrypt_meta(
            &item.meta_nonce,
            &item.meta_ciphertext,
            encryption_key,
            &item.envelope.body,
        )?;
        let payload = first_payload(item)?;
        check_payload_ciphertext_size(payload, MAX_DOCUMENT_CIPHERTEXT_BYTES)?;
        let ciphertext = api
            .download_object_payload(
                &item.id.to_string(),
                &payload.id.to_string(),
                payload.ciphertext_size,
            )
            .await?;
        verify_payload_hash(payload, &ciphertext)?;
        let value = documents::decrypt_value(
            &payload.nonce,
            &ciphertext,
            encryption_key,
            &item.envelope.body,
            payload.id,
        )?;
        Ok((
            LocalAppDocumentRecord {
                collection: meta.collection,
                revision: item.revision,
                value,
            },
            EncryptedInlineObject {
                object: encrypted_object_from_list_item(item),
                payload_ciphertext: ciphertext,
            },
        ))
    }

    pub(super) async fn show_app_documents(&self, stamp: u64, documents: &[AppDocument]) {
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let mut session = self.app_data.session.lock().await;
        let Some(session) = session.as_mut().filter(|session| session.epoch == epoch) else {
            return;
        };
        if let Err(error) = session.tables.show_documents(stamp, documents) {
            warn!("Failed to show documents in the app-data tables: {error}");
        }
    }

    pub(super) async fn show_held_app_documents(&self) {
        match self.local_store.visible_state(RECENT_CLIPBOARD_LIMIT).await {
            Ok(visible) => {
                self.show_app_documents(visible.stamp, &visible.app_documents)
                    .await;
            }
            Err(error) => warn!("Failed to read the held documents: {error}"),
        }
    }
}

fn document_collection(name: &str) -> Result<&'static Collection, ClientError> {
    collections::collection(name)
        .filter(|collection| collection.storage == Storage::Documents)
        .ok_or_else(|| ClientError::InvalidArgument(format!("{name} is not a document collection")))
}

fn parse_document_id(id: &str) -> Result<uuid::Uuid, ClientError> {
    let id: uuid::Uuid = id.parse().map_err(|source| ClientError::InvalidId {
        kind: "document id",
        source,
    })?;
    if id.get_version() != Some(uuid::Version::SortRand) {
        return Err(ClientError::InvalidArgument(format!(
            "document id must be a UUIDv7, got {id}"
        )));
    }
    Ok(id)
}

fn first_payload(item: &ObjectListItem) -> Result<&ObjectPayloadDescriptor, ClientError> {
    item.envelope
        .body
        .payloads
        .first()
        .and_then(|first| item.payloads.iter().find(|payload| payload.id == first.id))
        .ok_or_else(|| {
            ClientError::UnexpectedResponse(format!("document {} has no value payload", item.id))
        })
}
