# Contributing

## Pull Requests

I am not accepting pull requests right now.

Clipper is experimental in two related senses. The product is early: the final
feature set is not decided, the design is still moving, and the code has not
been security audited. The development process is also part of the experiment:
I am using this project to see how far I can write high-quality software while
relying heavily on agentic coding, without handing off the design judgment,
review responsibility, or maintenance ownership that quality still requires.

You are still welcome to read the code, run it locally, fork it, experiment, and
send feedback. This guide documents the development setup and checks so the
project is easy to work with even while upstream pull requests are closed.

## Project Status

Clipper should not be trusted with secrets you cannot afford to lose. See
[README.md](README.md), [SECURITY.md](SECURITY.md), and the known issues in
[docs/issues.md](docs/issues.md) before treating the code as anything other than
experimental software.

## Development Environment

Use the checked-in Nix flake through direnv:

```sh
direnv allow
```

After direnv has loaded the shell, run commands directly from the repository. Do
not wrap routine commands in `nix develop`.

The flake provides Rust, Node 24, pnpm, wasm-pack, Tauri desktop build
dependencies, React Native/UniFFI codegen helpers, SeaORM, OSV Scanner, and
formatting tools used by this repo. Android SDK/NDK installs and emulators may
come from the host.

Rust comes from [fenix](https://github.com/nix-community/fenix) as Nix
derivations, so there is no `~/.rustup` and no first-run download. The stable
channel is the default `cargo`/`rustc` on `$PATH`. A pinned nightly is exposed
at `$CLIPPER_RUST_NIGHTLY_BIN` for unstable rustfmt options. Each flake wrapper
selects its toolchain:

- The pinned nightly backs `.#fmt`, `.#rustfmt` and `.#udeps`.
- The stable channel backs every other Rust wrapper, including the build,
  serve, check, audit and entity wrappers.

Both channels are pinned by date and manifest hash in `flake.nix`
(`rustStableDate`, `rustNightlyDate`). To bump one, set the new date and run
`nix-prefetch-url --type sha256
https://static.rust-lang.org/dist/<date>/channel-rust-<channel>.toml` to get
the manifest hash.

Direnv can print a long environment diff for Nix shells. To hide only that diff,
put this in `~/.config/direnv/direnv.toml`:

```toml
[global]
hide_env_diff = true
```

## Repository Layout

- `crates/server` contains the Axum API and SQLite storage.
- `crates/client` contains the shared Rust sync client.
- `crates/schedule` contains the scheduling domain: series, recurrence
  expansion, calendar ingest and the alarm plan.
- `crates/core` contains the crypto core and shared models.
- `crates/fs-txn` contains the filesystem rollback guard for file writes.
- `crates/daemon` contains the local background process.
- `crates/api-types` contains shared HTTP and WebSocket payloads.
- `crates/daemon-types` contains daemon IPC types.
- `crates/app-types` contains display-ready app state.
- `crates/web-wasm` contains the browser wasm adapter.
- `crates/mobile-uniffi` contains the React Native UniFFI adapter.
- `packages/shared` contains shared frontend contracts.
- `packages/mobile-bridge` contains React Native UniFFI package glue.
- `web/src` contains the shared React UI for browser and native desktop.
- `web/src-tauri` contains the Tauri desktop shell and command adapter.
- `mobile/src` contains the React Native mobile UI.
- `docs` contains longer design and operations notes.

## Common Checks

Run the checks relevant to the change you are making. For broad changes, run the
full set:

```sh
nix run .#fmt
nix run .#audit
nix run .#udeps
cargo check --workspace
cargo test --workspace
nix run .#wasm-check
nix run .#js-check
nix run .#web-check
nix run .#mobile-check
cargo check -p clipper-desktop
nix run .#tauri-build -- --no-bundle
```

`nix run .#js-check` lints and type checks every TypeScript and JavaScript
source in the workspace. `nix run .#web-check` and `nix run .#mobile-check`
cover only the web and React Native packages.

Do not run `cargo fmt` directly. This repository's `rustfmt.toml` uses
nightly-only rustfmt options, so formatting must go through the flake wrapper:

```sh
nix run .#fmt
```

For Rust-only formatting, use:

```sh
nix run .#rustfmt
```

## Continuous Integration

CI lives in `.github/workflows/ci.yml` and does not run on every pull request
push. A `gate` job decides whether the other jobs run. It lets them run only
when:

- the run was started by hand (`workflow_dispatch`), or
- a pull request gets the `run-ci` label, or
- a pull request is merged (a run on `main` after the merge).

When the gate passes, these jobs run:

- `format`: `nix run .#fmt`, then fails if formatting changed any file.
- `rust`: clippy with `-D warnings`, `cargo test --workspace`, and the live
  scheduler sync regression, which builds `clipper-server` and runs the
  ignored `live_schedule` tests in `clipper-client`.
- `wasm-check`: `nix run .#wasm-check`.
- `udeps`: `nix run .#udeps`.
- `audit`: `nix run .#audit`.
- `web-native`: `nix run .#web-check`, `nix run .#mobile-check` and
  `nix run .#tauri-build -- --no-bundle`.

Run the local checks yourself. Do not assume a pull request has a green CI run
unless it has the `run-ci` label or has been merged.

## Runtime Paths

Each client reaches the server through the shared Rust client, `clipper-client`:

- Browser: `web/src` React UI -> `web/src/backend/wasm.ts` ->
  `crates/web-wasm` -> `clipper-client` -> server.
- Native desktop: `web/src` React UI -> `web/src/backend/tauri.ts` ->
  `web/src-tauri` commands -> `clipper-daemon` over local IPC ->
  `clipper-client` -> server.
- Android: `mobile/src` React Native UI -> `packages/mobile-bridge` ->
  `crates/mobile-uniffi` -> `clipper-client` -> server.
- Local daemon: the IPC types are in `crates/daemon-types` and the daemon is in
  `crates/daemon`.

The browser and native desktop clients share the React components and screens
in `web/src`. They split only at the backend adapter boundary, and
`web/src/backend/index.ts` picks the adapter at runtime. React Native mobile has
its own components in `mobile/src`, but shares frontend contracts in
`packages/shared` and app-visible Rust records in `crates/app-types`.

Use the project wrappers:

```sh
nix run .#web-build
nix run .#web-serve
nix run .#tauri-dev
nix run .#tauri-build
nix run .#mobile-start
nix run .#mobile-uniffi-android
nix run .#mobile-android
```

## Local Server

Create a local data directory and server secret:

```sh
mkdir -p data
test -f data/clipper-server.secret || \
  cargo run -p clipper-server -- generate-secret > data/clipper-server.secret
chmod 600 data/clipper-server.secret
```

Initialize and run the server:

```sh
export CLIPPER_SERVER_SECRET_FILE="$PWD/data/clipper-server.secret"
cargo run -p clipper-server -- init --data-dir data/clipper-server
cargo run -p clipper-server -- serve --data-dir data/clipper-server
```

Keep the same `CLIPPER_SERVER_SECRET_FILE` value for `init`, `serve`, and
`add-access-key`. The server database cannot be opened with a different pepper.

Create a one-time registration access key after initialization:

```sh
export CLIPPER_SERVER_SECRET_FILE="$PWD/data/clipper-server.secret"
ACCESS_KEY="$(openssl rand -base64 32)"
echo "Access key: $ACCESS_KEY"
cargo run -p clipper-server -- add-access-key \
  --data-dir data/clipper-server \
  --access-key "$ACCESS_KEY"
```

The app defaults to `http://127.0.0.1:8787`.

## Schema And Generated Code

Server schema changes live in `crates/server/src/migration/*.rs`. Regenerate
SeaORM entities after changing the schema:

```sh
nix run .#server-entities
```

Do not hand-edit generated entity files as the final change.

## Coding Notes

- Use typed Rust errors with `thiserror`.
- Use `tracing` for Rust diagnostics rather than `println!`, `eprintln!`, or
  `dbg!`.
- Do not add direct `anyhow` dependencies in repo crates.
- Keep `sha2` on the `0.10` line while `opaque-ke` depends on the `digest` 0.10
  trait ecosystem.
- Scope server private data by authenticated `user_id`.
- Use `crates/server/src/secret_storage.rs` helpers at the storage boundary for
  AEAD-wrapped auth blobs. Do not insert plaintext into those protected columns.
