#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

clipper_enter_repo

root="${CLIPPER_WEB_ROOT:-app/build/web}"
preferred_port="${1:-53880}"

if [ ! -f "$root/index.html" ]; then
  echo "missing $root/index.html; run nix run .#web-build first" >&2
  exit 1
fi

python3 "$SCRIPT_DIR/web-serve.py" "$root" "$preferred_port"
