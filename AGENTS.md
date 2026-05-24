# Agent Notes

## Environment

- Use the project environment from the checked-in `.envrc`. The shell hooks
  load the flake tools automatically, so run commands directly from the repo.
- If the environment has not been allowed yet, run `direnv allow` once. Do not
  wrap routine commands in `nix develop`.
- The flake provides Flutter, Dart, Java, Android helper environment, C/C++
  toolchain pieces, `cargo-edit`, `flutter_rust_bridge_codegen`, CocoaPods,
  `sea-orm-cli`, and `rustup`. Rust itself is intentionally the rustup
  `stable` toolchain under `~/.rustup`, installed/configured by the flake shell
  hook.
- The intent is not to make every mobile platform detail pure Nix. Use the
  flake for CLI, codegen, dependency, and build tooling. Emulators, Xcode, host
  Android SDK/NDK installs, signing, and other OS-tied platform setup can come
  from the host system; the flake discovers those where practical.
- If Android Gradle picks up the wrong Flutter SDK, run `cd app && flutter pub
  get` to refresh `app/android/local.properties` with the flake Flutter path.

## Common Commands

- Rust workspace check: `cargo check --workspace`
- Rust tests: `cargo test --workspace`
- Flutter dependencies: `cd app && flutter pub get`
- Flutter analysis/tests: `cd app && flutter analyze && flutter test`
- Regenerate Flutter Rust Bridge after Rust API changes:

  ```sh
  cd app && flutter_rust_bridge_codegen generate
  ```

- Regenerate SeaORM entities after server schema changes:

  ```sh
  tmpdir=$(mktemp -d)
  cargo run -q -p clipper-server -- init -d "$tmpdir/data"
  sea-orm-cli generate entity -u "sqlite:$tmpdir/data/clipper.db" -o crates/server/src/entity --with-prelude none
  rm -rf "$tmpdir"
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
- Auth is multi-user: access keys are one-time registration invites stored as
  hashes, while user passphrases must only flow through OPAQUE registration and
  login. Server handlers must scope private data by authenticated `user_id`.
- When auth state or auth commands cross the Flutter Rust Bridge, update the
  bridge adapter and regenerate FRB output so Dart, daemon IPC, and Rust stay
  aligned.
