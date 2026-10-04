use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rusqlite::{
    Connection,
    hooks::{AuthAction, AuthContext, Authorization},
    params,
    types::ValueRef,
};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::{
    collections::{self, COLLECTIONS, Collection, Storage},
    documents::AppDocument,
};

const QUERY_TIME_LIMIT: Duration = Duration::from_secs(5);
const PROGRESS_CHECK_OPERATIONS: i32 = 1_000;

pub(crate) type QueryRows = Vec<Map<String, Value>>;

pub(crate) struct AppDataTables {
    connection: Connection,
    documents_stamp: u64,
    shown_documents: HashMap<Uuid, (&'static Collection, u64)>,
}

impl AppDataTables {
    pub fn open() -> rusqlite::Result<Self> {
        let connection = Connection::open_in_memory()?;
        connection.pragma_update(None, "temp_store", "MEMORY")?;
        let mut schemas = Vec::new();
        for collection in COLLECTIONS {
            let schema = collection.schema();
            if !schemas.contains(&schema) {
                connection
                    .execute_batch(&format!("ATTACH DATABASE ':memory:' AS {}", quoted(schema)))?;
                schemas.push(schema);
            }
            connection.execute_batch(&format!(
                "CREATE TABLE {} (
                     id         TEXT    PRIMARY KEY NOT NULL,
                     revision   INTEGER NOT NULL,
                     written_at TEXT    NOT NULL,
                     value      TEXT    NOT NULL
                 ) STRICT",
                table_name(collection)
            ))?;
            for field in collection.indexed_fields {
                connection.execute_batch(&format!(
                    "CREATE INDEX {}.{} ON {} (json_extract(value, '$.{field}'))",
                    quoted(schema),
                    quoted(&format!("{}_{field}", collection.table())),
                    quoted(collection.table()),
                ))?;
            }
        }
        Ok(Self {
            connection,
            documents_stamp: 0,
            shown_documents: HashMap::new(),
        })
    }

    pub fn show_documents(
        &mut self,
        stamp: u64,
        documents: &[AppDocument],
    ) -> rusqlite::Result<()> {
        if stamp <= self.documents_stamp {
            return Ok(());
        }
        self.documents_stamp = stamp;
        let mut shown = HashMap::with_capacity(documents.len());
        for document in documents {
            let Some(collection) = collections::collection(&document.collection)
                .filter(|collection| collection.storage == Storage::Documents)
            else {
                continue;
            };
            let Ok(id) = document.id.parse::<Uuid>() else {
                continue;
            };
            match self.shown_documents.get(&id) {
                Some((held, revision))
                    if held.name == collection.name && *revision == document.revision => {}
                held => {
                    if let Some((held, _)) = held.filter(|(held, _)| held.name != collection.name) {
                        self.remove(held, id)?;
                    }
                    self.put(
                        collection,
                        id,
                        document.revision,
                        &document.written_at,
                        &document.value,
                    )?;
                }
            }
            shown.insert(id, (collection, document.revision));
        }
        for (id, (collection, _)) in &self.shown_documents {
            if !shown.contains_key(id) {
                self.remove(collection, *id)?;
            }
        }
        self.shown_documents = shown;
        Ok(())
    }

    pub fn put(
        &self,
        collection: &Collection,
        id: Uuid,
        revision: u64,
        written_at: &str,
        value: &Value,
    ) -> rusqlite::Result<()> {
        self.connection
            .prepare_cached(&format!(
                "INSERT INTO {} (id, revision, written_at, value) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET
                     revision   = excluded.revision,
                     written_at = excluded.written_at,
                     value      = excluded.value",
                table_name(collection)
            ))?
            .execute(params![
                id.to_string(),
                revision as i64,
                written_at,
                value.to_string()
            ])?;
        Ok(())
    }

    pub fn remove(&self, collection: &Collection, id: Uuid) -> rusqlite::Result<()> {
        self.connection
            .prepare_cached(&format!(
                "DELETE FROM {} WHERE id = ?1",
                table_name(collection)
            ))?
            .execute(params![id.to_string()])?;
        Ok(())
    }

    pub fn query(&self, sql: &str) -> Result<QueryRows, String> {
        let deadline = Instant::now() + QUERY_TIME_LIMIT;
        self.connection
            .authorizer(Some(read_only))
            .map_err(|error| error.to_string())?;
        let progress = self.connection.progress_handler(
            PROGRESS_CHECK_OPERATIONS,
            Some(move || Instant::now() >= deadline),
        );
        let rows = progress
            .map_err(|error| error.to_string())
            .and_then(|()| self.read(sql));
        let stopped = self
            .connection
            .progress_handler(PROGRESS_CHECK_OPERATIONS, None::<fn() -> bool>)
            .and_then(|()| {
                self.connection
                    .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            });
        if let Err(error) = stopped {
            tracing::warn!("Failed to reset the app-data query guards: {error}");
        }
        rows
    }

