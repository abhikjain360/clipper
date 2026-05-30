#!/usr/bin/env bash
set -euo pipefail

clipper_repo_root() {
  local script_dir
  local repo_root

  script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
  if [ -n "${CLIPPER_REPO_ROOT:-}" ] && [ -d "$CLIPPER_REPO_ROOT" ]; then
    printf '%s\n' "$CLIPPER_REPO_ROOT"
    return 0
  fi

  if repo_root="$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null)"; then
    printf '%s\n' "$repo_root"
    return 0
  fi

  if repo_root="$(git -C "$script_dir/.." rev-parse --show-toplevel 2>/dev/null)"; then
    printf '%s\n' "$repo_root"
    return 0
  fi

  if repo_root="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null)"; then
    printf '%s\n' "$repo_root"
    return 0
  fi

  echo "Unable to detect Clipper repo root" >&2
  return 1
}

clipper_init_repo_root() {
  if [ -z "${CLIPPER_REPO_ROOT:-}" ]; then
    export CLIPPER_REPO_ROOT="$(clipper_repo_root)"
  fi
}

clipper_enter_repo() {
  clipper_init_repo_root
  cd "$CLIPPER_REPO_ROOT"
}

clipper_enter_app() {
  clipper_enter_repo
  cd app
}

clipper_require_env() {
  local missing=0
  local name

  for name in "$@"; do
    if [ -z "${!name:-}" ]; then
      echo "Required environment variable missing: $name" >&2
      missing=1
    fi
  done

  return "$missing"
}

clipper_use_toolchain_path() {
  local toolchain_bin="$1"
  if [ -z "$toolchain_bin" ]; then
    echo "toolchain path is required" >&2
    return 1
  fi

  export PATH="${toolchain_bin}:$PATH"
}

clipper_use_stable() {
  clipper_require_env CLIPPER_STABLE_BIN
  clipper_use_toolchain_path "$CLIPPER_STABLE_BIN"
}

clipper_use_nightly() {
  clipper_require_env CLIPPER_RUST_NIGHTLY_BIN
  clipper_use_toolchain_path "$CLIPPER_RUST_NIGHTLY_BIN"
}

clipper_init_repo_root
