#!/usr/bin/env bash
set -euo pipefail

duration_sec=120
workers="all"
cpu_load=95
watch_status=1
status_url="http://127.0.0.1:8788/thermal/status"
gui_url="http://127.0.0.1:8788/"
open_gui=0
stress_pid=""
watch_pid=""

usage() {
  cat <<'EOF'
Usage: ./scripts/stress_test.sh [options]

Options:
  -d, --duration SEC     Test duration in seconds (default: 120)
  -w, --workers N|all    Number of CPU workers (default: all)
  -l, --load PCT         Per-worker CPU load percent (default: 95)
      --no-watch         Disable live nuc-powerd status output
      --status-url URL   Thermal status URL (default: http://127.0.0.1:8788/thermal/status)
      --gui-url URL      Dashboard URL (default: http://127.0.0.1:8788/)
      --open-gui         Open dashboard in a browser before test start
  -h, --help             Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -d|--duration)
      duration_sec="$2"; shift 2 ;;
    -w|--workers)
      workers="$2"; shift 2 ;;
    -l|--load)
      cpu_load="$2"; shift 2 ;;
    --no-watch)
      watch_status=0; shift ;;
    --status-url)
      status_url="$2"; shift 2 ;;
    --gui-url)
      gui_url="$2"; shift 2 ;;
    --open-gui)
      open_gui=1; shift ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      echo "Unknown arg: $1" >&2
      usage
      exit 1 ;;
  esac
done

is_uint() {
  [[ "$1" =~ ^[0-9]+$ ]]
}

if ! is_uint "$duration_sec" || [[ "$duration_sec" -lt 1 ]] || [[ "$duration_sec" -gt 86400 ]]; then
  echo "duration must be an integer in 1..=86400" >&2
  exit 1
fi

if ! is_uint "$cpu_load" || [[ "$cpu_load" -lt 1 ]] || [[ "$cpu_load" -gt 100 ]]; then
  echo "load must be an integer in 1..=100" >&2
  exit 1
fi

if ! command -v stress-ng >/dev/null 2>&1; then
  echo "stress-ng not found. Install it first (e.g. sudo apt-get install stress-ng)." >&2
  exit 1
fi

if [[ "$workers" == "all" ]]; then
  workers="$(nproc)"
fi
if ! is_uint "$workers" || [[ "$workers" -lt 1 ]] || [[ "$workers" -gt 512 ]]; then
  echo "workers must be 'all' or an integer in 1..=512" >&2
  exit 1
fi

cleanup() {
  if [[ -n "$watch_pid" ]] && kill -0 "$watch_pid" 2>/dev/null; then
    kill "$watch_pid" 2>/dev/null || true
    wait "$watch_pid" 2>/dev/null || true
  fi
  if [[ -n "$stress_pid" ]] && kill -0 "$stress_pid" 2>/dev/null; then
    kill "$stress_pid" 2>/dev/null || true
    wait "$stress_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

echo "Dashboard: $gui_url"
if curl -fsS "$gui_url" >/dev/null 2>&1; then
  echo "Dashboard reachable"
else
  echo "Dashboard not reachable at $gui_url (continuing stress test anyway)"
fi

if [[ "$open_gui" -eq 1 ]]; then
  if command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$gui_url" >/dev/null 2>&1 || true
  elif command -v open >/dev/null 2>&1; then
    open "$gui_url" >/dev/null 2>&1 || true
  else
    echo "No GUI opener found (xdg-open/open)."
  fi
fi

status_watcher() {
  while kill -0 "$1" 2>/dev/null; do
    ts="$(date +%H:%M:%S)"
    if raw="$(curl -fsS "$status_url" 2>/dev/null)"; then
      if command -v jq >/dev/null 2>&1; then
        mode="$(jq -r '.mode // "-"' <<<"$raw")"
        state="$(jq -r '.state // "-"' <<<"$raw")"
        health="$(jq -r '.health // "-"' <<<"$raw")"
        temp="$(jq -r '.temp_cpu_c // "-"' <<<"$raw")"
        freq="$(jq -r '.max_freq_khz // "-"' <<<"$raw")"
        turbo="$(jq -r '.no_turbo // "-"' <<<"$raw")"
        echo "[$ts] mode=$mode state=$state health=$health temp_c=$temp max_khz=$freq no_turbo=$turbo"
      else
        echo "[$ts] $(tr -d '\n' <<<"$raw")"
      fi
    else
      echo "[$ts] status unavailable at $status_url"
    fi
    sleep 1
  done
}

echo "Starting stress test: duration=${duration_sec}s workers=${workers} load=${cpu_load}%"
stress-ng --cpu "$workers" --cpu-load "$cpu_load" --timeout "${duration_sec}s" --metrics-brief &
stress_pid=$!

if [[ "$watch_status" -eq 1 ]]; then
  status_watcher "$stress_pid" &
  watch_pid=$!
fi

wait "$stress_pid"
stress_rc=$?

if [[ -n "$watch_pid" ]]; then
  wait "$watch_pid" || true
fi

echo "Stress test finished with exit code $stress_rc"
exit "$stress_rc"
