use std::{sync::Arc, time::Duration};

use clipper_app_types::{AppDataStatus, AppDataWrite};
use clipper_core::{
    crypto::app_data::{AppDataValueEnvelope, sign_app_data_change},
    models::{
        AppDataChange, AppDataChangeResult, AppDataChangesPage, AppDataChangesRequest, AppDataRow,
        DeviceId, MAX_APP_DATA_BATCH_CHANGES, MAX_APP_DATA_PAGE_CHANGES,
    },
};
use clipper_gym::ConflictRule;
use tracing::warn;
use uuid::Uuid;

use super::{Ordering, SyncEngine};
use crate::{
    api_client::ClientError,
    app_data::{
        AppDataKeys, AppDataSession,
        collections::{self, Collection, Storage},
        tables::{AppDataTables, QueryRows},
    },
    local_store::StoredAppDataRow,
};

const PUSH_RETRY_INTERVAL: Duration = Duration::from_secs(30);

impl SyncEngine {
    pub async fn query_app_data(&self, sql: &str) -> Result<QueryRows, ClientError> {
        self.run_work(None, self.query_app_data_inner(sql)).await
    }

    pub async fn write_app_data(
        &self,
        collection: &str,
        row_id: Option<&str>,
        write: AppDataWrite,
    ) -> Result<String, ClientError> {
        let entry = collections::collection(collection).ok_or_else(|| {
            ClientError::InvalidArgument(format!("unknown app-data collection {collection}"))
        })?;
        if entry.storage == Storage::Documents {
            return self
                .write_app_document(collection, row_id, None, write)
                .await;
        }
        let write = checked_write(entry, write)?;
        self.run_work(None, self.write_app_data_inner(collection, row_id, write))
            .await
    }

    pub async fn app_data_status(&self) -> Result<AppDataStatus, ClientError> {
        self.run_work(None, self.app_data_status_inner()).await
    }

    pub fn app_data_downloaded(&self) -> bool {
        self.app_data.downloaded()
    }

    pub async fn app_data_row_deleted(
        &self,
        collection: &str,
        row_id: Uuid,
    ) -> Result<bool, ClientError> {
        let collection = collections::collection(collection).ok_or_else(|| {
            ClientError::InvalidArgument(format!("unknown app-data collection {collection}"))
        })?;
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let mut guard = self.app_data.session.lock().await;
        let session = open_session(&mut guard, epoch)?;
        let row_key = session.keys.row_key(collection.name, row_id);
        Ok(self
            .local_store
            .app_data_row(&row_key)
            .await?
            .is_some_and(|(row, _)| row.deleted))
    }

