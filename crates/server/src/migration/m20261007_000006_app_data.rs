use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE app_data_rows (
                user_id UUID NOT NULL,
                row_key BLOB NOT NULL,
                revision BIGINT NOT NULL CHECK (revision > 0),
                sequence BIGINT NOT NULL UNIQUE CHECK (sequence > 0),
                deleted BOOLEAN NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL,
                device_id UUID,
                signature BLOB NOT NULL,
                received_at TEXT NOT NULL,
                PRIMARY KEY (user_id, row_key),
                CHECK (length(row_key) = 32),
                CHECK (length(signature) = 64),
                CHECK (length(nonce) = 24),
                CHECK (length(ciphertext) >= 16),
                FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE ON UPDATE CASCADE,
                FOREIGN KEY (device_id) REFERENCES devices (id) ON DELETE SET NULL ON UPDATE CASCADE
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX idx_app_data_rows_user_sequence ON app_data_rows (user_id, sequence)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE app_data_rows")
            .await?;
        Ok(())
    }
}
