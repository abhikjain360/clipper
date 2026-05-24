#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"
source "$SCRIPT_DIR/.env.android"

if [ -z "${CLIPPER_STABLE_BIN:-}" ] || [ -z "${CLIPPER_RUST_NIGHTLY_BIN:-}" ]; then
  echo "warning: expected CLIPPER_*_BIN variables are not set"
fi

if [ -n "${CLIPPER_STABLE_BIN:-}" ]; then
  clipper_use_toolchain_path "$CLIPPER_STABLE_BIN"
fi

if [ -n "${CLIPPER_STABLE_BIN:-}" ]; then
  export RUST_SRC_PATH="${CLIPPER_STABLE_BIN}/../lib/rustlib/src/rust/library"
else
  export RUST_SRC_PATH="${RUST_SRC_PATH:-}"
fi

clipper_setup_android_env

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
echo "  flutter: $(flutter --version | sed -n '1p')"
echo "  sea-orm-cli: $(sea-orm-cli --version)"
if [ -n "${ANDROID_HOME:-}" ]; then
  echo "  android sdk: $ANDROID_HOME"
fi
if [ -n "${ANDROID_NDK_HOME:-}" ]; then
  echo "  android ndk: $ANDROID_NDK_HOME"
else
  echo "  android ndk: not configured"
fi
echo "host Xcode, Android SDK/NDK installs, and emulators remain platform setup"
