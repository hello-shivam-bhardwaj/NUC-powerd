#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SERVICE_SRC="$REPO_DIR/packaging/nuc-powerd.service"
CONFIG_SRC="$REPO_DIR/config/nuc-powerd.example.toml"
WITH_UI=0

if [[ "${1:-}" == "--with-ui" ]]; then
  WITH_UI=1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found; install Rust toolchain first" >&2
  exit 1
fi

cargo build --release --bin nuc-powerd
sudo install -D -m 0755 "$REPO_DIR/target/release/nuc-powerd" /usr/local/bin/nuc-powerd

if [[ "$WITH_UI" -eq 1 ]]; then
  cargo build --release --features ui --bin nuc-powerd-ui
  sudo install -D -m 0755 "$REPO_DIR/target/release/nuc-powerd-ui" /usr/local/bin/nuc-powerd-ui
fi

sudo install -D -m 0644 "$CONFIG_SRC" /etc/nuc-powerd.toml
sudo install -D -m 0644 "$SERVICE_SRC" /etc/systemd/system/nuc-powerd.service
sudo install -d -m 0755 /run/nuc-powerd
sudo systemctl daemon-reload
sudo systemctl enable --now nuc-powerd.service

if [[ "$WITH_UI" -eq 1 ]]; then
  echo "Installed nuc-powerd (+ optional nuc-powerd-ui) and started nuc-powerd.service"
else
  echo "Installed nuc-powerd and started nuc-powerd.service"
fi
