#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

clipper_enter_repo
clipper_use_stable

tmpdir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmpdir"
}
trap cleanup EXIT

RUST_LOG="${RUST_LOG:-warn}" cargo run -q -p clipper-server -- init -d "$tmpdir/data"
sea-orm-cli generate entity \
  -u "sqlite:$tmpdir/data/clipper.db" \
  -o crates/server/src/entity \
  --with-prelude none