    async fn query_app_data_inner(&self, sql: &str) -> Result<QueryRows, ClientError> {
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let session = Arc::clone(&self.app_data.session).lock_owned().await;
        if !session.as_ref().is_some_and(|open| open.epoch == epoch) {
            return Err(ClientError::NotAuthenticated);
        }
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || match session.as_ref() {
            Some(open) => open.tables.query(&sql),
            None => Err("app data is not open".into()),
        })
        .await
        .map_err(ClientError::Task)?
        .map_err(ClientError::InvalidArgument)
    }

    async fn write_app_data_inner(
        &self,
        collection_name: &str,
        row_id: Option<&str>,
        write: AppDataWrite,
    ) -> Result<String, ClientError> {
        let collection = collections::collection(collection_name).ok_or_else(|| {
            ClientError::InvalidArgument(format!("unknown app-data collection {collection_name}"))
        })?;
        let requested = row_id
            .map(|id| {
                id.parse::<Uuid>().map_err(|source| ClientError::InvalidId {
                    kind: "row id",
                    source,
                })
            })
            .transpose()?;
        let (value, derived) = match write {
            AppDataWrite::Value(value) => {
                let derived = derived_row_id(collection, &value)?;
                (Some(value), derived)
            }
            AppDataWrite::Delete => (None, None),
        };

        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let _active_key = self.hold_session_for_write(epoch).await?;
        let (_, device_id, signing_key) = self.current_device_signing_context().await?;
        let mut guard = self.app_data.session.lock().await;
        let session = open_session(&mut guard, epoch)?;
        let requested_row_exists = match requested {
            Some(id) if derived.is_some_and(|derived| derived != id) => self
                .local_store
                .app_data_row(&session.keys.row_key(collection.name, id))
                .await?
                .is_some_and(|(row, _)| !row.deleted),
            _ => false,
        };
        let row_id = row_id_for(
            collection,
            requested,
            derived,
            value.is_some(),
            requested_row_exists,
        )?;
        let row_key = session.keys.row_key(collection.name, row_id);
        let stored = self.local_store.app_data_row(&row_key).await?;
        let value = match (value, collection.merge, &stored) {
            (Some(value), Some(merge), Some((row, _))) if !row.deleted => {
                let earlier = session.keys.open(
                    &row_key,
                    row.revision,
                    false,
                    &row.nonce,
                    &row.ciphertext,
                )?;
                Some(match earlier.value {
                    Some(earlier) => merge(&value, &earlier).ok().flatten().unwrap_or(value),
                    None => value,
                })
            }
            (value, _, _) => value,
        };
        let (revision, replaces_revision) = match stored {
            None if value.is_none() => {
                return Err(ClientError::ItemNotFound {
                    id: row_id.to_string(),
                });
            }
            None => (1, 0),
            Some((row, _)) if row.deleted => {
                return Err(ClientError::InvalidArgument(format!(
                    "row {row_id} of {} was deleted",
                    collection.name
                )));
            }
            Some((row, Some(pending))) => (row.revision, pending.replaces_revision),
            Some((row, None)) => (
                row.revision
                    .checked_add(1)
                    .filter(|revision| *revision <= i64::MAX as u64)
                    .ok_or_else(|| {
                        ClientError::InvalidArgument(format!("row {row_id} has no next revision"))
                    })?,
                row.revision,
            ),
        };
        let envelope = AppDataValueEnvelope {
            collection: collection.name.to_string(),
            row_id: row_id.into(),
            schema_version: collection.schema_version,
            written_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            deleted: value.is_none(),
            value,
        };
        let row = sealed_row(
            &session.keys,
            row_key,
            revision,
            &envelope,
            device_id,
            &signing_key,
        )?;
        self.local_store
            .save_local_app_data_change(&row, replaces_revision)
            .await?;
        show(&session.tables, &envelope, revision)?;
        drop(guard);
        self.app_data.request_push();
        self.bump_version();
        Ok(row_id.to_string())
    }

    async fn app_data_status_inner(&self) -> Result<AppDataStatus, ClientError> {
        let epoch = self.history_epoch.load(Ordering::SeqCst);
        let _active_key = self.hold_session_for_write(epoch).await?;
        let mut guard = self.app_data.session.lock().await;
        let session = open_session(&mut guard, epoch)?;
        let last_sync_error = session
            .push_error
            .clone()
            .or_else(|| session.pull_error.clone());
        let counts = self.local_store.app_data_pending_counts().await?;
        Ok(AppDataStatus {
            pending_changes: u32::try_from(counts.pending).unwrap_or(u32::MAX),
            refused_changes: u32::try_from(counts.refused).unwrap_or(u32::MAX),
            last_sync_error,
        })
    }

    pub(super) async fn open_app_data(
        &self,
        epoch: u64,
        data_key: &[u8; 32],
    ) -> Result<(), ClientError> {
        let _active_key = self.hold_session_for_write(epoch).await?;
        let keys = AppDataKeys::derive(data_key);
        let tables = AppDataTables::open().map_err(table_error)?;
        for row in self.local_store.app_data_rows().await? {
            if row.deleted {
                continue;
            }
            match keys.open(
                &row.row_key,
                row.revision,
                false,
                &row.nonce,
                &row.ciphertext,
            ) {
                Ok(envelope) => show(&tables, &envelope, row.revision)?,
                Err(error) => warn!("Skipping a local app-data row that will not decrypt: {error}"),
            }
        }
        self.app_data
            .set_last_applied(self.local_store.last_applied_app_data_sequence().await?);
        *self.app_data.session.lock().await = Some(AppDataSession {
            epoch,
            keys,
            tables,
            pull_error: None,
            push_error: None,
        });
        Ok(())
    }

    pub(super) async fn app_data_sync_loop(self: Arc<Self>, epoch: u64) {
        loop {
            if !self.session_is_current(epoch) {
                return;
            }
            let connected = !self.offline.load(Ordering::SeqCst);
            let mut failed = false;
            if connected && self.app_data.take_pull_request() {
                let pulled = self.pull_app_data(epoch).await;
                let error = match &pulled {
                    Ok(0) => None,
                    Ok(rejected) => Some(format!(
                        "Rejected {rejected} app-data changes from the server that failed verification"
                    )),
                    Err(error) => Some(error.to_string()),
                };
                if pulled.is_ok() {
                    self.app_data.mark_downloaded();
                }
                if let Err(error) = pulled {
                    if self.app_data_sync_ended(epoch, &error).await {
                        return;
                    }
                    self.app_data.mark_pull_needed();
                    failed = true;
                }
                self.record_app_data_sync_error(epoch, |session| session.pull_error = error)
                    .await;
            }
            if connected {
                match self.push_app_data(epoch).await {
                    Ok(None) => {}
                    Ok(Some(refusals)) => {
                        let error = (!refusals.is_empty()).then(|| {
                            format!(
                                "The server refused {} app-data changes: {}",
                                refusals.len(),
                                refusals.join(", ")
                            )
                        });
                        self.record_app_data_sync_error(epoch, |session| {
                            session.push_error = error;
                        })
                        .await;
                    }
                    Err(error) => {
                        if self.app_data_sync_ended(epoch, &error).await {
                            return;
                        }
                        failed = true;
                        self.record_app_data_sync_error(epoch, |session| {
                            session.push_error = Some(error.to_string());
                        })
                        .await;
                    }
                }
            }
            let waiting = !matches!(self.app_data_pending_count(epoch).await, Ok(0));
            if failed || waiting {
                let _ =
                    tokio::time::timeout(PUSH_RETRY_INTERVAL, self.app_data.wake.notified()).await;
            } else {
                self.app_data.wake.notified().await;
            }
        }
    }

    async fn app_data_sync_ended(&self, epoch: u64, error: &ClientError) -> bool {
        warn!("App-data sync failed: {error}");
        self.end_refused_session_for_epoch(epoch, error).await || !self.session_is_current(epoch)
    }

    async fn app_data_pending_count(&self, epoch: u64) -> Result<u64, ClientError> {
        let _active_key = self.hold_session_for_write(epoch).await?;
        Ok(self.local_store.app_data_pending_counts().await?.pending)
    }

    async fn record_app_data_sync_error(
        &self,
        epoch: u64,
        record: impl FnOnce(&mut AppDataSession),
    ) {
        if let Some(session) = self
            .app_data
            .session
            .lock()
            .await
            .as_mut()
            .filter(|session| session.epoch == epoch)
        {
            record(session);
        }
    }

    async fn pull_app_data(&self, epoch: u64) -> Result<usize, ClientError> {
        let mut rejected = 0;
        loop {
            let (api, after) = {
                let _active_key = self.hold_session_for_write(epoch).await?;
                (
                    self.api.with_current_token()?,
                    self.local_store.last_applied_app_data_sequence().await?,
                )
            };
            let page = api
                .app_data_changes(after, MAX_APP_DATA_PAGE_CHANGES)
                .await?;
            check_page(&page, after)?;
            let Some(last) = page.rows.last().map(|row| row.sequence) else {
                return Ok(rejected);
            };
            rejected += self.apply_pulled_rows(epoch, &page.rows, last).await?;
            if last >= page.newest_sequence {
                return Ok(rejected);
            }
        }
    }

    async fn apply_pulled_rows(
        &self,
        epoch: u64,
        rows: &[AppDataRow],
        last_sequence: i64,
    ) -> Result<usize, ClientError> {
        let _active_key = self.hold_session_for_write(epoch).await?;
        let mut guard = self.app_data.session.lock().await;
        let session = open_session(&mut guard, epoch)?;
        let mut stored = Vec::with_capacity(rows.len());
        let mut envelopes = Vec::with_capacity(rows.len());
        let mut rejected = 0;
        for row in rows {
            let opened = session.keys.verified(row).and_then(|envelope| {
                StoredAppDataRow::from_server(row)
                    .map(|stored| (stored, envelope))
                    .ok_or_else(|| ClientError::UnexpectedResponse("malformed row key".into()))
            });
            match opened {
                Ok((row, envelope)) => {
                    stored.push(row);
                    envelopes.push(envelope);
                }
                Err(error) => {
                    warn!(
                        sequence = row.sequence,
                        "Rejected an app-data change from the server: {error}"
                    );
                    rejected += 1;
                }
            }
        }
        let applied = self
            .local_store
            .apply_app_data_page(&stored, last_sequence)
            .await?;
        let mut changed = false;
        for ((row, envelope), applied) in stored.iter().zip(&envelopes).zip(applied) {
            if applied {
                show(&session.tables, envelope, row.revision)?;
                changed = true;
            }
        }
        self.app_data.set_last_applied(last_sequence);
        if changed {
            self.bump_version();
        }
        Ok(rejected)
    }

    async fn push_app_data(&self, epoch: u64) -> Result<Option<Vec<String>>, ClientError> {
        let mut refusals: Option<Vec<String>> = None;
        let mut resent = Vec::new();
        let mut after_position = 0;
        loop {
            let (api, batch) = {
                let _active_key = self.hold_session_for_write(epoch).await?;
                (
                    self.api.with_current_token()?,
                    self.local_store
                        .pending_app_data_changes(after_position, MAX_APP_DATA_BATCH_CHANGES)
                        .await?,
                )
            };
            let Some((last_position, _)) = batch.last() else {
                break;
            };
            after_position = *last_position;
            let pushed = self
                .send_app_data_batch(
                    epoch,
                    &api,
                    batch.into_iter().map(|(_, change)| change).collect(),
                )
                .await?;
            refusals.get_or_insert_default().extend(pushed.refusals);
            resent.extend(pushed.resent);
        }
        for row_keys in resent.chunks(MAX_APP_DATA_BATCH_CHANGES) {
            let (api, changes) = {
                let _active_key = self.hold_session_for_write(epoch).await?;
                (
                    self.api.with_current_token()?,
                    self.local_store
                        .pending_app_data_changes_for(row_keys)
                        .await?,
                )
            };
            if changes.is_empty() {
                continue;
            }
            let pushed = self.send_app_data_batch(epoch, &api, changes).await?;
            refusals.get_or_insert_default().extend(pushed.refusals);
        }
        Ok(refusals)
    }

    async fn send_app_data_batch(
        &self,
        epoch: u64,
        api: &crate::api_client::ApiClient,
        changes: Vec<AppDataChange>,
    ) -> Result<PushedBatch, ClientError> {
        let request = AppDataChangesRequest { changes };
        let response = api.send_app_data_changes(&request).await?;
        if response.results.len() != request.changes.len() {
            return Err(ClientError::UnexpectedResponse(
                "app-data results do not match the changes sent".into(),
            ));
        }
        self.apply_push_results(epoch, request.changes, response.results)
            .await
    }

    async fn apply_push_results(
        &self,
        epoch: u64,
        changes: Vec<AppDataChange>,
        results: Vec<AppDataChangeResult>,
    ) -> Result<PushedBatch, ClientError> {
        let _active_key = self.hold_session_for_write(epoch).await?;
        let (_, device_id, signing_key) = self.current_device_signing_context().await?;
        let mut guard = self.app_data.session.lock().await;
        let session = open_session(&mut guard, epoch)?;
        let writer = Writer {
            device_id,
            signing_key: &signing_key,
        };
        let mut pushed = PushedBatch::default();
        for (change, result) in changes.into_iter().zip(results) {
            let row_key: [u8; 32] = change.row_key.as_slice().try_into().map_err(|_| {
                ClientError::LocalStore("a pending app-data change has a malformed row key".into())
            })?;
            match result {
                AppDataChangeResult::Accepted { sequence } => {
                    if !self
                        .local_store
                        .accept_app_data_change(&row_key, &change.signature, sequence)
                        .await?
                    {
                        self.resend_app_data_change_after(
                            session,
                            &row_key,
                            change.revision,
                            &writer,
                            None,
                        )
                        .await?;
                        pushed.resent.push(row_key);
                    }
                }
                AppDataChangeResult::Refused { reason } => {
                    let reason = format!("{reason:?}");
                    if self
                        .local_store
                        .refuse_app_data_change(&row_key, &change.signature, &reason)
                        .await?
                    {
                        pushed.refusals.push(reason);
                    }
                }
                AppDataChangeResult::Conflict { current } => {
                    match self
                        .resolve_app_data_conflict(session, &row_key, &change, current, &writer)
                        .await
                    {
                        Ok(true) => pushed.resent.push(row_key),
                        Ok(false) => {}
                        Err(error) => {
                            let reason = error.to_string();
                            if self
                                .local_store
                                .refuse_app_data_change(&row_key, &change.signature, &reason)
                                .await?
                            {
                                pushed.refusals.push(reason);
                            }
                        }
                    }
                }
            }
        }
        Ok(pushed)
    }

    async fn resolve_app_data_conflict(
        &self,
        session: &mut AppDataSession,
        row_key: &[u8; 32],
        sent: &AppDataChange,
        current: Option<AppDataRow>,
        writer: &Writer<'_>,
    ) -> Result<bool, ClientError> {
        let Some((local, Some(_))) = self.local_store.app_data_row(row_key).await? else {
            return Ok(false);
        };
        let Some(current) = current else {
            if local.revision != 1 {
                return Err(ClientError::UnexpectedResponse(format!(
                    "the server no longer holds a row this device holds at revision {}",
                    local.revision
                )));
            }
            self.resend_app_data_change_after(session, row_key, 0, writer, None)
                .await?;
            return Ok(true);
        };
        if current.row_key != row_key.as_slice() {
            return Err(ClientError::UnexpectedResponse(
                "the server reported a conflict for a different row".into(),
            ));
        }
        if current.revision <= sent.replaces_revision {
            return Err(ClientError::UnexpectedResponse(format!(
                "the server's revision {} is not newer than revision {} this change replaces",
                current.revision, sent.replaces_revision
            )));
        }
        let theirs = session.keys.verified(&current)?;
        let ours = session.keys.open(
            row_key,
            local.revision,
            local.deleted,
            &local.nonce,
            &local.ciphertext,
        )?;
        let keep_ours = if ours.deleted || theirs.deleted {
            ours.deleted && !theirs.deleted
        } else {
            collections::collection(&ours.collection).is_some_and(|collection| {
                collection.storage == Storage::Rows(ConflictRule::LastWriteWins)
                    && written_later(&ours.written_at, &theirs.written_at)
            })
        };
        let merged = match (
            &ours.value,
            &theirs.value,
            collections::collection(&ours.collection).and_then(|collection| collection.merge),
        ) {
            (Some(our_value), Some(their_value), Some(merge))
                if !ours.deleted && !theirs.deleted =>
            {
                let (winner, loser) = if keep_ours {
                    (our_value, their_value)
                } else {
                    (their_value, our_value)
                };
                merge(winner, loser).ok().flatten()
            }
            _ => None,
        };
        if keep_ours || merged.is_some() {
            let replacement = merged.map(|value| AppDataValueEnvelope {
                written_at: if keep_ours {
                    ours.written_at.clone()
                } else {
                    theirs.written_at.clone()
                },
                value: Some(value),
                ..ours.clone()
            });
            self.resend_app_data_change_after(
                session,
                row_key,
                current.revision,
                writer,
                replacement,
            )
            .await?;
            return Ok(true);
        }
        let row = StoredAppDataRow::from_server(&current)
            .ok_or_else(|| ClientError::UnexpectedResponse("malformed row key".into()))?;
        self.local_store.replace_app_data_row(&row).await?;
        show(&session.tables, &theirs, current.revision)?;
        self.bump_version();
        Ok(false)
    }

    async fn resend_app_data_change_after(
        &self,
        session: &mut AppDataSession,
        row_key: &[u8; 32],
        onto_revision: u64,
        writer: &Writer<'_>,
        replacement: Option<AppDataValueEnvelope>,
    ) -> Result<(), ClientError> {
        let Some((local, Some(_))) = self.local_store.app_data_row(row_key).await? else {
            return Ok(());
        };
        let envelope = match replacement {
            Some(envelope) => envelope,
            None => session.keys.open(
                row_key,
                local.revision,
                local.deleted,
                &local.nonce,
                &local.ciphertext,
            )?,
        };
        let revision = onto_revision + 1;
        let row = sealed_row(
            &session.keys,
            *row_key,
            revision,
            &envelope,
            writer.device_id,
            writer.signing_key,
        )?;
        self.local_store
            .save_local_app_data_change(&row, onto_revision)
            .await?;
        show(&session.tables, &envelope, revision)
    }
}

