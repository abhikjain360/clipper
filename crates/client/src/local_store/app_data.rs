use clipper_core::models::{AppDataChange, AppDataRow, DeviceId};
use rusqlite::{Connection, OptionalExtension, params};

use super::{LocalStore, LocalStoreError};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_data_rows (
    row_key    BLOB    PRIMARY KEY NOT NULL,
    revision   INTEGER NOT NULL,
    sequence   INTEGER,
    deleted    INTEGER NOT NULL,
    nonce      BLOB    NOT NULL,
    ciphertext BLOB    NOT NULL,
    device_id  TEXT,
    signature  BLOB    NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS app_data_pending (
    row_key           BLOB    PRIMARY KEY NOT NULL
                              REFERENCES app_data_rows (row_key) ON DELETE CASCADE,
    replaces_revision INTEGER NOT NULL,
    refusal           TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS app_data_sync (
    id            INTEGER PRIMARY KEY CHECK (id = 1),
    last_sequence INTEGER NOT NULL
) STRICT;
";

#[derive(Debug, Clone)]
pub(crate) struct StoredAppDataRow {
    pub row_key: [u8; 32],
    pub revision: u64,
    pub sequence: Option<i64>,
    pub deleted: bool,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub device_id: Option<DeviceId>,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingAppDataChange {
    pub replaces_revision: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AppDataPendingCounts {
    pub pending: u64,
    pub refused: u64,
}

impl StoredAppDataRow {
    pub fn from_server(row: &AppDataRow) -> Option<Self> {
        Some(Self {
            row_key: row.row_key.as_slice().try_into().ok()?,
            revision: row.revision,
            sequence: Some(row.sequence),
            deleted: row.deleted,
            nonce: row.nonce.clone(),
            ciphertext: row.ciphertext.clone(),
            device_id: row.device_id,
            signature: row.signature.clone(),
        })
    }

    pub fn change(&self, replaces_revision: u64) -> Option<AppDataChange> {
        Some(AppDataChange {
            row_key: self.row_key.to_vec(),
            revision: self.revision,
            replaces_revision,
            deleted: self.deleted,
            nonce: self.nonce.clone(),
            ciphertext: self.ciphertext.clone(),
            device_id: self.device_id?,
            signature: self.signature.clone(),
        })
    }
}

impl LocalStore {
    pub(crate) async fn app_data_rows(&self) -> Result<Vec<StoredAppDataRow>, LocalStoreError> {
        self.with_database(|connection| {
            let mut statement =
                connection.prepare(&format!("SELECT {ROW_COLUMNS} FROM app_data_rows r"))?;
            let rows = statement.query_map([], read_row)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
    }

    pub(crate) async fn app_data_row(
        &self,
        row_key: &[u8; 32],
    ) -> Result<Option<(StoredAppDataRow, Option<PendingAppDataChange>)>, LocalStoreError> {
        self.with_database(|connection| row_with_pending(connection, row_key))
            .await
    }

    pub(crate) async fn save_local_app_data_change(
        &self,
        row: &StoredAppDataRow,
        replaces_revision: u64,
    ) -> Result<(), LocalStoreError> {
        self.with_database(|connection| {
            let transaction = connection.transaction()?;
            upsert_row(&transaction, row)?;
            transaction.execute(
                "INSERT INTO app_data_pending (row_key, replaces_revision, refusal)
                 VALUES (?1, ?2, NULL)
                 ON CONFLICT (row_key) DO UPDATE SET
                     replaces_revision = excluded.replaces_revision,
                     refusal = NULL",
                params![row.row_key.as_slice(), replaces_revision as i64],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn pending_app_data_changes(
        &self,
        limit: usize,
    ) -> Result<Vec<AppDataChange>, LocalStoreError> {
        self.with_database(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {ROW_COLUMNS}, p.replaces_revision
                 FROM app_data_rows r JOIN app_data_pending p ON p.row_key = r.row_key
                 WHERE p.refusal IS NULL
                 ORDER BY r.rowid
                 LIMIT ?1"
            ))?;
            let rows = statement.query_map(params![limit as i64], |row| {
                Ok((read_row(row)?, row.get::<_, i64>(8)? as u64))
            })?;
            let mut changes = Vec::new();
            for row in rows {
                let (row, replaces_revision) = row?;
                match row.change(replaces_revision) {
                    Some(change) => changes.push(change),
                    None => tracing::warn!("Skipping a pending app-data change with no device id"),
                }
            }
            Ok(changes)
        })
        .await
    }

    pub(crate) async fn accept_app_data_change(
        &self,
        row_key: &[u8; 32],
        signature: &[u8],
        sequence: i64,
    ) -> Result<bool, LocalStoreError> {
        self.with_database(|connection| {
            let transaction = connection.transaction()?;
            if !holds_pending_change(&transaction, row_key, signature)? {
                return Ok(false);
            }
            transaction.execute(
                "UPDATE app_data_rows SET sequence = ?2 WHERE row_key = ?1",
                params![row_key.as_slice(), sequence],
            )?;
            transaction.execute(
                "DELETE FROM app_data_pending WHERE row_key = ?1",
                params![row_key.as_slice()],
            )?;
            transaction.commit()?;
            Ok(true)
        })
        .await
    }

    pub(crate) async fn refuse_app_data_change(
        &self,
        row_key: &[u8; 32],
        signature: &[u8],
        reason: &str,
    ) -> Result<bool, LocalStoreError> {
        self.with_database(|connection| {
            let transaction = connection.transaction()?;
            if !holds_pending_change(&transaction, row_key, signature)? {
                return Ok(false);
            }
            transaction.execute(
                "UPDATE app_data_pending SET refusal = ?2 WHERE row_key = ?1",
                params![row_key.as_slice(), reason],
            )?;
            transaction.commit()?;
            Ok(true)
        })
        .await
    }

    pub(crate) async fn replace_app_data_row(
        &self,
        row: &StoredAppDataRow,
    ) -> Result<(), LocalStoreError> {
        self.with_database(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute(
                "DELETE FROM app_data_pending WHERE row_key = ?1",
                params![row.row_key.as_slice()],
            )?;
            upsert_row(&transaction, row)?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn apply_app_data_page(
        &self,
        rows: &[StoredAppDataRow],
        last_sequence: i64,
    ) -> Result<Vec<bool>, LocalStoreError> {
        self.with_database(|connection| {
            let transaction = connection.transaction()?;
            let mut applied = Vec::with_capacity(rows.len());
            for row in rows {
                let newer = match row_with_pending(&transaction, &row.row_key)? {
                    None => true,
                    Some((_, Some(_))) => false,
                    Some((local, None)) => row.revision > local.revision,
                };
                if newer {
                    upsert_row(&transaction, row)?;
                }
                applied.push(newer);
            }
            transaction.execute(
                "INSERT INTO app_data_sync (id, last_sequence) VALUES (1, ?1)
                 ON CONFLICT (id) DO UPDATE SET
                     last_sequence = max(last_sequence, excluded.last_sequence)",
                params![last_sequence],
            )?;
            transaction.commit()?;
            Ok(applied)
        })
        .await
    }

    pub(crate) async fn last_applied_app_data_sequence(&self) -> Result<i64, LocalStoreError> {
        self.with_database(|connection| {
            Ok(connection
                .query_row(
                    "SELECT last_sequence FROM app_data_sync WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0))
        })
        .await
    }

    pub(crate) async fn app_data_pending_counts(
        &self,
    ) -> Result<AppDataPendingCounts, LocalStoreError> {
        self.with_database(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FILTER (WHERE refusal IS NULL),
                        count(*) FILTER (WHERE refusal IS NOT NULL)
                 FROM app_data_pending",
                [],
                |row| {
                    Ok(AppDataPendingCounts {
                        pending: row.get::<_, i64>(0)? as u64,
                        refused: row.get::<_, i64>(1)? as u64,
                    })
                },
            )?)
        })
        .await
    }
}

const ROW_COLUMNS: &str =
    "r.row_key, r.revision, r.sequence, r.deleted, r.nonce, r.ciphertext, r.device_id, r.signature";

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredAppDataRow> {
    let row_key: Vec<u8> = row.get(0)?;
    let device_id: Option<String> = row.get(6)?;
    Ok(StoredAppDataRow {
        row_key: row_key.as_slice().try_into().map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Blob,
                "stored row key is not 32 bytes".into(),
            )
        })?,
        revision: row.get::<_, i64>(1)? as u64,
        sequence: row.get(2)?,
        deleted: row.get(3)?,
        nonce: row.get(4)?,
        ciphertext: row.get(5)?,
        device_id: device_id
            .map(|id| id.parse())
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    6,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        signature: row.get(7)?,
    })
}

fn row_with_pending(
    connection: &Connection,
    row_key: &[u8; 32],
) -> Result<Option<(StoredAppDataRow, Option<PendingAppDataChange>)>, LocalStoreError> {
    connection
        .query_row(
            &format!(
                "SELECT {ROW_COLUMNS}, p.replaces_revision
                 FROM app_data_rows r LEFT JOIN app_data_pending p ON p.row_key = r.row_key
                 WHERE r.row_key = ?1"
            ),
            params![row_key.as_slice()],
            |row| {
                let pending =
                    row.get::<_, Option<i64>>(8)?
                        .map(|replaces_revision| PendingAppDataChange {
                            replaces_revision: replaces_revision as u64,
                        });
                Ok((read_row(row)?, pending))
            },
        )
        .optional()
        .map_err(Into::into)
}

fn holds_pending_change(
    connection: &Connection,
    row_key: &[u8; 32],
    signature: &[u8],
) -> Result<bool, LocalStoreError> {
    Ok(connection
        .query_row(
            "SELECT r.signature = ?2
             FROM app_data_rows r JOIN app_data_pending p ON p.row_key = r.row_key
             WHERE r.row_key = ?1",
            params![row_key.as_slice(), signature],
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false))
}

fn upsert_row(connection: &Connection, row: &StoredAppDataRow) -> Result<(), LocalStoreError> {
    connection.execute(
        "INSERT INTO app_data_rows
             (row_key, revision, sequence, deleted, nonce, ciphertext, device_id, signature)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (row_key) DO UPDATE SET
             revision   = excluded.revision,
             sequence   = excluded.sequence,
             deleted    = excluded.deleted,
             nonce      = excluded.nonce,
             ciphertext = excluded.ciphertext,
             device_id  = excluded.device_id,
             signature  = excluded.signature",
        params![
            row.row_key.as_slice(),
            row.revision as i64,
            row.sequence,
            row.deleted,
            row.nonce,
            row.ciphertext,
            row.device_id.map(|id| id.to_string()),
            row.signature,
        ],
    )?;
    Ok(())
}
