# Development Environment

Use the repository flake for development through direnv. Let direnv enter the
flake shell:

```sh
direnv allow
```

After that, run commands directly from the repository. The shell hooks load the
flake tools automatically; do not wrap routine commands in `nix develop`.

Nix provides the CLI, codegen, dependency, and build tools: Flutter, Dart,
rustup, rust-analyzer, Flutter Rust Bridge codegen, cargo-edit, SeaORM CLI,
CocoaPods, SQLite, CMake, Ninja, JDK, OpenSSL, libclang, pkg-config, nixfmt,
and wasm-pack.

Rust is managed through rustup inside the Nix shell because Flutter Rust Bridge
cargokit already invokes `rustup run stable cargo` for Android builds. The
required stable channel, components, and Rust std targets are declared in
`rust-toolchain.toml`, and the direnv-managed shell ensures they are installed.
The flake also installs a pinned nightly toolchain for unstable rustfmt options
and Flutter Rust Bridge's web build. Keep stable as the normal toolchain; use
the flake wrappers for the pinned-nightly cases.

The shell auto-detects Android SDKs in the common local locations and, when an
NDK is installed, exports the target C/C++ compiler, archiver, ranlib, and cargo
linker variables for:

- `aarch64-linux-android`
- `armv7-linux-androideabi`
- `i686-linux-android`
- `x86_64-linux-android`

The browser client additionally uses the stable `wasm32-unknown-unknown` Rust
target and the pinned nightly's `rust-src` component for FRB's `build-std`
WASM build.

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
```

Regenerate SeaORM entities after server schema changes. Server migrations are
the schema owner; generated entity files should not be hand-edited as the final
change.

```sh
tmpdir=$(mktemp -d)
cargo run -q -p clipper-server -- init -d "$tmpdir/data"
sea-orm-cli generate entity -u "sqlite:$tmpdir/data/clipper.db" -o crates/server/src/entity --with-prelude none
rm -rf "$tmpdir"
```

Direnv can print a long environment diff for Nix shells. To hide only that diff,
put this in `~/.config/direnv/direnv.toml`:

```toml
[global]
hide_env_diff = true
```
