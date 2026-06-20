#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

export CLIPPER_SERVER_SECRET_FILE="$PWD/data/clipper-server.secret"

KEY=$(openssl rand -base64 32)

cargo run -p clipper-server -- add-access-key \
  --data-dir data/clipper-server \
  --access-key "$KEY"

echo "$KEY"
