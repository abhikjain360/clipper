#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

clipper_require_env CLIPPER_WASM_TARGET

clipper_enter_repo
clipper_use_nightly

cargo check -p rust_lib_clipper_app --target "$CLIPPER_WASM_TARGET" "$@"