    fn read(&self, sql: &str) -> Result<QueryRows, String> {
        let mut statement = self
            .connection
            .prepare(sql)
            .map_err(|error| refused_query(&error))?;
        if statement.column_count() == 0 || !statement.readonly() {
            return Err("only one read-only SELECT statement is allowed".into());
        }
        let columns: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(String::from)
            .collect();
        let mut rows = statement.query([]).map_err(|error| refused_query(&error))?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(|error| refused_query(&error))? {
            let mut object = Map::new();
            for (index, column) in columns.iter().enumerate() {
                let value = row.get_ref(index).map_err(|error| error.to_string())?;
                object.insert(column.clone(), json_value(value));
            }
            result.push(object);
        }
        Ok(result)
    }
}

fn read_only(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Select
        | AuthAction::Read { .. }
        | AuthAction::Function { .. }
        | AuthAction::Recursive => Authorization::Allow,
        _ => Authorization::Deny,
    }
}

fn refused_query(error: &rusqlite::Error) -> String {
    match error {
        rusqlite::Error::MultipleStatement => "only one statement is allowed".into(),
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::AuthorizationForStatementDenied =>
        {
            "only one read-only SELECT statement is allowed".into()
        }
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::OperationInterrupted =>
        {
            format!(
                "the query ran longer than {} seconds",
                QUERY_TIME_LIMIT.as_secs()
            )
        }
        error => error.to_string(),
    }
}

fn json_value(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(number) => Value::from(number),
        ValueRef::Real(number) => {
            serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
        }
        ValueRef::Text(text) => Value::String(String::from_utf8_lossy(text).into_owned()),
        ValueRef::Blob(bytes) => Value::String(STANDARD.encode(bytes)),
    }
}

fn table_name(collection: &Collection) -> String {
    format!(
        "{}.{}",
        quoted(collection.schema()),
        quoted(collection.table())
    )
}

fn quoted(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_read_the_tables_and_refuse_everything_else() {
        let tables = AppDataTables::open().unwrap();
        let exercises = super::super::collections::collection("gym.exercises").unwrap();
        let id = Uuid::now_v7();
        tables
            .put(
                exercises,
                id,
                1,
                "2026-10-07T10:00:00Z",
                &serde_json::json!({"name": "Squat", "muscles": [], "archived": false}),
            )
            .unwrap();

        let rows = tables
            .query(
                "SELECT id, revision, json_extract(value, '$.name') AS name
                 FROM gym.exercises WHERE json_extract(value, '$.archived') = 0",
            )
            .unwrap();
        assert_eq!(
            rows,
            vec![
                serde_json::json!({"id": id.to_string(), "revision": 1, "name": "Squat"})
                    .as_object()
                    .unwrap()
                    .clone()
            ]
        );

        for refused in [
            "DELETE FROM gym.exercises",
            "UPDATE gym.exercises SET revision = 2",
            "INSERT INTO gym.exercises VALUES ('x', 1, 'now', '{}')",
            "DROP TABLE gym.exercises",
            "CREATE TABLE gym.notes (id TEXT)",
            "CREATE TEMP TABLE notes (id TEXT)",
            "ATTACH DATABASE 'stolen.db' AS stolen",
            "PRAGMA writable_schema = ON",
            "PRAGMA temp_store = FILE",
            "PRAGMA temp_store",
            "BEGIN",
            "SELECT 1; DELETE FROM gym.exercises",
            "SELECT 1; SELECT 2",
            "",
        ] {
            assert!(tables.query(refused).is_err(), "{refused:?} was allowed");
        }
        assert!(
            tables
                .query(
                    "WITH RECURSIVE forever(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM forever)
                     SELECT count(*) FROM forever"
                )
                .is_err()
        );
        assert_eq!(
            tables
                .query("SELECT count(*) AS rows FROM gym.exercises")
                .unwrap()[0]["rows"],
            1
        );
        let temp_store: i64 = tables
            .connection
            .query_row("PRAGMA temp_store", [], |row| row.get(0))
            .unwrap();
        assert_eq!(temp_store, 2, "sorts and temporary tables stay in memory");
    }
}
