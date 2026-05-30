# Contributing

## Pull Requests

I am not accepting pull requests right now.

Clipper is experimental in two related senses. The product is early: the final
feature set is not decided, the design is still moving, and the code has not
been security audited. The development process is also part of the experiment:
I am using this project to see how far I can write high-quality software while
relying heavily on agentic coding, without handing off the design judgment,
review responsibility, or maintenance ownership that quality still requires.

That makes outside pull requests a poor fit for now. Reviewing and coordinating
PRs would change the experiment as much as it would change the project, and it
would add process before either the product or the contribution model has
settled.

You are still welcome to read the code, run it locally, fork it, experiment, and
send feedback. This guide documents the development setup and checks so the
project is easy to work with even while upstream pull requests are closed.

## Project Status

Clipper should not be trusted with secrets you cannot afford to lose. See
[README.md](README.md), [SECURITY.md](SECURITY.md), and
[docs/rust-code-review.md](docs/rust-code-review.md) before treating the code as
anything other than experimental software.

## Development Environment

Use the checked-in Nix flake through direnv:

```sh
direnv allow
```

After direnv has loaded the shell, run commands directly from the repository. Do
not wrap routine commands in `nix develop`.

The flake provides the Rust, Flutter, Dart, Java, Android helper, C/C++,
Flutter Rust Bridge, SeaORM, CocoaPods, OSV Scanner, wasm, and formatting tools
used by this repo. Android SDK/NDK installs, emulators, Xcode, signing, and
other OS-tied platform setup still come from the host system.

If Android Gradle picks up the wrong Flutter SDK, refresh the generated local
properties file:

```sh
cd app && flutter pub get
```

More detail is in [docs/development-environment.md](docs/development-environment.md).

## Repository Layout

- `crates/server` contains the Axum API and SQLite storage.
- `crates/client` contains the shared Rust sync client.
- `crates/daemon` contains the macOS/Linux background process.
- `crates/api-types` contains shared HTTP and WebSocket payloads.
- `crates/daemon-types` contains daemon IPC types.
- `crates/app-types` contains display-ready app state.
- `app` contains the Flutter app and Flutter Rust Bridge adapter.
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
cd app && flutter analyze && flutter test
nix run .#wasm-check
```

Do not run `cargo fmt` directly. This repository's `rustfmt.toml` uses
nightly-only rustfmt options, so formatting must go through the flake wrapper:

```sh
nix run .#fmt
```

For Rust-only formatting, use:

```sh
nix run .#rustfmt
```

## Flutter Rust Bridge

Regenerate Flutter Rust Bridge output after Rust bridge API changes:

```sh
nix run .#frb-generate
```

Keep Rust and Dart `flutter_rust_bridge` versions aligned. The bridge adapter
types in `app/rust/src/api/clipper.rs` should stay adapter-only, with exhaustive
conversions so state schema changes fail at compile time.

For web artifacts, use the project wrappers:

```sh
nix run .#frb-build-web
nix run .#web-build
nix run .#web-serve
```

The web client requires shared-memory wasm and cross-origin isolation headers.
Use `nix run .#web-serve` rather than a generic static file server.

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

The app defaults to `http://127.0.0.1:8787`. Android emulators should use
`http://10.0.2.2:8787`.

## Schema And Generated Code

Server schema changes live in `crates/server/src/migration/*.rs`. Regenerate
SeaORM entities after changing the schema:

```sh
nix run .#server-entities
```

Do not hand-edit generated entity files as the final change.

When auth state or auth commands cross Flutter Rust Bridge, update the bridge
adapter and regenerate bridge output so Dart, daemon IPC, and Rust stay aligned.

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

## Working Locally

Because upstream pull requests are closed for now, the most useful workflow is:

1. Fork the repository or work on a local branch.
2. Make the smallest coherent change.
3. Run the relevant checks from this guide.
4. Keep notes about design tradeoffs, test coverage, and any skipped checks.

If upstream contribution policy changes later, this file will be updated with
the current review and submission process.
