#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

clipper_require_env CLIPPER_WASM_TARGET FLUTTER_ROOT

clipper_enter_app
clipper_use_nightly
export FLUTTER_ROOT

flutter pub get
flutter_rust_bridge_codegen build-web
flutter build web --no-pub --no-wasm-dry-run "$@"
