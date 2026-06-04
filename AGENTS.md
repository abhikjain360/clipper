# Agent Notes

## Environment

- Use the project environment from the checked-in `.envrc`. The shell hooks
  load the flake tools automatically, so run commands directly from the repo.
- If the environment has not been allowed yet, run `direnv allow` once. Do not
  wrap routine commands in `nix develop`.
- The flake provides Rust from fenix, Node 24, pnpm, wasm-pack, Tauri desktop
  build dependencies, React Native/UniFFI codegen helpers, `cargo-edit`,
  `cargo-udeps`, `sea-orm-cli`, `osv-scanner`, `nixfmt`, and the supporting
  C/C++ toolchain pieces. Rust comes from
  [fenix](https://github.com/nix-community/fenix): the stable channel is the
  default toolchain on `$PATH`, with `rustfmt`, `clippy`, `rust-src`,
  `rust-analyzer`, and the browser wasm plus Android `rust-std` targets; the
  pinned nightly is exposed at `$CLIPPER_RUST_NIGHTLY_BIN` for unstable rustfmt
  options. Bump either toolchain by editing `rustStableDate` /
  `rustNightlyDate` in `flake.nix` and supplying the new `nix-prefetch-url`
  manifest hash.
- The intent is not to make every native platform detail pure Nix. Use the flake
  for CLI, dependency, codegen, and build tooling. OS-tied signing and packaging
  details, plus Android SDK/NDK and emulators, can come from the host system
  where needed.

## Common Commands

- Format everything: `nix run .#fmt`
  - Do not run `cargo fmt` directly in this repo. `rustfmt.toml` uses unstable
    nightly-only options, so formatting must go through the flake wrapper,
    which selects the pinned nightly toolchain before invoking Cargo.
  - For Rust-only formatting, use `nix run .#rustfmt` rather than `cargo fmt`.
- Dependency vulnerability scan: `nix run .#audit`
- Unused Rust dependency scan: `nix run .#udeps`
- Rust workspace check: `cargo check --workspace`
- Rust tests: `cargo test --workspace`
- Browser wasm adapter check: `nix run .#wasm-check`
- Web lint/type check: `nix run .#web-check`
- Build the standalone web client: `nix run .#web-build`
- Serve the standalone web client locally: `nix run .#web-serve`
  - These wrappers regenerate the `crates/web-wasm` package before invoking
    Vite.
- Run the Tauri desktop shell: `nix run .#tauri-dev`
- Build the Tauri desktop shell: `nix run .#tauri-build`
  - These wrappers install the pnpm workspace from `pnpm-lock.yaml`, use the
    flake Rust toolchain, regenerate the browser wasm package for Vite, and
    invoke the Tauri CLI from `web/package.json`.
- Mobile lint/type check: `nix run .#mobile-check`
- Generate the Android React Native UniFFI bridge:
  `nix run .#mobile-uniffi-android`
- Start the Expo development server: `nix run .#mobile-start`
- Run the Android React Native app: `nix run .#mobile-android`

- Local server setup and serve:

  ```sh
  mkdir -p data
  test -f data/clipper-server.secret || \
    cargo run -p clipper-server -- generate-secret > data/clipper-server.secret
  chmod 600 data/clipper-server.secret

  export CLIPPER_SERVER_SECRET_FILE="$PWD/data/clipper-server.secret"
  cargo run -p clipper-server -- init --data-dir data/clipper-server
  cargo run -p clipper-server -- serve --data-dir data/clipper-server
  ```

  Keep the same `CLIPPER_SERVER_SECRET_FILE` value for `init`, `serve`, and
  `add-access-key`; the server database cannot be opened with a different
  pepper.

- Regenerate SeaORM entities after server schema changes:

  ```sh
  nix run .#server-entities
  ```

## Dependency Notes

- Use typed Rust errors with `thiserror`. Binary entrypoints should log via
  `tracing` and exit with typed error codes where useful; do not force stderr
  output with `eprintln!`.
- Use the configured logger (`tracing` in Rust code) for diagnostics instead of
  direct `println!`, `eprintln!`, or `dbg!` calls.
- `sha2` must stay on the `0.10` line while `opaque-ke` depends on the `digest`
  0.10 trait ecosystem.

## Boundaries

- Shared HTTP/WebSocket payloads live in `crates/api-types`.
- Daemon IPC types live in `crates/daemon-types`.
- Display-ready app state lives in `crates/app-types`.
- Browser wasm bindings live in `crates/web-wasm`.
- Tauri desktop commands live in `web/src-tauri`.
- Shared browser/native React UI lives in `web/src`.
- React Native mobile UI lives in `mobile/src` and uses Tamagui as well.
- Shared frontend contracts live in `packages/shared`.
- React Native UniFFI package glue lives in `packages/mobile-bridge`.
- Mobile UniFFI bindings are exported by `crates/mobile-uniffi`; app-visible
  records should be derived on `crates/app-types` types where practical.
- Server schema changes live in `crates/server/src/migration/*.rs`; keep SeaORM
  entities aligned by regenerating them with `sea-orm-cli`. Do not hand-edit
  generated entity files as the final change.
- `event_log.seq` is an application-assigned monotonic microsecond timestamp
  (see `AppState::next_event_seq`), not a database autoincrement. It is the
  sync cursor, so it must stay strictly increasing and unique; allocate it only
  while the surrounding transaction already holds the write lock so seq order
  matches commit order.
- This project is not deployed anywhere yet. Do not preserve legacy schema, API,
  ciphertext, or local-storage compatibility unless explicitly asked; prefer
  coherent current design over compatibility migrations for abandoned local
  state.
- Auth is multi-user: access keys are one-time registration invites stored as
  hashes, while user passphrases must only flow through OPAQUE registration and
  login. Server handlers must scope private data by authenticated `user_id`.
- Server auth blobs (`users.opaque_server_setup`,
  `users.opaque_password_file`, `users.encryption_salt`,
  `server_config.access_key_hash_salt`) are AEAD-wrapped at rest with a pepper
  sourced from `CLIPPER_SERVER_SECRET` / `CLIPPER_SERVER_SECRET_FILE`. Use
  `crates/server/src/secret_storage.rs` helpers at the storage boundary - never
  insert plaintext into those columns. Access-key hashes use the same pepper as
  Argon2's `secret`. See `docs/server-secret.md` for ops, `docs/opaque.md` for
  the wrap layer in relation to OPAQUE.
