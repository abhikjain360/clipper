#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

if [ -z "${CLIPPER_STABLE_BIN:-}" ] || [ -z "${CLIPPER_RUST_NIGHTLY_BIN:-}" ]; then
  echo "warning: expected CLIPPER_*_BIN variables are not set"
fi

if [ -n "${CLIPPER_STABLE_BIN:-}" ]; then
  clipper_use_toolchain_path "$CLIPPER_STABLE_BIN"
fi

case "$(ulimit -n)" in
  unlimited) ;;
  *[!0-9]*) ;;
  *)
    if [ "$(ulimit -n)" -lt 4096 ]; then
      ulimit -n 4096 2>/dev/null || true
    fi
    ;;
esac

if [ -n "${CLIPPER_STABLE_BIN:-}" ]; then
  export RUST_SRC_PATH="${CLIPPER_STABLE_BIN}/../lib/rustlib/src/rust/library"
else
  export RUST_SRC_PATH="${RUST_SRC_PATH:-}"
fi

if [ "${CLIPPER_DEV_SHELL_BANNER:-0}" = "1" ]; then
  echo "clipper dev shell"
  if [ -n "${CLIPPER_STABLE_BIN:-}" ]; then
    echo "  rust stable: $("$CLIPPER_STABLE_BIN/rustc" --version)"
  else
    echo "  rust stable: unavailable"
  fi
  if [ -n "${CLIPPER_RUST_NIGHTLY_BIN:-}" ]; then
    echo "  rust nightly: $("$CLIPPER_RUST_NIGHTLY_BIN/rustc" --version)"
  else
    echo "  rust nightly: unavailable"
  fi
  echo "  sea-orm-cli: $(sea-orm-cli --version)"
  echo "  node: $(node --version)"
  echo "  pnpm: $(pnpm --version)"
fi
