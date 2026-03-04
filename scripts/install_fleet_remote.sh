#!/usr/bin/env bash
set -euo pipefail

INVENTORY=""
REPO_URL="https://github.com/hello-shivam-bhardwaj/NUC-powerd.git"
BRANCH="main"
REMOTE_DIR="~/nuc-powerd"
CONFIG_SRC="config/nuc-powerd.example.toml"
SKIP_BUILD=0
NO_ENABLE=0
SKIP_VALIDATE=0
ALLOW_CONFLICTS=0
CONTINUE_ON_ERROR=0
DRY_RUN=0
SSH_CONNECT_TIMEOUT=10

usage() {
  cat <<'EOF'
Usage: ./scripts/install_fleet_remote.sh --inventory PATH [options]

Deploys nuc-powerd to multiple robots over SSH.

Inventory format (pipe-delimited, one host per line):
  name|host|user|port|identity_file

Examples:
  stretch-a|10.1.10.21|hello-robot|22|~/.ssh/stretch_a
  stretch-b|robot-b.local|hello-robot|2222|~/.ssh/stretch_b
  # user/port/key are optional:
  stretch-c|10.1.10.23|||

Options:
  --inventory PATH      Inventory file path (required)
  --repo-url URL        Git repo URL (default: https://github.com/hello-shivam-bhardwaj/NUC-powerd.git)
  --branch NAME         Branch to deploy (default: main)
  --remote-dir PATH     Checkout path on robot (default: ~/nuc-powerd)
  --config-src PATH     Config path relative to remote repo (default: config/nuc-powerd.example.toml)
  --skip-build          Pass --skip-build to install script
  --no-enable           Install files only; do not enable/start service
  --skip-validate       Skip ./scripts/validate_system.sh
  --allow-conflicts     Proceed even if known conflicting services are active
  --continue-on-error   Continue to next host if one host fails
  --dry-run             Print actions without executing them
  -h, --help            Show this help
EOF
}

trim() {
  local s="$1"
  s="${s#"${s%%[![:space:]]*}"}"
  s="${s%"${s##*[![:space:]]}"}"
  printf "%s" "$s"
}

validate_no_whitespace() {
  local label="$1"
  local value="$2"
  if [[ "$value" =~ [[:space:]] ]]; then
    echo "$label must not contain whitespace: $value" >&2
    exit 1
  fi
}

run_host() {
  local name="$1"
  local host="$2"
  local user="$3"
  local port="$4"
  local identity="$5"

  local target="$host"
  if [[ -n "$user" ]]; then
    target="${user}@${host}"
  fi

  if [[ -n "$identity" && "$identity" == "~/"* ]]; then
    identity="${HOME}/${identity#~/}"
  fi
  if [[ -n "$identity" && ! -f "$identity" ]]; then
    echo "[$name] identity file not found: $identity" >&2
    return 1
  fi

  local -a ssh_cmd=(
    ssh
    -o BatchMode=yes
    -o ConnectTimeout="$SSH_CONNECT_TIMEOUT"
    -o StrictHostKeyChecking=accept-new
  )
  if [[ -n "$port" ]]; then
    ssh_cmd+=(-p "$port")
  fi
  if [[ -n "$identity" ]]; then
    ssh_cmd+=(-i "$identity")
  fi
  ssh_cmd+=("$target")

  remote() {
    local cmd="$1"
    if [[ "$DRY_RUN" -eq 1 ]]; then
      echo "[dry-run][$name] ${ssh_cmd[*]} bash -lc $(printf '%q' "$cmd")"
      return 0
    fi
    "${ssh_cmd[@]}" "bash -lc $(printf '%q' "$cmd")"
  }

  echo "[$name] connecting to $target"
  remote "echo connected"

  echo "[$name] preflight checks"
  local preflight_cmd
  preflight_cmd=$(cat <<EOF
set -euo pipefail
command -v git >/dev/null
command -v sudo >/dev/null
command -v systemctl >/dev/null
sudo -n true >/dev/null
EOF
)
  if [[ "$SKIP_BUILD" -eq 0 ]]; then
    preflight_cmd+=$'\n'
    preflight_cmd+="command -v cargo >/dev/null"
  fi
  remote "$preflight_cmd"

  echo "[$name] syncing repo"
  local sync_cmd
  sync_cmd=$(cat <<EOF
set -euo pipefail
remote_dir=$(printf '%q' "$REMOTE_DIR")
branch=$(printf '%q' "$BRANCH")
repo_url=$(printf '%q' "$REPO_URL")
if [ -d "\$remote_dir/.git" ]; then
  git -C "\$remote_dir" fetch origin "\$branch"
  git -C "\$remote_dir" checkout "\$branch"
  git -C "\$remote_dir" pull --ff-only origin "\$branch"
else
  if [ -e "\$remote_dir" ]; then
    echo "remote path exists but is not a git repo: \$remote_dir" >&2
    exit 1
  fi
  git clone --branch "\$branch" --single-branch "\$repo_url" "\$remote_dir"
fi
EOF
)
  remote "$sync_cmd"

  if [[ "$ALLOW_CONFLICTS" -eq 0 ]]; then
    echo "[$name] checking for known service conflicts"
    local conflict_cmd
    conflict_cmd=$(cat <<'EOF'
set -euo pipefail
conflicts=()
for svc in thermald.service tlp.service auto-cpufreq.service power-profiles-daemon.service; do
  if sudo systemctl is-active --quiet "$svc"; then
    conflicts+=("$svc")
  fi
done
if [ "${#conflicts[@]}" -gt 0 ]; then
  echo "conflicting services active: ${conflicts[*]}" >&2
  exit 42
fi
EOF
)
    remote "$conflict_cmd"
  fi

  echo "[$name] backing up existing local install files"
  local backup_cmd
  backup_cmd=$(cat <<'EOF'
set -euo pipefail
backup_dir="/var/backups/nuc-powerd-$(date +%Y%m%d-%H%M%S)"
sudo mkdir -p "$backup_dir"
for file in /usr/local/bin/nuc-powerd /etc/nuc-powerd.toml /etc/systemd/system/nuc-powerd.service; do
  if [ -e "$file" ]; then
    sudo cp -a "$file" "$backup_dir/"
  fi
done
echo "backup_dir=$backup_dir"
EOF
)
  remote "$backup_cmd"

  local -a install_args=("--config-src" "$CONFIG_SRC")
  if [[ "$SKIP_BUILD" -eq 1 ]]; then
    install_args+=("--skip-build")
  fi
  if [[ "$NO_ENABLE" -eq 1 ]]; then
    install_args+=("--no-enable")
  fi
  local install_args_quoted=""
  local arg
  for arg in "${install_args[@]}"; do
    install_args_quoted+=" $(printf '%q' "$arg")"
  done

  echo "[$name] installing service"
  local install_cmd
  install_cmd=$(cat <<EOF
set -euo pipefail
remote_dir=$(printf '%q' "$REMOTE_DIR")
cd "\$remote_dir"
./scripts/install_service_hardened.sh$install_args_quoted
EOF
)
  remote "$install_cmd"

  if [[ "$SKIP_VALIDATE" -eq 0 && "$NO_ENABLE" -eq 0 ]]; then
    echo "[$name] validating system"
    local validate_cmd
    validate_cmd=$(cat <<EOF
set -euo pipefail
remote_dir=$(printf '%q' "$REMOTE_DIR")
cd "\$remote_dir"
./scripts/validate_system.sh
EOF
)
    remote "$validate_cmd"
  fi

  if [[ "$NO_ENABLE" -eq 0 ]]; then
    echo "[$name] service footprint summary"
    local summary_cmd
    summary_cmd=$(cat <<'EOF'
set -euo pipefail
if sudo systemctl is-active --quiet nuc-powerd.service; then
  pid="$(sudo systemctl show -p MainPID --value nuc-powerd.service)"
  echo "service=active pid=$pid"
  if [[ "$pid" =~ ^[0-9]+$ ]] && [[ "$pid" -gt 0 ]]; then
    ps -o rss=,pcpu=,nlwp=,comm= -p "$pid" | awk 'NR==1 {printf "rss_kb=%s cpu_pct=%s threads=%s cmd=%s\n",$1,$2,$3,$4}'
  fi
else
  echo "service=inactive"
fi
EOF
)
    remote "$summary_cmd"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --inventory)
      [[ $# -ge 2 ]] || {
        usage
        exit 1
      }
      INVENTORY="$2"
      shift 2
      ;;
    --repo-url)
      [[ $# -ge 2 ]] || {
        usage
        exit 1
      }
      REPO_URL="$2"
      shift 2
      ;;
    --branch)
      [[ $# -ge 2 ]] || {
        usage
        exit 1
      }
      BRANCH="$2"
      shift 2
      ;;
    --remote-dir)
      [[ $# -ge 2 ]] || {
        usage
        exit 1
      }
      REMOTE_DIR="$2"
      shift 2
      ;;
    --config-src)
      [[ $# -ge 2 ]] || {
        usage
        exit 1
      }
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
    --skip-validate)
      SKIP_VALIDATE=1
      shift
      ;;
    --allow-conflicts)
      ALLOW_CONFLICTS=1
      shift
      ;;
    --continue-on-error)
      CONTINUE_ON_ERROR=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
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

