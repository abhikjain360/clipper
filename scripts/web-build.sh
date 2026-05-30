#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

clipper_require_env CLIPPER_WASM_TARGET FLUTTER_ROOT

clipper_enter_app
clipper_use_nightly
clipper_use_wasm_rustc_warning_filter
export FLUTTER_ROOT

flutter pub get
wasm_rustc_config="build.rustc=\"$(command -v rustc)\""
flutter_rust_bridge_codegen build-web \
  --cargo-build-args --config \
  --cargo-build-args "$wasm_rustc_config" \
  --wasm-pack-rustflags "$(clipper_wasm_shared_memory_rustflags)"
flutter build web --no-pub --no-wasm-dry-run "$@"
