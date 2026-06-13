# Rust Code Review Notes

Threat model: the server is not trusted with plaintext. Clients perform auth
key derivation, encryption, decryption, signing, local cache updates, and sync
state management through shared Rust code. The browser, Tauri, and React Native
frontends should remain thin adapters over that Rust surface.

## Current Focus Areas

- Server auth blobs and access-key salts must remain AEAD-wrapped at rest with
  the configured server pepper. Storage-boundary helpers in
  `crates/server/src/secret_storage.rs` are the only sanctioned way in and out
  of those columns; never insert plaintext.
- Server handlers must scope private data by authenticated `user_id`.
- `event_log.seq` must remain strictly increasing and unique; it is the sync
  cursor, not a database autoincrement. It is allocated only while the
  surrounding transaction already holds the write lock so seq order matches
  commit order.
- Clipboard and file object envelopes must preserve authenticated metadata,
  payload hash verification, and client-side encryption.
- Client-side at-rest protection: the device signing key and cached objects are
  encrypted locally; review changes to the local store so cached plaintext is
  not reintroduced.
- Server-side abuse controls live in `crates/server/src/rate_limit.rs`
  (per-client and per-username limits) and
  `crates/server/src/storage_quota.rs` (per-user storage quotas); cheap
  rejections should happen before expensive work (e.g. OPAQUE, hashing,
  persistence).
- Registration must not leak whether a username already exists; duplicate
  registration is hidden rather than returning a distinguishable conflict.
- The daemon IPC socket must reject peers that are not the current user; the
  peer-uid check (`SO_PEERCRED` / `getpeereid`) lives in
  `crates/daemon/src/main.rs`.
- Browser, Tauri, and mobile adapters should not duplicate auth, crypto, or sync
  logic in TypeScript.
- Native desktop file and clipboard operations should stay isolated to Tauri
  plugins and commands, with shared UI in `web/src`.
- Mobile file and clipboard operations should stay isolated to React Native
  platform APIs, with sync/auth state crossing UniFFI as records.

## Conventions

- Crates target Rust edition 2024 via the workspace package settings; new
  crates inherit `edition.workspace = true`.
- Use typed errors with `thiserror`; surface failures as typed error enums
  rather than stringly-typed or `anyhow`-style erasure at crate boundaries.
- Diagnostics go through `tracing`, never `println!`, `eprintln!`, or `dbg!`.
  Binary entrypoints should log via `tracing` and exit with typed error codes
  where useful; do not force stderr output. (Writing actual program output to
  stdout — e.g. emitting a generated secret meant to be redirected to a file —
  is the only legitimate use of `println!`.)
- Do not run `cargo fmt` directly. `rustfmt.toml` enables nightly-only options
  (`group_imports`, `imports_granularity`), so formatting must go through the
  flake wrapper, which selects the pinned nightly toolchain: `nix run .#fmt`
  for everything or `nix run .#rustfmt` for Rust only.
- Do not hand-edit generated SeaORM entity files as the final change;
  regenerate them with `nix run .#server-entities` after schema changes.

## Checks

```sh
cargo check --workspace
cargo test --workspace
nix run .#fmt          # flake fmt wrapper; not bare `cargo fmt`
nix run .#audit        # dependency vulnerability scan
nix run .#udeps        # unused Rust dependency scan
nix run .#wasm-check
nix run .#web-check
nix run .#mobile-check
nix run .#tauri-build -- --no-bundle
```
