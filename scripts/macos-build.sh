#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

case "$(uname -s)" in
  Darwin) ;;
  *)
    echo "macOS app builds require Darwin/Xcode; run this on macOS." >&2
    exit 1
    ;;
esac

clipper_require_env CLIPPER_STABLE_BIN
clipper_enter_app

export CARGOKIT_CARGO="${CARGOKIT_CARGO:-$CLIPPER_STABLE_BIN/cargo}"
export CARGOKIT_RUSTC="${CARGOKIT_RUSTC:-$CLIPPER_STABLE_BIN/rustc}"

if [ ! -x "$CARGOKIT_CARGO" ]; then
  echo "CARGOKIT_CARGO is not executable: $CARGOKIT_CARGO" >&2
  exit 1
fi

if [ ! -x "$CARGOKIT_RUSTC" ]; then
  echo "CARGOKIT_RUSTC is not executable: $CARGOKIT_RUSTC" >&2
  exit 1
fi

host_flutter="${CLIPPER_HOST_FLUTTER:-}"
if [ -z "$host_flutter" ]; then
  for candidate in /opt/homebrew/bin/flutter /usr/local/bin/flutter; do
    if [ -x "$candidate" ]; then
      host_flutter="$candidate"
      break
    fi
  done
fi

if [ -z "$host_flutter" ] || [ ! -x "$host_flutter" ]; then
  echo "Host Flutter was not found. Install Flutter outside Nix or set CLIPPER_HOST_FLUTTER." >&2
  exit 1
fi

case "$host_flutter" in
  /nix/store/*)
    echo "macOS packaging needs a writable host Flutter SDK, not Nix Flutter: $host_flutter" >&2
    exit 1
    ;;
esac

host_flutter_root="${CLIPPER_HOST_FLUTTER_ROOT:-}"
if [ -z "$host_flutter_root" ]; then
  case "$host_flutter" in
    /opt/homebrew/bin/flutter)
      host_flutter_root="/opt/homebrew/share/flutter"
      ;;
    /usr/local/bin/flutter)
      host_flutter_root="/usr/local/share/flutter"
      ;;
    */bin/flutter)
      host_flutter_root="$(cd -- "$(dirname -- "$host_flutter")/.." && pwd -P)"
      ;;
  esac
fi

flutter_root_env=()
if [ -n "$host_flutter_root" ]; then
  if [ ! -d "$host_flutter_root/packages/flutter_tools" ]; then
    echo "Host Flutter root does not look valid: $host_flutter_root" >&2
    echo "Set CLIPPER_HOST_FLUTTER_ROOT if CLIPPER_HOST_FLUTTER is a wrapper or symlink." >&2
    exit 1
  fi
  flutter_root_env=(FLUTTER_ROOT="$host_flutter_root")
else
  flutter_root_env=(-u FLUTTER_ROOT)
fi

if [ "$#" -eq 0 ]; then
  set -- --debug
fi

host_flutter_bin="$(cd -- "$(dirname -- "$host_flutter")" && pwd -P)"
host_path="/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin"
clean_path="$host_path:$host_flutter_bin:$CLIPPER_STABLE_BIN:$PATH"

echo "Using host Flutter: $host_flutter"
if [ -n "$host_flutter_root" ]; then
  echo "Using host Flutter root: $host_flutter_root"
fi
echo "Using Flutter Swift Package Manager: ${CLIPPER_FLUTTER_SWIFT_PACKAGE_MANAGER:-false}"
echo "Using Nix cargo for Rust: $CARGOKIT_CARGO"
echo "Using Nix rustc for Rust: $CARGOKIT_RUSTC"

env \
  -u SDKROOT \
  -u SDKROOT_FOR_BUILD \
  -u DEVELOPER_DIR \
  -u DEVELOPER_DIR_FOR_BUILD \
  -u NIX_CFLAGS_COMPILE \
  -u NIX_LDFLAGS \
  -u NIX_CC \
  -u NIX_CC_WRAPPER_TARGET_HOST_arm64_apple_darwin \
  -u NIX_BINTOOLS \
  -u NIX_BINTOOLS_WRAPPER_TARGET_HOST_arm64_apple_darwin \
  -u NIX_PKG_CONFIG_WRAPPER_TARGET_HOST_arm64_apple_darwin \
  -u CC \
  -u CXX \
  -u LD \
  PATH="$clean_path" \
  CARGOKIT_CARGO="$CARGOKIT_CARGO" \
  CARGOKIT_RUSTC="$CARGOKIT_RUSTC" \
  CLIPPER_STABLE_BIN="$CLIPPER_STABLE_BIN" \
  FLUTTER_SWIFT_PACKAGE_MANAGER="${CLIPPER_FLUTTER_SWIFT_PACKAGE_MANAGER:-false}" \
  "${flutter_root_env[@]}" \
  "$host_flutter" build macos "$@"
