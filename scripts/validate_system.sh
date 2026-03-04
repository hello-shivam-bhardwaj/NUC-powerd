#!/usr/bin/env bash
set -euo pipefail

SERVICE="nuc-powerd.service"
STATUS_JSON="/run/nuc-powerd/status.json"
SAMPLE_SEC=2
WITH_STRESS=0
STRESS_SEC=30
STRESS_LOAD=95
STRESS_WORKERS="$(nproc)"

usage() {
  cat <<'EOF'
Usage: ./scripts/validate_system.sh [options]

Validates a running nuc-powerd systemd service end-to-end.

Options:
  --sample-sec N      Seconds between status samples (default: 2)
  --with-stress       Run stress-ng workload while monitoring status
  --stress-sec N      Stress duration in seconds (default: 30)
  --stress-load N     stress-ng --cpu-load (default: 95)
  --stress-workers N  stress-ng --cpu workers (default: nproc)
  -h, --help          Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --sample-sec)
      [[ $# -ge 2 ]] || { usage; exit 1; }
      SAMPLE_SEC="$2"
      shift 2
      ;;
    --with-stress)
      WITH_STRESS=1
      shift
      ;;
    --stress-sec)
      [[ $# -ge 2 ]] || { usage; exit 1; }
      STRESS_SEC="$2"
      shift 2
      ;;
    --stress-load)
      [[ $# -ge 2 ]] || { usage; exit 1; }
      STRESS_LOAD="$2"
      shift 2
      ;;
    --stress-workers)
      [[ $# -ge 2 ]] || { usage; exit 1; }
      STRESS_WORKERS="$2"
      shift 2
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

is_uint() {
  [[ "$1" =~ ^[0-9]+$ ]]
}

if ! is_uint "$SAMPLE_SEC" || [[ "$SAMPLE_SEC" -lt 1 ]]; then
  echo "--sample-sec must be a positive integer" >&2
  exit 1
fi
if ! is_uint "$STRESS_SEC" || [[ "$STRESS_SEC" -lt 1 ]]; then
  echo "--stress-sec must be a positive integer" >&2
  exit 1
fi
if ! is_uint "$STRESS_LOAD" || [[ "$STRESS_LOAD" -lt 1 ]] || [[ "$STRESS_LOAD" -gt 100 ]]; then
  echo "--stress-load must be in 1..=100" >&2
  exit 1
fi
if ! is_uint "$STRESS_WORKERS" || [[ "$STRESS_WORKERS" -lt 1 ]]; then
  echo "--stress-workers must be a positive integer" >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required for validation output parsing" >&2
  exit 1
fi

echo "[validate] checking service is active"
if ! sudo systemctl is-active --quiet "$SERVICE"; then
  echo "[validate] service is not active"
  sudo systemctl status "$SERVICE" --no-pager -l || true
  sudo journalctl -u "$SERVICE" -n 120 --no-pager || true
  exit 1
fi

if [[ ! -f "$STATUS_JSON" ]]; then
  echo "[validate] status file missing: $STATUS_JSON" >&2
  exit 1
fi

read_status_field() {
  local field="$1"
  sudo jq -r "$field" "$STATUS_JSON"
}

echo "[validate] checking status freshness"
t1="$(read_status_field '.last_update_ms')"
sleep "$SAMPLE_SEC"
t2="$(read_status_field '.last_update_ms')"
if ! is_uint "$t1" || ! is_uint "$t2" || [[ "$t2" -le "$t1" ]]; then
  echo "[validate] status timestamp did not advance ($t1 -> $t2)" >&2
  exit 1
fi

echo "[validate] status is updating ($t1 -> $t2)"
sudo jq '{mode,state,health,temp_cpu_c,cpu_util_pct,pkg_power_w,max_freq_khz,no_turbo,last_update_ms}' "$STATUS_JSON"

echo "[validate] checking for recent controller tick errors"
if sudo journalctl -u "$SERVICE" --since "-2 min" --no-pager | rg -q "controller tick failed"; then
  echo "[validate] detected recent controller tick failure(s) in journal" >&2
  sudo journalctl -u "$SERVICE" --since "-2 min" --no-pager
  exit 1
fi

if [[ "$WITH_STRESS" -eq 0 ]]; then
  echo "[validate] PASS (baseline)"
  exit 0
fi

if ! command -v stress-ng >/dev/null 2>&1; then
  echo "stress-ng is required for --with-stress validation" >&2
  exit 1
fi

echo "[validate] running stress-ng for ${STRESS_SEC}s (workers=${STRESS_WORKERS}, load=${STRESS_LOAD}%)"
stress-ng --cpu "$STRESS_WORKERS" --cpu-load "$STRESS_LOAD" --timeout "${STRESS_SEC}s" --metrics-brief \
  >/tmp/nuc-powerd.validate.stress.log 2>&1 &
stress_pid="$!"

max_temp=0
max_util=0
end_ts=$((SECONDS + STRESS_SEC))
while kill -0 "$stress_pid" 2>/dev/null && [[ "$SECONDS" -lt "$end_ts" ]]; do
  temp="$(read_status_field '.temp_cpu_c // 0')"
  util="$(read_status_field '.cpu_util_pct // 0')"

  temp_i="${temp%.*}"
  util_i="${util%.*}"
  if [[ "$temp_i" -gt "$max_temp" ]]; then
    max_temp="$temp_i"
  fi
  if [[ "$util_i" -gt "$max_util" ]]; then
    max_util="$util_i"
  fi
  sleep 1
done
wait "$stress_pid"

echo "[validate] stress max observed: temp=${max_temp}C util=${max_util}%"

if ! sudo systemctl is-active --quiet "$SERVICE"; then
  echo "[validate] service became inactive during stress test" >&2
  sudo systemctl status "$SERVICE" --no-pager -l || true
  sudo journalctl -u "$SERVICE" -n 120 --no-pager || true
  exit 1
fi

if sudo journalctl -u "$SERVICE" --since "-5 min" --no-pager | rg -q "controller tick failed"; then
  echo "[validate] detected controller failures during stress window" >&2
  sudo journalctl -u "$SERVICE" --since "-5 min" --no-pager
  exit 1
fi

echo "[validate] PASS (with stress)"
