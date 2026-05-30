# Development Environment

Use the repository flake for development through direnv. Let direnv enter the
flake shell:

```sh
direnv allow
```

After that, run commands directly from the repository. The shell hooks load the
flake tools automatically; do not wrap routine commands in `nix develop`.

Nix provides the CLI, codegen, dependency, and build tools: Flutter, Dart,
Rust (via [fenix](https://github.com/nix-community/fenix)), rust-analyzer,
Flutter Rust Bridge codegen, cargo-edit, SeaORM CLI, CocoaPods, SQLite, CMake,
Ninja, JDK, OpenSSL, libclang, pkg-config, nixfmt, and wasm-pack.

Rust toolchains come from fenix as proper Nix derivations — no `~/.rustup`,
no first-run downloads. The stable channel is the default `cargo`/`rustc` on
`$PATH` and bundles `rustfmt`, `clippy`, `rust-src`, `rust-analyzer`, and the
Android + `wasm32-unknown-unknown` `rust-std` targets. A pinned nightly is
exposed at `$CLIPPER_RUST_NIGHTLY_BIN` for unstable rustfmt options and
Flutter Rust Bridge's web build; the flake wrappers (`nix run .#fmt`,
`.#rustfmt`, `.#frb-build-web`, `.#web-build`) use it automatically. Both
channels are pinned by date and manifest hash in `flake.nix`
(`rustStableDate`, `rustNightlyDate`); to bump either one, set the new date
and run `nix-prefetch-url --type sha256
https://static.rust-lang.org/dist/<date>/channel-rust-<channel>.toml` to get
the manifest hash.

Flutter Rust Bridge's cargokit (vendored at `app/rust_builder/cargokit`)
normally shells out to `rustup` for Android plugin builds. It is patched to
prefer `CARGOKIT_CARGO` / `CARGOKIT_RUSTC` when those env vars are set; the
devShell points them at the fenix stable toolchain. The `rust-toolchain.toml`
at the repo root is informational only for tools outside Nix that still
honour it.

The shell auto-detects Android SDKs in the common local locations and, when an
NDK is installed, exports the target C/C++ compiler, archiver, ranlib, and cargo
linker variables for:

- `aarch64-linux-android`
- `armv7-linux-androideabi`
- `i686-linux-android`
- `x86_64-linux-android`

The browser client additionally uses the stable `wasm32-unknown-unknown` Rust
target and the pinned nightly's `rust-src` component for FRB's `build-std`
WASM build. Use `nix run .#frb-build-web` or `nix run .#web-build` for web
artifacts; the wrappers pass the shared-memory wasm linker flags that FRB's
worker pool needs.

Android SDK/NDK installation, physical devices, emulators, Xcode, signing, and
other OS-tied platform setup remain host setup. The flake discovers those where
practical, but it does not try to make mobile emulators or Apple tooling pure
Nix. Once the host platform pieces are present, use the flake for the commands
that build, test, update dependencies, and generate code.

If Android Gradle picks up the wrong Flutter SDK, refresh
`app/android/local.properties` from the flake:

```sh
cd app && flutter pub get
```

Useful checks:

```sh
nix run .#fmt
cargo test --workspace
cd app/rust && cargo check
cd app && flutter analyze && flutter test
nix run .#wasm-check
cd app/android && ./gradlew :app:assembleDebug
```

Regenerate Flutter Rust Bridge files after changing Rust bridge APIs:

```sh
nix run .#frb-generate
```

Build the web bridge package and Flutter web client:

```sh
nix run .#frb-build-web
nix run .#web-build
nix run .#web-serve
```

The web client must be served with `Cross-Origin-Opener-Policy: same-origin`
and `Cross-Origin-Embedder-Policy: require-corp`. Flutter Rust Bridge uses a
Rust wasm worker with shared memory, and browsers only allow that in a
cross-origin isolated page. The `web-serve` wrapper serves `app/build/web` with
the required headers for local development; generic static file servers do not.

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
