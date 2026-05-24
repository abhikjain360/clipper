#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

clipper_require_env FLUTTER_ROOT

clipper_enter_app

export FLUTTER_ROOT
flutter_rust_bridge_codegen generate "$@"