#[derive(Default)]
struct PushedBatch {
    refusals: Vec<String>,
    resent: Vec<[u8; 32]>,
}

struct Writer<'a> {
    device_id: DeviceId,
    signing_key: &'a [u8; 32],
}

fn open_session(
    session: &mut Option<AppDataSession>,
    epoch: u64,
) -> Result<&mut AppDataSession, ClientError> {
    session
        .as_mut()
        .filter(|session| session.epoch == epoch)
        .ok_or(ClientError::NotAuthenticated)
}

pub(super) fn checked_write(
    collection: &Collection,
    write: AppDataWrite,
) -> Result<AppDataWrite, ClientError> {
    match write {
        AppDataWrite::Value(value) => (collection.checked_value)(value)
            .map(AppDataWrite::Value)
            .map_err(|error| {
                ClientError::InvalidArgument(format!("invalid {} value: {error}", collection.name))
            }),
        AppDataWrite::Delete => Ok(AppDataWrite::Delete),
    }
}

fn derived_row_id(
    collection: &Collection,
    value: &serde_json::Value,
) -> Result<Option<Uuid>, ClientError> {
    collection
        .fixed_row_id
        .map(|derive| derive(value).map_err(ClientError::InvalidArgument))
        .transpose()
}

fn row_id_for(
    collection: &Collection,
    requested: Option<Uuid>,
    derived: Option<Uuid>,
    writes_value: bool,
    requested_row_exists: bool,
) -> Result<Uuid, ClientError> {
    match (requested, derived) {
        (None, _) if !writes_value => Err(ClientError::InvalidArgument(
            "a delete needs a row id".into(),
        )),
        (Some(id), _) if !writes_value => Ok(id),
        (Some(id), Some(derived)) if id == derived || requested_row_exists => Ok(id),
        (Some(_), Some(derived)) => Err(ClientError::InvalidArgument(format!(
            "{} rows use the id derived from their value, {derived}",
            collection.name
        ))),
        (None, Some(derived)) => Ok(derived),
        (Some(id), None) if id.get_version() == Some(uuid::Version::SortRand) => Ok(id),
        (Some(id), None) => Err(ClientError::InvalidArgument(format!(
            "row id must be a UUIDv7, got {id}"
        ))),
        (None, None) => Ok(Uuid::now_v7()),
    }
}

