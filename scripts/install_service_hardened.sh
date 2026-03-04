#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SERVICE_SRC="$REPO_DIR/packaging/nuc-powerd.service"
CONFIG_SRC="$REPO_DIR/config/nuc-powerd.example.toml"
SKIP_BUILD=0
NO_ENABLE=0

usage() {
  cat <<'EOF'
Usage: ./scripts/install_service_hardened.sh [options]

Options:
  --config-src PATH   Config template to install (default: config/nuc-powerd.example.toml)
  --skip-build        Do not run cargo build (install existing binary only)
  --no-enable         Install files but do not enable/start service
  -h, --help          Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --config-src)
      [[ $# -ge 2 ]] || { usage; exit 1; }
      CONFIG_SRC="$2"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    --no-enable)
      NO_ENABLE=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ ! -f "$SERVICE_SRC" ]]; then
  echo "service file not found: $SERVICE_SRC" >&2
  exit 1
fi
if [[ ! -f "$CONFIG_SRC" ]]; then
  echo "config source not found: $CONFIG_SRC" >&2
  exit 1
fi

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found; install Rust toolchain first" >&2
    exit 1
  fi
  echo "[install] building nuc-powerd release binary"
  cargo build --release --bin nuc-powerd
fi

BIN_SRC="$REPO_DIR/target/release/nuc-powerd"
if [[ ! -x "$BIN_SRC" ]]; then
  echo "binary not found: $BIN_SRC" >&2
  echo "run without --skip-build or build manually first" >&2
  exit 1
fi

echo "[install] installing binary + unit + config"
sudo install -D -m 0755 "$BIN_SRC" /usr/local/bin/nuc-powerd
sudo install -D -m 0644 "$SERVICE_SRC" /etc/systemd/system/nuc-powerd.service
sudo install -D -m 0644 "$CONFIG_SRC" /etc/nuc-powerd.toml
sudo install -d -m 0755 /run/nuc-powerd
sudo systemctl daemon-reload

if [[ "$NO_ENABLE" -eq 1 ]]; then
  echo "[install] files installed; service not started (--no-enable)"
  echo "next: sudo systemctl enable --now nuc-powerd.service"
  exit 0
fi

echo "[install] enabling and starting nuc-powerd.service"
sudo systemctl reset-failed nuc-powerd.service || true
sudo systemctl enable --now nuc-powerd.service
sleep 2

if ! sudo systemctl is-active --quiet nuc-powerd.service; then
  echo "[install] service failed to start; diagnostics:"
  sudo systemctl status nuc-powerd.service --no-pager -l || true
  sudo journalctl -u nuc-powerd.service -n 120 --no-pager || true
  exit 1
fi

echo "[install] service is active"
echo "run ./scripts/validate_system.sh for functional validation"
