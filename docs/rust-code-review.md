# Rust Code Review Notes

Threat model: the server is not trusted with plaintext. Clients perform auth
key derivation, encryption, decryption, signing, local cache updates, and sync
state management through shared Rust code. The browser, Tauri, and React Native
frontends should remain thin adapters over that Rust surface.

## Current Focus Areas

- Server auth blobs and access-key salts must remain AEAD-wrapped at rest with
  the configured server pepper.
- Server handlers must scope private data by authenticated `user_id`.
- `event_log.seq` must remain strictly increasing and unique; it is the sync
  cursor, not a database autoincrement.
- Clipboard and file object envelopes must preserve authenticated metadata,
  payload hash verification, and client-side encryption.
- Browser, Tauri, and mobile adapters should not duplicate auth, crypto, or sync
  logic in TypeScript.
- Native desktop file and clipboard operations should stay isolated to Tauri
  plugins and commands, with shared UI in `web/src`.
- Mobile file and clipboard operations should stay isolated to React Native
  platform APIs, with sync/auth state crossing UniFFI as records.

## Checks

```sh
cargo check --workspace
cargo test --workspace
nix run .#wasm-check
nix run .#web-check
nix run .#mobile-check
nix run .#tauri-build -- --no-bundle
```
