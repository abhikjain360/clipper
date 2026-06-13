# Type Boundaries

Clipper keeps shared contracts in small Rust crates and small TypeScript
packages so the browser client, Tauri desktop shell, React Native mobile app,
daemon, and server do not drift.

## Crates

These are the crates that define cross-component contracts or back the
frontend adapters:

- `crates/api-types`: HTTP and WebSocket payloads shared by server and clients.
- `crates/app-types`: display-ready decrypted app state used by `clipper-client`,
  daemon state events, wasm bindings, and Tauri commands. The `uniffi` feature
  derives UniFFI records/enums directly on these types for the mobile adapter.
- `crates/daemon-types`: local daemon IPC protocol types. Its state module is a
  compatibility re-export of `crates/app-types`, so daemon IPC and app state
  share one source of truth.
- `crates/client`: shared encrypted sync engine and local at-rest-encrypted
  object cache. Cached clipboard/file records hold only encrypted object memory;
  payload bytes are decrypted on demand from the local ciphertext, and the
  device signing identity is stored wrapped.
- `crates/web-wasm`: wasm-bindgen adapter (`cdylib`) for the browser runtime.
- `crates/mobile-uniffi`: UniFFI adapter (`cdylib`/`staticlib`/`rlib`) for React
  Native mobile.
- `web/src-tauri`: Tauri command adapter for the native desktop runtime.

The remaining workspace members are not shared contracts but are referenced by
the rules below:

- `crates/core`: crypto core (`crates/core/src/crypto.rs`) and shared models.
- `crates/server`: axum + SeaORM/SQLite server.
- `crates/daemon`: local daemon process.
- `crates/fs-txn`: filesystem rollback guard for file writes.

## Packages

- `packages/shared` (`@clipper/shared`): TypeScript frontend contracts shared by
  web, Tauri, and mobile.
- `packages/mobile-bridge` (`@clipper/mobile-bridge`): React Native package glue
  around generated UniFFI bindings (under `src/generated`).

## Frontend

- `web/src` owns shared React UI/components for browser and native desktop and
  uses Tamagui.
- `mobile/src` owns React Native mobile UI/components and uses Tamagui as well.
- `packages/shared` owns the frontend backend contract.
- `web/src/backend/index.ts` selects the backend at runtime (Tauri vs. wasm).
- `web/src/backend/wasm.ts` implements the browser backend through
  `crates/web-wasm`.
- `web/src/backend/tauri.ts` implements the native backend through Tauri
  commands.
- `packages/mobile-bridge/src` implements the mobile backend through UniFFI.

## Rules

- HTTP/WebSocket schema changes belong in `crates/api-types` first.
- Display state changes belong in `crates/app-types`; adapter conversions must
  fail loudly at compile time when fields change.
- App-visible UniFFI records should be derived on existing `crates/app-types`
  types where practical instead of duplicating mobile-only record structs.
- Daemon IPC protocol changes belong in `crates/daemon-types`.
- Server schema changes belong in `crates/server/src/migration/*.rs`, followed
  by `nix run .#server-entities`. Do not hand-edit generated SeaORM entity files
  as the final change.
- Do not duplicate encryption, auth, or sync logic in frontend TypeScript.
  Browser, Tauri, and mobile frontends should call the shared Rust client paths.
