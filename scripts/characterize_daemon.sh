#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_PATH="$REPO_DIR/config/nuc-powerd.example.toml"
SAMPLE_SEC=2
WITH_UI=0

usage() {
  cat <<'USAGE'
Usage:
  ./scripts/characterize_daemon.sh [--config <path>] [--sample-sec <n>] [--with-ui]

Options:
  --config <path>    TOML config to use (default: config/nuc-powerd.example.toml)
  --sample-sec <n>   Seconds to wait before sampling process stats (default: 2)
  --with-ui          Also characterize optional nuc-powerd-ui sidecar
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --config)
      [[ $# -ge 2 ]] || {
        usage
        exit 1
      }
      CONFIG_PATH="$2"
      shift 2
      ;;
    --sample-sec)
      [[ $# -ge 2 ]] || {
        usage
        exit 1
      }
      SAMPLE_SEC="$2"
      shift 2
      ;;
    --with-ui)
      WITH_UI=1
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

if [[ ! -f "$CONFIG_PATH" ]]; then
  echo "config not found: $CONFIG_PATH" >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found" >&2
  exit 1
fi

if ! command -v strip >/dev/null 2>&1; then
  echo "strip not found (install binutils)" >&2
  exit 1
fi

if ! [[ "$SAMPLE_SEC" =~ ^[0-9]+$ ]] || [[ "$SAMPLE_SEC" -lt 1 ]]; then
  echo "--sample-sec must be a positive integer" >&2
  exit 1
fi

TMP_CFG="$(mktemp /tmp/nuc-powerd.char.XXXXXX.toml)"
DAEMON_LOG="$(mktemp /tmp/nuc-powerd.char.daemon.XXXXXX.log)"
UI_LOG="$(mktemp /tmp/nuc-powerd.char.ui.XXXXXX.log)"
DEFAULT_TREE="$(mktemp /tmp/nuc-powerd.char.default.tree.XXXXXX.txt)"
UI_TREE="$(mktemp /tmp/nuc-powerd.char.ui.tree.XXXXXX.txt)"
DAEMON_STRIP="$(mktemp /tmp/nuc-powerd.char.daemon.strip.XXXXXX)"
UI_STRIP="$(mktemp /tmp/nuc-powerd.char.ui.strip.XXXXXX)"
STATUS_TMP="$(mktemp /tmp/nuc-powerd.status.XXXXXX.json)"
CONTROL_TMP="$(mktemp /tmp/nuc-powerd.control.XXXXXX.json)"
DAEMON_PID=""
UI_PID=""

cleanup() {
  if [[ -n "$UI_PID" ]] && kill -0 "$UI_PID" >/dev/null 2>&1; then
    kill "$UI_PID" >/dev/null 2>&1 || true
    wait "$UI_PID" 2>/dev/null || true
  fi
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" >/dev/null 2>&1; then
    kill "$DAEMON_PID" >/dev/null 2>&1 || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  rm -f "$TMP_CFG" "$DAEMON_LOG" "$UI_LOG" "$DEFAULT_TREE" "$UI_TREE" \
    "$DAEMON_STRIP" "$UI_STRIP" "$STATUS_TMP" "$CONTROL_TMP"
}
trap cleanup EXIT

cp "$CONFIG_PATH" "$TMP_CFG"
rm -f "$STATUS_TMP" "$CONTROL_TMP"
PORT=$((8800 + RANDOM % 1000))
sed -i "s|^status_path = \".*\"|status_path = \"${STATUS_TMP}\"|" "$TMP_CFG"
sed -i "s|^control_path = \".*\"|control_path = \"${CONTROL_TMP}\"|" "$TMP_CFG"
sed -i "s|^api_bind = \".*\"|api_bind = \"127.0.0.1:${PORT}\"|" "$TMP_CFG"

if ! rg -q '^status_path = ' "$TMP_CFG" || ! rg -q '^control_path = ' "$TMP_CFG" || ! rg -q '^api_bind = ' "$TMP_CFG"; then
  echo "config must contain daemon status_path/control_path/api_bind keys" >&2
  exit 1
fi

echo "building daemon..."
cargo build --release --bin nuc-powerd >/dev/null

DAEMON_BIN="$REPO_DIR/target/release/nuc-powerd"
DAEMON_SIZE_RAW="$(stat -c%s "$DAEMON_BIN")"
cp "$DAEMON_BIN" "$DAEMON_STRIP"
strip -s "$DAEMON_STRIP"
DAEMON_SIZE_STRIPPED="$(stat -c%s "$DAEMON_STRIP")"