fn sealed_row(
    keys: &AppDataKeys,
    row_key: [u8; 32],
    revision: u64,
    envelope: &AppDataValueEnvelope,
    device_id: DeviceId,
    signing_key: &[u8; 32],
) -> Result<StoredAppDataRow, ClientError> {
    let (nonce, ciphertext) = keys.seal(&row_key, revision, envelope)?;
    let mut change = AppDataChange {
        row_key: row_key.to_vec(),
        revision,
        replaces_revision: revision - 1,
        deleted: envelope.deleted,
        nonce,
        ciphertext,
        device_id,
        signature: Vec::new(),
    };
    change.signature = sign_app_data_change(signing_key, &change)?;
    Ok(StoredAppDataRow {
        row_key,
        revision,
        sequence: None,
        deleted: change.deleted,
        nonce: change.nonce,
        ciphertext: change.ciphertext,
        device_id: Some(device_id),
        signature: change.signature,
    })
}

fn show(
    tables: &AppDataTables,
    envelope: &AppDataValueEnvelope,
    revision: u64,
) -> Result<(), ClientError> {
    let Some(collection) = collections::collection(&envelope.collection)
        .filter(|collection| collection.storage != Storage::Documents)
    else {
        return Ok(());
    };
    let id = envelope.row_id.into_uuid();
    match &envelope.value {
        Some(value) if !envelope.deleted => {
            tables.put(collection, id, revision, &envelope.written_at, value)
        }
        _ => tables.remove(collection, id),
    }
    .map_err(table_error)
}

fn written_later(ours: &str, theirs: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(ours),
        chrono::DateTime::parse_from_rfc3339(theirs),
    ) {
        (Ok(ours), Ok(theirs)) => ours > theirs,
        _ => false,
    }
}

fn check_page(page: &AppDataChangesPage, after: i64) -> Result<(), ClientError> {
    if page.rows.len() as u64 > MAX_APP_DATA_PAGE_CHANGES {
        return Err(ClientError::UnexpectedResponse(
            "app-data page exceeds the requested limit".into(),
        ));
    }
    let mut previous = after;
    for row in &page.rows {
        if row.sequence <= previous {
            return Err(ClientError::UnexpectedResponse(
                "app-data page sequences do not advance".into(),
            ));
        }
        previous = row.sequence;
    }
    Ok(())
}

fn table_error(error: rusqlite::Error) -> ClientError {
    ClientError::LocalStore(format!("app-data tables: {error}"))
}
