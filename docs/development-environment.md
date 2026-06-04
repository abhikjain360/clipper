# Development Environment

Use the repository flake for development through direnv. Let direnv enter the
flake shell:

```sh
direnv allow
```

After that, run commands directly from the repository. The shell hooks load the
flake tools automatically; do not wrap routine commands in `nix develop`.

Nix provides the CLI, dependency, and build tools: Rust via
[fenix](https://github.com/nix-community/fenix), rust-analyzer, Node 24, pnpm,
wasm-pack, Tauri desktop build dependencies, React Native/UniFFI codegen
helpers, cargo-edit, cargo-udeps, SeaORM CLI, SQLite, CMake, Ninja, OpenSSL,
libclang, pkg-config, nixfmt, and osv-scanner.

Rust toolchains come from fenix as proper Nix derivations - no `~/.rustup`, no
first-run downloads. The stable channel is the default `cargo`/`rustc` on
`$PATH` and bundles `rustfmt`, `clippy`, `rust-src`, `rust-analyzer`, the
`wasm32-unknown-unknown` `rust-std` target, and Android `rust-std` targets used
by the UniFFI mobile bridge. A pinned nightly is exposed at
`$CLIPPER_RUST_NIGHTLY_BIN` for unstable rustfmt options; the flake wrappers
(`nix run .#fmt`, `.#rustfmt`, `.#web-build`, `.#tauri-build`, mobile wrappers)
use the right stable or nightly toolchain automatically. Both channels are
pinned by date and manifest hash in `flake.nix` (`rustStableDate`,
`rustNightlyDate`); to bump either one, set the new date and run
`nix-prefetch-url --type sha256
https://static.rust-lang.org/dist/<date>/channel-rust-<channel>.toml` to get
the manifest hash.

Useful checks:

```sh
nix run .#fmt
nix run .#audit
nix run .#udeps
cargo test --workspace
nix run .#wasm-check
nix run .#web-check
nix run .#mobile-check
cargo check -p clipper-desktop
nix run .#tauri-build -- --no-bundle
```

Build or serve the browser and Tauri clients:

```sh
nix run .#web-build
nix run .#web-serve
nix run .#tauri-dev
nix run .#tauri-build
nix run .#mobile-start
nix run .#mobile-uniffi-android
nix run .#mobile-android
```

The browser client talks to the shared Rust sync client through
`crates/web-wasm`. The Tauri desktop shell reuses the same React UI and talks to
`clipper-client` through Tauri commands in `web/src-tauri`.

The mobile client is React Native/Expo. It shares TypeScript contracts through
`packages/shared`, uses Tamagui in `mobile/src`, and talks to `clipper-client`
through UniFFI records exposed by `crates/mobile-uniffi` and packaged by
`packages/mobile-bridge`. The flake provides Node, pnpm, Rust targets, and
codegen tooling; Android SDK/NDK installs and emulators may come from the host.

Regenerate SeaORM entities after server schema changes. Server migrations are
the schema owner; generated entity files should not be hand-edited as the final
change.

```sh
nix run .#server-entities
```

Direnv can print a long environment diff for Nix shells. To hide only that diff,
put this in `~/.config/direnv/direnv.toml`:

```toml
[global]
hide_env_diff = true
```
