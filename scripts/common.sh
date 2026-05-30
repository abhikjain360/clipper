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

  if repo_root="$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null)"; then
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

clipper_wasm_shared_memory_rustflags() {
  local shared_flags
  shared_flags="-C target-feature=+atomics,+bulk-memory,+mutable-globals"
  shared_flags+=" -C link-arg=--shared-memory"
  shared_flags+=" -C link-arg=--import-memory"
  shared_flags+=" -C link-arg=--max-memory=4294967296"
  shared_flags+=" -C link-arg=--export=__heap_base"
  shared_flags+=" -C link-arg=--export=__wasm_init_tls"
  shared_flags+=" -C link-arg=--export=__tls_size"
  shared_flags+=" -C link-arg=--export=__tls_align"
  shared_flags+=" -C link-arg=--export=__tls_base"

  printf '%s\n' "$shared_flags"
}

clipper_use_wasm_shared_memory_rustflags() {
  local shared_flags
  shared_flags="$(clipper_wasm_shared_memory_rustflags)"
  export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$shared_flags"
}

clipper_use_wasm_rustc_warning_filter() {
  local wrapper_dir
  local wrapper

  wrapper_dir="$(mktemp -d "${TMPDIR:-/tmp}/clipper-rustc-wasm-filter.XXXXXX")"
  wrapper="$wrapper_dir/rustc"
  cat > "$wrapper" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [ -z "${CLIPPER_REAL_RUSTC:-}" ]; then
  echo "CLIPPER_REAL_RUSTC is required" >&2
  exit 1
fi

stderr_file="$(mktemp "${TMPDIR:-/tmp}/clipper-rustc-stderr.XXXXXX")"
stdout_file="$(mktemp "${TMPDIR:-/tmp}/clipper-rustc-stdout.XXXXXX")"
trap 'rm -f "$stderr_file" "$stdout_file"' EXIT

set +e
"$CLIPPER_REAL_RUSTC" "$@" >"$stdout_file" 2>"$stderr_file"
status=$?
set -e

perl -0pe '
  s/^\{"\$message_type":"diagnostic","message":"unstable feature specified for `-Ctarget-feature`: `atomics`".*\n//mg;
  my $without_summary = $_;
  $without_summary =~ s/^\{"\$message_type":"diagnostic","message":"\d+ warnings? emitted".*\n//mg;
  if ($without_summary !~ /"\$message_type":"diagnostic".*"level":"warning"/s) {
    $_ = $without_summary;
  }
' "$stdout_file"

perl -0pe '
  s/^\{"\$message_type":"diagnostic","message":"unstable feature specified for `-Ctarget-feature`: `atomics`".*\n//mg;
  s/warning: unstable feature specified for `-Ctarget-feature`: `atomics`\n  \|\n  = note: this feature is not stably supported; its behavior can change in the future\n\n//g;
  my $without_summary = $_;
  $without_summary =~ s/^\{"\$message_type":"diagnostic","message":"\d+ warnings? emitted".*\n//mg;
  $without_summary =~ s/warning: \d+ warnings? emitted\n\n?//g;
  if (
    $without_summary !~ /(?:^|\n)warning:/
    && $without_summary !~ /"\$message_type":"diagnostic".*"level":"warning"/s
  ) {
    $_ = $without_summary;
  }
' "$stderr_file" >&2

exit "$status"
EOF
  chmod +x "$wrapper"

  export CLIPPER_REAL_RUSTC="$(command -v rustc)"
  export PATH="$wrapper_dir:$PATH"
  trap "rm -rf '$wrapper_dir'" EXIT
}

clipper_init_repo_root
