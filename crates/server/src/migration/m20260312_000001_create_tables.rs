use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260312_000001_create_tables"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Use raw SQL for SQLite-specific CHECK constraints
        let db = manager.get_connection();

        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS server_config (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                auth_salt BLOB NOT NULL,
                auth_hash BLOB NOT NULL,
                enc_salt BLOB NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .await?;

        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                token_hash BLOB NOT NULL UNIQUE,
                device_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL,
                user_agent TEXT,
                ip_addr TEXT
            )",
        )
        .await?;

        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS clipboard_items (
                id TEXT PRIMARY KEY,
                ciphertext_path TEXT NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext_size INTEGER NOT NULL,
                sha256_ciphertext BLOB NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                source_device_id TEXT NOT NULL
            )",
        )
        .await?;

        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS files (
                id TEXT PRIMARY KEY,
                blob_path TEXT NOT NULL,
                meta_ciphertext BLOB NOT NULL,
                meta_nonce BLOB NOT NULL,
                blob_nonce BLOB NOT NULL,
                blob_size INTEGER NOT NULL,
                sha256_ciphertext BLOB NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                source_device_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending'
            )",
        )
        .await?;

        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS devices (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                platform TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL
            )",
        )
        .await?;

        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS event_log (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                object_kind TEXT NOT NULL,
                object_id TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS event_log").await?;
        db.execute_unprepared("DROP TABLE IF EXISTS devices").await?;
        db.execute_unprepared("DROP TABLE IF EXISTS files").await?;
        db.execute_unprepared("DROP TABLE IF EXISTS clipboard_items").await?;
        db.execute_unprepared("DROP TABLE IF EXISTS sessions").await?;
        db.execute_unprepared("DROP TABLE IF EXISTS server_config").await?;
        Ok(())
    }
}
