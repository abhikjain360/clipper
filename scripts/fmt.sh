#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

clipper_enter_repo
clipper_use_nightly

nixfmt flake.nix
cargo fmt --all
dart format app/lib app/test app/integration_test app/test_driver
