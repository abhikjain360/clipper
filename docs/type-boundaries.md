# Type Boundaries

Use this map before adding or changing shared types. The goal is one owner for
each layer, with compatibility re-exports only where they keep old imports
working.

## Database Domain

Owner:

- `crates/server/src/migration/m20260312_000001_create_tables.rs`

Generated/derived:

- `crates/server/src/entity/*.rs`

Rules:

- Change table, column, relation, and constraint definitions in the migration.
- Regenerate SeaORM entities from a migrated SQLite database.
- Do not make entity files the source of truth for schema decisions.

## Server <-> Client API

Owner:

- `crates/api-types/src/lib.rs`

Compatibility re-export:

- `crates/core/src/models.rs`

Consumers:

- `crates/server`
- `crates/client`

Rules:

- HTTP and WebSocket JSON payloads live in `clipper-api-types`.
- Database entities, decrypted UI state, and daemon IPC commands do not belong
  in this crate.
- `Argon2Params` lives here because it is sent in server API responses and also
  drives client/server crypto helpers.

## Daemon <-> Client IPC

Owner:

- `crates/daemon-types/src/protocol.rs`

Consumers:

- `crates/daemon`
- `app/rust`

Rules:

- Requests use `DaemonRequest` plus typed `DaemonCommand` variants.
- Response payloads use typed result structs such as `CopyToLocalResult` and
  `UploadFileResult` before being wrapped in `DaemonResponse`.
- Stream lines are parsed as `DaemonLine`; bridge code must not maintain its
  own response/event union.

## Client <-> UI State

Owner:

- `crates/app-types/src/lib.rs`

Compatibility re-export:

- `crates/daemon-types/src/state.rs`

Consumers:

- `crates/client`
- `crates/daemon-types`
- `app/rust`

Rules:

- Decrypted/display-ready state lives in `clipper-app-types`.
- The Flutter Rust Bridge `Bridge*` structs in `app/rust/src/api/clipper.rs`
  are adapter/codegen types only.
- Bridge conversions should destructure `clipper-app-types` structs
  exhaustively so app-state schema changes fail compilation until the UI
  boundary is updated.
