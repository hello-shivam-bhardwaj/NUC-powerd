#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SERVICE_SRC="$REPO_DIR/packaging/nuc-powerd.service"
CONFIG_SRC="$REPO_DIR/config/nuc-powerd.example.toml"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found; install Rust toolchain first" >&2
  exit 1
fi

cargo build --release
sudo install -D -m 0755 "$REPO_DIR/target/release/nuc-powerd" /usr/local/bin/nuc-powerd
sudo install -D -m 0644 "$CONFIG_SRC" /etc/nuc-powerd.toml
sudo install -D -m 0644 "$SERVICE_SRC" /etc/systemd/system/nuc-powerd.service
sudo install -d -m 0755 /run/nuc-powerd
sudo systemctl daemon-reload
sudo systemctl enable --now nuc-powerd.service

echo "Installed and started nuc-powerd.service"
