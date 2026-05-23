# Development Environment

Use the repository flake for development:

```sh
nix develop .
```

Nix provides the shell tools: Flutter, Dart, rustup, rust-analyzer, SeaORM CLI,
SQLite, CMake, Ninja, JDK 17, OpenSSL, libclang, and pkg-config.

Rust is managed through rustup inside the Nix shell because Flutter Rust Bridge
cargokit already invokes `rustup run stable cargo` for Android builds. The
required channel, components, and Android Rust std targets are declared in
`rust-toolchain.toml`, and `nix develop` ensures they are installed.

The shell auto-detects Android SDKs in the common local locations and, when an
NDK is installed, exports the target C/C++ compiler, archiver, ranlib, and cargo
linker variables for:

- `aarch64-linux-android`
- `armv7-linux-androideabi`
- `i686-linux-android`
- `x86_64-linux-android`

Android SDK/NDK installation, physical devices, emulators, and Xcode remain
host setup. Once those are present, the Nix shell should provide the remaining
developer tooling.

Useful checks:

```sh
nix develop . -c cargo test
nix develop . -c cargo check -p rust_lib_clipper_app --target aarch64-linux-android
```