echo "sampling daemon runtime..."
"$DAEMON_BIN" --config "$TMP_CFG" dry-run >"$DAEMON_LOG" 2>&1 &
DAEMON_PID="$!"
sleep "$SAMPLE_SEC"
if ! kill -0 "$DAEMON_PID" >/dev/null 2>&1; then
  echo "daemon failed to start; log follows:" >&2
  cat "$DAEMON_LOG" >&2
  exit 1
fi

read -r DAEMON_RSS_KB DAEMON_CPU_PCT DAEMON_THREADS DAEMON_VSZ_KB < <(
  ps -o rss=,pcpu=,nlwp=,vsz= -p "$DAEMON_PID" | awk 'NR==1 {print $1, $2, $3, $4}'
)

cargo tree >"$DEFAULT_TREE"
DEFAULT_TREE_LINES="$(wc -l <"$DEFAULT_TREE")"
if rg -q 'tiny_http' "$DEFAULT_TREE"; then
  DEFAULT_HAS_TINY_HTTP="yes"
else
  DEFAULT_HAS_TINY_HTTP="no"
fi

echo
echo "daemon_profile:"
echo "  binary_unstripped_bytes: $DAEMON_SIZE_RAW"
echo "  binary_stripped_bytes:   $DAEMON_SIZE_STRIPPED"
echo "  rss_kb_idle:             $DAEMON_RSS_KB"
echo "  cpu_pct_idle:            $DAEMON_CPU_PCT"
echo "  threads_idle:            $DAEMON_THREADS"
echo "  virtual_mem_kb_idle:     $DAEMON_VSZ_KB"
echo "  cargo_tree_lines:        $DEFAULT_TREE_LINES"
echo "  includes_tiny_http:      $DEFAULT_HAS_TINY_HTTP"

if [[ "$WITH_UI" -eq 1 ]]; then
  echo
  echo "building optional ui sidecar..."
  cargo build --release --features ui --bin nuc-powerd-ui >/dev/null

  UI_BIN="$REPO_DIR/target/release/nuc-powerd-ui"
  UI_SIZE_RAW="$(stat -c%s "$UI_BIN")"
  cp "$UI_BIN" "$UI_STRIP"
  strip -s "$UI_STRIP"
  UI_SIZE_STRIPPED="$(stat -c%s "$UI_STRIP")"

  echo "sampling ui runtime..."
  "$UI_BIN" --config "$TMP_CFG" >"$UI_LOG" 2>&1 &
  UI_PID="$!"
  sleep "$SAMPLE_SEC"

  UI_STATUS="ok"
  if kill -0 "$UI_PID" >/dev/null 2>&1; then
    read -r UI_RSS_KB UI_CPU_PCT UI_THREADS UI_VSZ_KB < <(
      ps -o rss=,pcpu=,nlwp=,vsz= -p "$UI_PID" | awk 'NR==1 {print $1, $2, $3, $4}'
    )
  else
    UI_STATUS="failed_to_start"
    UI_RSS_KB="n/a"
    UI_CPU_PCT="n/a"
    UI_THREADS="n/a"
    UI_VSZ_KB="n/a"
  fi

  cargo tree --features ui >"$UI_TREE"
  UI_TREE_LINES="$(wc -l <"$UI_TREE")"
  if rg -q 'tiny_http' "$UI_TREE"; then
    UI_HAS_TINY_HTTP="yes"
  else
    UI_HAS_TINY_HTTP="no"
  fi

  echo
  echo "ui_profile:"
  echo "  status:                  $UI_STATUS"
  echo "  binary_unstripped_bytes: $UI_SIZE_RAW"
  echo "  binary_stripped_bytes:   $UI_SIZE_STRIPPED"
  echo "  rss_kb_idle:             $UI_RSS_KB"
  echo "  cpu_pct_idle:            $UI_CPU_PCT"
  echo "  threads_idle:            $UI_THREADS"
  echo "  virtual_mem_kb_idle:     $UI_VSZ_KB"
  echo "  cargo_tree_lines:        $UI_TREE_LINES"
  echo "  includes_tiny_http:      $UI_HAS_TINY_HTTP"

  if [[ "$UI_STATUS" != "ok" ]]; then
    echo
    echo "ui_start_log:"
    sed -n '1,40p' "$UI_LOG"
  fi
fi
