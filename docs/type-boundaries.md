# Type Boundaries

Clipper keeps shared contracts in small Rust crates and small TypeScript
packages so the browser client, Tauri desktop shell, React Native mobile app,
daemon, and server do not drift.

## Crates

- `crates/api-types`: HTTP and WebSocket payloads shared by server and clients.
- `crates/app-types`: display-ready decrypted app state used by `clipper-client`,
  daemon state events, wasm bindings, and Tauri commands.
- `crates/daemon-types`: local daemon IPC protocol types.
- `crates/client`: shared encrypted sync engine and local plaintext cache.
- `crates/web-wasm`: wasm-bindgen adapter for the browser runtime.
- `crates/mobile-uniffi`: UniFFI adapter for React Native mobile.
- `web/src-tauri`: Tauri command adapter for the native desktop runtime.

## Packages

- `packages/shared`: TypeScript frontend contracts shared by web, Tauri, and
  mobile.
- `packages/mobile-bridge`: React Native package glue around generated UniFFI
  bindings.

## Frontend

- `web/src` owns shared React UI/components for browser and native desktop.
- `mobile/src` owns React Native mobile UI/components.
- `packages/shared` owns the frontend backend contract.
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
- Server schema changes belong in `crates/server/src/migration/*.rs`, followed
  by `nix run .#server-entities`.
- Do not duplicate encryption, auth, or sync logic in frontend TypeScript.
  Browser, Tauri, and mobile frontends should call the shared Rust client paths.
