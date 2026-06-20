#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

mkdir -p data

if [[ ! -f data/clipper-server.secret ]]; then
  cargo run -p clipper-server -- generate-secret > data/clipper-server.secret
  chmod 600 data/clipper-server.secret
fi

export CLIPPER_SERVER_SECRET_FILE="$PWD/data/clipper-server.secret"

cargo run -p clipper-server -- init --data-dir data/clipper-server
cargo run -p clipper-server -- serve --data-dir data/clipper-server