if [[ -z "$INVENTORY" ]]; then
  echo "--inventory is required" >&2
  usage
  exit 1
fi
if [[ ! -f "$INVENTORY" ]]; then
  echo "inventory not found: $INVENTORY" >&2
  exit 1
fi

validate_no_whitespace "repo-url" "$REPO_URL"
validate_no_whitespace "branch" "$BRANCH"
validate_no_whitespace "remote-dir" "$REMOTE_DIR"
validate_no_whitespace "config-src" "$CONFIG_SRC"

success_count=0
failure_count=0

line_no=0
while IFS= read -r raw_line || [[ -n "$raw_line" ]]; do
  line_no=$((line_no + 1))
  raw_line="${raw_line%$'\r'}"
  line="$(trim "$raw_line")"
  if [[ -z "$line" || "$line" == \#* ]]; then
    continue
  fi

  IFS='|' read -r raw_name raw_host raw_user raw_port raw_identity extra <<<"$line"
  if [[ -n "${extra:-}" ]]; then
    echo "inventory parse error on line $line_no: expected 5 columns max" >&2
    exit 1
  fi

  name="$(trim "${raw_name:-}")"
  host="$(trim "${raw_host:-}")"
  user="$(trim "${raw_user:-}")"
  port="$(trim "${raw_port:-}")"
  identity="$(trim "${raw_identity:-}")"

  if [[ -z "$name" || -z "$host" ]]; then
    echo "inventory parse error on line $line_no: name and host are required" >&2
    exit 1
  fi
  if [[ -z "$port" ]]; then
    port="22"
  fi
  if ! [[ "$port" =~ ^[0-9]+$ ]]; then
    echo "inventory parse error on line $line_no: invalid port '$port'" >&2
    exit 1
  fi

  echo
  echo "=== [$name] deploy start ==="
  if run_host "$name" "$host" "$user" "$port" "$identity"; then
    success_count=$((success_count + 1))
    echo "=== [$name] deploy success ==="
  else
    failure_count=$((failure_count + 1))
    echo "=== [$name] deploy failed ===" >&2
    if [[ "$CONTINUE_ON_ERROR" -eq 0 ]]; then
      break
    fi
  fi
done <"$INVENTORY"

echo
echo "fleet deploy summary: success=$success_count failed=$failure_count"
if [[ "$failure_count" -gt 0 ]]; then
  exit 1
fi
