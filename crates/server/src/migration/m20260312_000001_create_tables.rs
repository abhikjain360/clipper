use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ServerConfig::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ServerConfig::Id)
                            .integer()
                            .not_null()
                            .primary_key()
                            .check(Expr::col(ServerConfig::Id).eq(1)),
                    )
                    .col(ColumnDef::new(ServerConfig::CreatedAt).text().not_null())
                    .col(ColumnDef::new(ServerConfig::UpdatedAt).text().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(AccessKeys::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AccessKeys::KeyHash)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(AccessKeys::CreatedAt).text().not_null())
                    .col(ColumnDef::new(AccessKeys::ExpiresAt).text())
                    .col(ColumnDef::new(AccessKeys::UsedAt).text())
                    .col(ColumnDef::new(AccessKeys::UsedByUserId).uuid())
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Users::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Users::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Users::OpaqueServerSetup).blob().not_null())
                    .col(ColumnDef::new(Users::OpaquePasswordFile).blob().not_null())
                    .col(ColumnDef::new(Users::EncryptionSalt).blob().not_null())
                    .col(
                        ColumnDef::new(Users::AccessKeyHash)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Users::CreatedAt).text().not_null())
                    .col(ColumnDef::new(Users::UpdatedAt).text().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_users_access_key_hash")
                            .from(Users::Table, Users::AccessKeyHash)
                            .to(AccessKeys::Table, AccessKeys::KeyHash)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Devices::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Devices::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Devices::UserId).uuid().not_null())
                    .col(ColumnDef::new(Devices::Name).text().not_null())
                    .col(ColumnDef::new(Devices::Platform).text().not_null())
                    .col(ColumnDef::new(Devices::CreatedAt).text().not_null())
                    .col(ColumnDef::new(Devices::UpdatedAt).text().not_null())
                    .col(ColumnDef::new(Devices::LastSeenAt).text().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_devices_user_id")
                            .from(Devices::Table, Devices::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Sessions::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Sessions::Id).uuid().not_null().primary_key())
                    .col(
                        ColumnDef::new(Sessions::TokenHash)
                            .blob()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Sessions::UserId).uuid().not_null())
                    .col(ColumnDef::new(Sessions::DeviceId).uuid().not_null())
                    .col(ColumnDef::new(Sessions::CreatedAt).text().not_null())
                    .col(ColumnDef::new(Sessions::ExpiresAt).text().not_null())
                    .col(ColumnDef::new(Sessions::LastSeenAt).text().not_null())
                    .col(ColumnDef::new(Sessions::UserAgent).text())
                    .col(ColumnDef::new(Sessions::IpAddr).text())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sessions_device_id")
                            .from(Sessions::Table, Sessions::DeviceId)
                            .to(Devices::Table, Devices::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sessions_user_id")
                            .from(Sessions::Table, Sessions::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ClipboardItems::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ClipboardItems::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ClipboardItems::UserId).uuid().not_null())
                    .col(
                        ColumnDef::new(ClipboardItems::CiphertextPath)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(ClipboardItems::Nonce).blob().not_null())
                    .col(
                        ColumnDef::new(ClipboardItems::CiphertextSize)
                            .big_integer()
                            .not_null()
                            .check(Expr::col(ClipboardItems::CiphertextSize).gte(0)),
                    )
                    .col(
                        ColumnDef::new(ClipboardItems::Sha256Ciphertext)
                            .blob()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ClipboardItems::CreatedAt).text().not_null())
                    .col(ColumnDef::new(ClipboardItems::ExpiresAt).text().not_null())
                    .col(
                        ColumnDef::new(ClipboardItems::SourceDeviceId)
                            .uuid()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_clipboard_items_source_device_id")
                            .from(ClipboardItems::Table, ClipboardItems::SourceDeviceId)
                            .to(Devices::Table, Devices::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_clipboard_items_user_id")
                            .from(ClipboardItems::Table, ClipboardItems::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Files::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Files::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Files::UserId).uuid().not_null())
                    .col(
                        ColumnDef::new(Files::BlobPath)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Files::MetaCiphertext).blob().not_null())
                    .col(ColumnDef::new(Files::MetaNonce).blob().not_null())
                    .col(ColumnDef::new(Files::BlobNonce).blob().not_null())
                    .col(
                        ColumnDef::new(Files::BlobSize)
                            .big_integer()
                            .not_null()
                            .check(Expr::col(Files::BlobSize).gte(0)),
                    )
                    .col(ColumnDef::new(Files::Sha256Ciphertext).blob().not_null())
                    .col(ColumnDef::new(Files::CreatedAt).text().not_null())
                    .col(ColumnDef::new(Files::UpdatedAt).text().not_null())
                    .col(ColumnDef::new(Files::SourceDeviceId).uuid().not_null())
                    .col(
                        ColumnDef::new(Files::Status)
                            .text()
                            .not_null()
                            .default("pending")
                            .check(Expr::col(Files::Status).is_in([
                                "pending",
                                "uploading",
                                "uploaded",
                                "complete",
                            ])),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_files_source_device_id")
                            .from(Files::Table, Files::SourceDeviceId)
                            .to(Devices::Table, Devices::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_files_user_id")
                            .from(Files::Table, Files::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Objects::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Objects::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Objects::UserId).uuid().not_null())
                    .col(
                        ColumnDef::new(Objects::Kind)
                            .text()
                            .not_null()
                            .check(Expr::col(Objects::Kind).is_in(["clipboard", "file"])),
                    )
                    .col(ColumnDef::new(Objects::MetaCiphertext).blob().not_null())
                    .col(ColumnDef::new(Objects::MetaNonce).blob().not_null())
                    .col(ColumnDef::new(Objects::CreatedAt).text().not_null())
                    .col(ColumnDef::new(Objects::UpdatedAt).text().not_null())
                    .col(ColumnDef::new(Objects::SourceDeviceId).uuid().not_null())
                    .col(
                        ColumnDef::new(Objects::Status)
                            .text()
                            .not_null()
                            .default("pending")
                            .check(Expr::col(Objects::Status).is_in(["pending", "complete"])),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_objects_source_device_id")
                            .from(Objects::Table, Objects::SourceDeviceId)
                            .to(Devices::Table, Devices::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_objects_user_id")
                            .from(Objects::Table, Objects::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ObjectPayloads::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(ObjectPayloads::ObjectId).uuid().not_null())
                    .col(ColumnDef::new(ObjectPayloads::PayloadId).text().not_null())
                    .col(
                        ColumnDef::new(ObjectPayloads::CiphertextPath)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(ObjectPayloads::Nonce).blob().not_null())
                    .col(
                        ColumnDef::new(ObjectPayloads::CiphertextSize)
                            .big_integer()
                            .not_null()
                            .check(Expr::col(ObjectPayloads::CiphertextSize).gte(0)),
                    )
                    .col(
                        ColumnDef::new(ObjectPayloads::Sha256Ciphertext)
                            .blob()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ObjectPayloads::CreatedAt).text().not_null())
                    .col(ColumnDef::new(ObjectPayloads::UpdatedAt).text().not_null())
                    .col(
                        ColumnDef::new(ObjectPayloads::Status)
                            .text()
                            .not_null()
                            .default("pending")
                            .check(Expr::col(ObjectPayloads::Status).is_in([
                                "pending",
                                "uploading",
                                "uploaded",
                                "complete",
                            ])),
                    )
                    .primary_key(
                        Index::create()
                            .name("pk_object_payloads")
                            .col(ObjectPayloads::ObjectId)
                            .col(ObjectPayloads::PayloadId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_object_payloads_object_id")
                            .from(ObjectPayloads::Table, ObjectPayloads::ObjectId)
                            .to(Objects::Table, Objects::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(EventLog::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EventLog::Seq)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(EventLog::UserId).uuid().not_null())
                    .col(ColumnDef::new(EventLog::EventType).text().not_null())
                    .col(
                        ColumnDef::new(EventLog::ObjectKind)
                            .text()
                            .not_null()
                            .check(Expr::col(EventLog::ObjectKind).is_in(["clipboard", "file"])),
                    )
                    .col(ColumnDef::new(EventLog::ObjectId).uuid().not_null())
                    .col(ColumnDef::new(EventLog::CreatedAt).text().not_null())
                    .check(
                        Cond::any()
                            .add(
                                Cond::all()
                                    .add(Expr::col(EventLog::EventType).eq("clipboard.created"))
                                    .add(Expr::col(EventLog::ObjectKind).eq("clipboard")),
                            )
                            .add(
                                Cond::all()
                                    .add(Expr::col(EventLog::EventType).eq("file.created"))
                                    .add(Expr::col(EventLog::ObjectKind).eq("file")),
                            )
                            .add(
                                Cond::all()
                                    .add(Expr::col(EventLog::EventType).eq("file.deleted"))
                                    .add(Expr::col(EventLog::ObjectKind).eq("file")),
                            )
                            .into(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_event_log_user_id")
                            .from(EventLog::Table, EventLog::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(EventLog::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ObjectPayloads::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(Objects::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Files::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardItems::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(Sessions::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Devices::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Users::Table).if_exists().to_owned())
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(AccessKeys::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ServerConfig::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ServerConfig {
    Table,
    Id,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum AccessKeys {
    Table,
    KeyHash,
    CreatedAt,
    ExpiresAt,
    UsedAt,
    UsedByUserId,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    OpaqueServerSetup,
    OpaquePasswordFile,
    EncryptionSalt,
    AccessKeyHash,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Sessions {
    Table,
    Id,
    TokenHash,
    UserId,
    DeviceId,
    CreatedAt,
    ExpiresAt,
    LastSeenAt,
    UserAgent,
    IpAddr,
}

#[derive(DeriveIden)]
enum ClipboardItems {
    Table,
    Id,
    UserId,
    CiphertextPath,
    Nonce,
    CiphertextSize,
    Sha256Ciphertext,
    CreatedAt,
    ExpiresAt,
    SourceDeviceId,
}

#[derive(DeriveIden)]
enum Files {
    Table,
    Id,
    UserId,
    BlobPath,
    MetaCiphertext,
    MetaNonce,
    BlobNonce,
    BlobSize,
    Sha256Ciphertext,
    CreatedAt,
    UpdatedAt,
    SourceDeviceId,
    Status,
}

#[derive(DeriveIden)]
enum Objects {
    Table,
    Id,
    UserId,
    Kind,
    MetaCiphertext,
    MetaNonce,
    CreatedAt,
    UpdatedAt,
    SourceDeviceId,
    Status,
}

#[derive(DeriveIden)]
enum ObjectPayloads {
    Table,
    ObjectId,
    PayloadId,
    CiphertextPath,
    Nonce,
    CiphertextSize,
    Sha256Ciphertext,
    CreatedAt,
    UpdatedAt,
    Status,
}

#[derive(DeriveIden)]
enum Devices {
    Table,
    Id,
    UserId,
    Name,
    Platform,
    CreatedAt,
    UpdatedAt,
    LastSeenAt,
}

#[derive(DeriveIden)]
enum EventLog {
    Table,
    Seq,
    UserId,
    EventType,
    ObjectKind,
    ObjectId,
    CreatedAt,
}
