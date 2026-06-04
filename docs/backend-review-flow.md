# Backend Review Flow

Use this map when reviewing changes that cross server, sync, daemon, browser,
native desktop, or mobile code.

## Runtime Paths

- Browser: `web/src` React UI -> `web/src/backend/wasm.ts` ->
  `crates/web-wasm` -> `clipper-client` -> server.
- Native desktop: `web/src` React UI -> `web/src/backend/tauri.ts` ->
  `web/src-tauri` commands -> `clipper-client` -> server.
- Mobile Android: `mobile/src` React Native UI -> `packages/mobile-bridge` ->
  `crates/mobile-uniffi` -> `clipper-client` -> server.
- Local daemon: daemon IPC types in `crates/daemon-types`; daemon implementation
  in `crates/daemon`.

Browser and native desktop share the same components and screens under
`web/src`. They split only at the backend adapter boundary. React Native mobile
uses separate native components under `mobile/src`, but shares frontend
contracts in `packages/shared` and app-visible Rust records in
`crates/app-types`.

## Review Checklist

- Identify which boundary changed: server API, shared app state, daemon IPC,
  browser wasm adapter, Tauri command adapter, UniFFI bridge, or React UI.
- For auth changes, preserve the invite-key/OPAQUE boundary: access keys are
  registration invites; passphrases only flow through OPAQUE registration/login.
- For private data changes, verify every server handler scopes data by
  authenticated `user_id`.
- For encrypted object changes, verify metadata and payload encryption stays in
  `clipper-client` and server-visible payloads remain ciphertext.
- For app state changes, update `crates/app-types` and both frontend adapters.
- For schema changes, update migrations and regenerate SeaORM entities with
  `nix run .#server-entities`.
- For UI changes, keep browser and Tauri behavior shared unless the operation is
  genuinely platform-specific, such as file dialogs or clipboard APIs.

## Useful Checks

```sh
cargo check --workspace
cargo test --workspace
nix run .#wasm-check
nix run .#web-check
nix run .#mobile-check
nix run .#tauri-build -- --no-bundle
```
