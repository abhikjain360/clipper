#!/usr/bin/env bash
set -euo pipefail

launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.clipper.daemon.plist" 2>/dev/null || true
pkill -x clipper-daemon 2>/dev/null || true
pkill -f "clipper-server.*serve|target/debug/clipper-server" 2>/dev/null || true

if command -v lsof >/dev/null 2>&1; then
  while IFS= read -r pid; do
    kill "$pid" 2>/dev/null || true
  done < <(lsof -nP -iTCP:53880 -sTCP:LISTEN -t 2>/dev/null || true)
fi

rm -f \
  "$HOME/Library/LaunchAgents/com.clipper.daemon.plist" \
  "/tmp/clipper-daemon.stdout.log" \
  "/tmp/clipper-daemon.stderr.log"

rm -rf \
  "$HOME/Library/Logs/Clipper" \
  "$HOME/Library/Application Support/Clipper" \
  "$HOME/Library/Containers/com.clipper.clipperApp" \
  "data/clipper-server" \
  "data/clipper-server.secret" \
  ".clipper-server" \
  ".clipper-server.secret" \
  "web/.tamagui" \
  "web/dist" \
  "web/src/generated"

security delete-generic-password -s com.clipper.daemon -a credentials >/dev/null 2>&1 || true
security delete-generic-password -s com.clipper.daemon -a ipc-secret-v1 >/dev/null 2>&1 || true
