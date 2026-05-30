# Agent Notes

## Environment

- Use the project environment from the checked-in `.envrc`. The shell hooks
  load the flake tools automatically, so run commands directly from the repo.
- If the environment has not been allowed yet, run `direnv allow` once. Do not
  wrap routine commands in `nix develop`.
- The flake provides Flutter, Dart, Java, Android helper environment, C/C++
  toolchain pieces, `cargo-edit`, `cargo-udeps`,
  `flutter_rust_bridge_codegen`, CocoaPods, `sea-orm-cli`, `osv-scanner`,
  `wasm-pack`, and `nixfmt`. Rust comes from
  [fenix](https://github.com/nix-community/fenix): the stable channel (the
  default toolchain on `$PATH`, with `rustfmt`, `clippy`, `rust-src`,
  `rust-analyzer`, and the Android + `wasm32-unknown-unknown` `rust-std`
  targets) and a pinned nightly (exposed at `$CLIPPER_RUST_NIGHTLY_BIN`) for
  unstable rustfmt options and Flutter Rust Bridge web builds. Bump either
  toolchain by editing `rustStableDate` / `rustNightlyDate` in `flake.nix`
  and supplying the new `nix-prefetch-url` manifest hash.
- Cargokit's Android plugin build (`app/rust_builder/cargokit`) is patched to
  prefer the fenix toolchain via `CARGOKIT_CARGO` / `CARGOKIT_RUSTC` instead
  of calling `rustup`. If you re-vendor cargokit, reapply that patch — see
  `app/rust_builder/cargokit/PATCHES.md`.
- The intent is not to make every mobile platform detail pure Nix. Use the
  flake for CLI, codegen, dependency, and build tooling. Emulators, Xcode, host
  Android SDK/NDK installs, signing, and other OS-tied platform setup can come
  from the host system; the flake discovers those where practical.
- If Android Gradle picks up the wrong Flutter SDK, run `cd app && flutter pub
  get` to refresh `app/android/local.properties` with the flake Flutter path.

## Common Commands

- Format everything: `nix run .#fmt`
- Dependency vulnerability scan: `nix run .#audit`
- Unused Rust dependency scan: `nix run .#udeps`
- Rust workspace check: `cargo check --workspace`
- Rust tests: `cargo test --workspace`
- WASM bridge check: `nix run .#wasm-check`
- Flutter dependencies: `cd app && flutter pub get`
- Flutter analysis/tests: `cd app && flutter analyze && flutter test`
- Regenerate Flutter Rust Bridge after Rust API changes:

  ```sh
  nix run .#frb-generate
  ```

- Rebuild the Flutter Rust Bridge web package: `nix run .#frb-build-web`
- Build the Flutter web client: `nix run .#web-build`
- Serve the Flutter web client locally: `nix run .#web-serve`
  - Flutter Rust Bridge requires shared-memory wasm and cross-origin isolation
    for the wasm worker. Use these wrappers instead of generic build or static
    file server commands.
- Build the macOS Flutter app: `nix run .#macos-build`
  - This wrapper is Darwin-only. It uses host Flutter/Xcode for app packaging
    because Flutter mutates copied macOS engine framework artifacts during the
    build, but keeps Rust on the flake-provided fenix toolchain through
    `CARGOKIT_CARGO` / `CARGOKIT_RUSTC`.
  - It defaults `FLUTTER_SWIFT_PACKAGE_MANAGER=false` so newer host Flutter
    versions do not rewrite the current CocoaPods-based macOS project.

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

- Keep Rust and Dart `flutter_rust_bridge` versions aligned. After changing
  either side, regenerate FRB files and check both Rust and Flutter.
- The app Rust crate disables FRB's own `anyhow` feature while keeping
  logging/user utilities enabled. `anyhow` can still appear transitively through
  FRB's `allo-isolate` dependency on non-wasm targets; do not add direct
  `anyhow` dependencies in repo crates.
- Use typed Rust errors with `thiserror`. Binary entrypoints should log via
  `tracing` and exit with typed error codes where useful; do not force stderr
  output with `eprintln!`.
- Use the configured logger (`tracing` in Rust code) for diagnostics instead
  of direct `println!`, `eprintln!`, or `dbg!` calls.
- `sha2` must stay on the `0.10` line while `opaque-ke` depends on the
  `digest` 0.10 trait ecosystem.
- `app/rust_builder/cargokit/build_tool` keeps runtime dependencies pinned
  because its bundle tool runner does not use `pubspec.lock`.

## Boundaries

- Shared HTTP/WebSocket payloads live in `crates/api-types`.
- Daemon IPC types live in `crates/daemon-types`.
- Display-ready app state lives in `crates/app-types`.
- Flutter Rust Bridge types in `app/rust/src/api/clipper.rs` are adapter types
  only. Keep conversions exhaustive so state schema changes fail at compile
  time.
- Server schema changes live in `crates/server/src/migration/*.rs`; keep
  SeaORM entities aligned by regenerating them with `sea-orm-cli`. Do not
  hand-edit generated entity files as the final change.
- `event_log.seq` is an application-assigned monotonic microsecond timestamp
  (see `AppState::next_event_seq`), not a database autoincrement. It is the
  sync cursor, so it must stay strictly increasing and unique; allocate it
  only while the surrounding transaction already holds the write lock so seq
  order matches commit order.
- This project is not deployed anywhere yet. Do not preserve legacy schema,
  API, ciphertext, or local-storage compatibility unless explicitly asked;
  prefer coherent current design over compatibility migrations for abandoned
  local state.
- Auth is multi-user: access keys are one-time registration invites stored as
  hashes, while user passphrases must only flow through OPAQUE registration and
  login. Server handlers must scope private data by authenticated `user_id`.
- Server auth blobs (`users.opaque_server_setup`,
  `users.opaque_password_file`, `users.encryption_salt`,
  `server_config.access_key_hash_salt`) are AEAD-wrapped at rest with a
  pepper sourced from `CLIPPER_SERVER_SECRET` / `CLIPPER_SERVER_SECRET_FILE`.
  Use `crates/server/src/secret_storage.rs` helpers at the storage
  boundary — never insert plaintext into those columns. Access-key
  hashes use the same pepper as Argon2's `secret`. See
  `docs/server-secret.md` for ops, `docs/opaque.md` for the wrap layer
  in relation to OPAQUE.
- When auth state or auth commands cross the Flutter Rust Bridge, update the
  bridge adapter and regenerate FRB output so Dart, daemon IPC, and Rust stay
  aligned.
