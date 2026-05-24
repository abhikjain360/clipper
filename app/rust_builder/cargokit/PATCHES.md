# Local cargokit patches

Cargokit is vendored as a flat copy in this directory. When upstream is
re-vendored, the local changes below need to be reapplied.

## `CARGOKIT_CARGO` / `CARGOKIT_RUSTC` env override

Goal: let the Nix flake's fenix-built toolchain serve cargokit's Android
plugin build, without `rustup` needing to exist on `$PATH`.

When `CARGOKIT_CARGO` is set, cargokit must:

1. Skip every `rustup toolchain install` / `rustup target add` /
   `rustup component add` call — fenix already provisions the toolchain.
2. Use `$CARGOKIT_CARGO` (and `$CARGOKIT_RUSTC` for `RUSTC`) directly
   instead of `rustup run <toolchain> cargo ...` and
   `rustup which --toolchain <toolchain> rustc`.
3. Avoid constructing `Rustup()` in a way that immediately shells out to
   `rustup` — the lookup must be lazy so the constructor stays cheap.

When `CARGOKIT_CARGO` is unset, behaviour is the upstream rustup-driven
flow.

## Files touched

- `build_tool/lib/src/builder.dart`
  - Added `import 'dart:io';` for `Platform.environment`.
  - `RustBuilder.prepare`: early-return when
    `Platform.environment['CARGOKIT_CARGO'] != null`.
  - `RustBuilder.build`: branch on `Platform.environment['CARGOKIT_CARGO']`.
    When set, invoke that cargo path directly with the same arg list and
    set `RUSTC` from `CARGOKIT_RUSTC`. When unset, the original `rustup
    which` + `rustup run` pair runs.
- `build_tool/lib/src/rustup.dart`
  - `Rustup()` constructor no longer eagerly populates
    `_installedToolchains`. The field became a lazy getter
    (`_installedToolchainsCache ??= _getInstalledToolchains()`), so
    constructing a `Rustup` instance does not shell out.

Search the touched files for the string `Local patch` to find the exact
hunks.
