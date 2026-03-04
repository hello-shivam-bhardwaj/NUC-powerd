# nuc-powerd

`nuc-powerd` is a lightweight Rust daemon for Intel NUC systems that continuously
applies thermal-aware CPU policy limits for efficiency and safety.

The project is now split into:

- `nuc-powerd` (core daemon, minimal runtime footprint, no web stack)
- `nuc-powerd-ui` (optional local web/API sidecar, built only with `--features ui`)

## What It Does

- Reads telemetry:
  - CPU temperature (thermal zones)
  - CPU utilization (`/proc/stat`)
  - current CPU frequency (`scaling_cur_freq`)
  - optional package power (RAPL energy delta)
- Applies controls through sysfs:
  - `energy_performance_preference`
  - `scaling_max_freq`
  - `intel_pstate/no_turbo`
  - optional RAPL package power limit
- Uses a thermal state machine with hysteresis and dwell time:
  - `cool` -> `warm` -> `hot` -> `critical`
- Writes runtime status to JSON (`/run/nuc-powerd/status.json` by default).
- Persists runtime control state under `/run/nuc-powerd/control.json`.
- Optional sidecar exposes local integration API + web dashboard (`127.0.0.1:8788` by default).

## Repository Layout

- `src/config.rs`: TOML config loading/validation
- `src/sensors.rs`: Linux telemetry readers
- `src/actuators.rs`: sysfs writers + rollback support
- `src/policy.rs`: state-machine transitions and hysteresis
- `src/controller.rs`: control loop tick logic
- `src/status.rs`: runtime status JSON types/writer
- `src/main.rs`: CLI (`run`, `dry-run`, `status`, `doctor`)
- `src/bin/nuc-powerd-ui.rs`: optional web/API sidecar
- `config/nuc-powerd.example.toml`: Intel NUC baseline profile
- `packaging/nuc-powerd.service`: systemd unit
- `docs/SERVICE_INSTALL_AND_VALIDATION.md`: service install + validation runbook
- `scripts/install.sh`: build/install/start helper
- `scripts/install_service_hardened.sh`: hardened service installer (recommended)
- `scripts/install_fleet_remote.sh`: multi-robot SSH deployment helper
- `scripts/validate_system.sh`: end-to-end service validator
- `scripts/uninstall.sh`: remove helper
- `scripts/characterize_daemon.sh`: reproducible footprint profiling

## Prerequisites

- Linux on Intel NUC
- `rustc` + `cargo`
- permission to write relevant sysfs nodes (typically root/systemd service)

## Build And Validate

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test

# Minimal daemon build (default)
cargo build --release --bin nuc-powerd

# Optional UI/API sidecar build
cargo build --release --features ui --bin nuc-powerd-ui
```

Characterize footprint on the current machine:

```bash
# Daemon only (recommended for real deployment sizing)
./scripts/characterize_daemon.sh

# Include optional UI sidecar comparison
./scripts/characterize_daemon.sh --with-ui
```

## CLI Usage

Use default example config:

```bash
# Active mode (writes sysfs)
./target/release/nuc-powerd run --config config/nuc-powerd.example.toml

# Dry run (no sysfs writes)
./target/release/nuc-powerd dry-run --config config/nuc-powerd.example.toml

# Print latest status JSON
./target/release/nuc-powerd status --config config/nuc-powerd.example.toml

# Environment checks + conflict checks
./target/release/nuc-powerd doctor --config config/nuc-powerd.example.toml
```

Note: You can also run with no subcommand (`nuc-powerd --config ...`) and it defaults to `run`.

## Optional UI Sidecar

Start the daemon and UI as separate processes:

```bash
# Terminal 1: lightweight daemon
./target/release/nuc-powerd run --config config/nuc-powerd.example.toml

# Terminal 2: optional local UI/API (requires --features ui build)
./target/release/nuc-powerd-ui --config config/nuc-powerd.example.toml
```

The UI sidecar reads `status_path` and writes `control_path`; the daemon consumes `control_path` on each tick.
Control updates are serialized with a lock file to avoid daemon/UI write races.

## Local Thermal API (Sidecar)

`nuc-powerd-ui` exposes a loopback-only JSON API suitable for body-server integration.
Default bind address is `127.0.0.1:8788` (`[daemon].api_bind` in config).

Web dashboard (served by the same local server):

```bash
xdg-open http://127.0.0.1:8788/
# or just open that URL in your browser
```

Dashboard includes built-in stress controls (start/stop + live process status) so you can
run load tests directly from the UI while watching thermal behavior. It also includes daemon
service controls (start/stop/restart + live service state) for `nuc-powerd.service`.

Status endpoint:

```bash
curl -sS http://127.0.0.1:8788/thermal/status | jq .
```

Expected response fields:

- `mode`, `state`, `health`
- `temp_cpu_c`, `cpu_util_pct`, `pkg_power_w`, `freq_mhz`
- `no_turbo`, `max_freq_khz`
- `override_expires_ms`, `target_temp_c`
- `target_min_c`, `target_max_c`
- `last_update_ms`, `message`

Set base mode:

```bash
curl -sS -X POST http://127.0.0.1:8788/thermal/mode \
  -H 'Content-Type: application/json' \
  -d '{"mode":"auto"}'

curl -sS -X POST http://127.0.0.1:8788/thermal/mode \
  -H 'Content-Type: application/json' \
  -d '{"mode":"eco"}'

curl -sS -X POST http://127.0.0.1:8788/thermal/mode \
  -H 'Content-Type: application/json' \
  -d '{"mode":"performance"}'

curl -sS -X POST http://127.0.0.1:8788/thermal/mode \
  -H 'Content-Type: application/json' \
  -d '{"mode":"paused"}'
```

Set temporary override mode with TTL (seconds):

```bash
curl -sS -X POST http://127.0.0.1:8788/thermal/override \
  -H 'Content-Type: application/json' \
  -d '{"mode":"eco","ttl_sec":30}'
```

Set target temperature:

```bash
curl -sS -X POST http://127.0.0.1:8788/thermal/target \
  -H 'Content-Type: application/json' \
  -d '{"target_temp_c":76.0}'
```

Start stress test from API:

```bash
curl -sS -X POST http://127.0.0.1:8788/stress/start \
  -H 'Content-Type: application/json' \
  -d '{"duration_sec":90,"workers":4,"cpu_load":95}'
```

Read stress process status:

```bash
curl -sS http://127.0.0.1:8788/stress/status | jq .
```

Stop stress process:

```bash
curl -sS -X POST http://127.0.0.1:8788/stress/stop | jq .
```

Get daemon service status:

```bash
curl -sS http://127.0.0.1:8788/service/status | jq .
```

Control daemon service:

```bash
curl -sS -X POST http://127.0.0.1:8788/service/start | jq .
curl -sS -X POST http://127.0.0.1:8788/service/stop | jq .
curl -sS -X POST http://127.0.0.1:8788/service/restart | jq .
```

Behavior notes:

- `paused` stops sysfs writes but continues telemetry/status updates.
- `panic_temp_c` safety override always enforces critical profile behavior.
- override mode expires back to `auto`.
- service-control endpoints require the UI sidecar to have permission to run `systemctl`
  (run as root, or configure passwordless `sudo systemctl` for that user).

## Systemd Install

```bash
# Minimal install (daemon only)
./scripts/install.sh

# Recommended: hardened service install + start
./scripts/install_service_hardened.sh

# Optional: also install nuc-powerd-ui binary
./scripts/install.sh --with-ui

sudo systemctl status nuc-powerd.service --no-pager
```

Fleet rollout (multiple robots over SSH):

```bash
# 1) copy and edit inventory
cp scripts/robots.inventory.example /tmp/robots.inventory

# 2) deploy to all inventory hosts
./scripts/install_fleet_remote.sh --inventory /tmp/robots.inventory
```

Safe canary rollout pattern:

```bash
# install files only on fleet (do not start service yet)
./scripts/install_fleet_remote.sh --inventory /tmp/robots.inventory --no-enable

# on one canary robot, start + validate manually first
ssh hello-robot@10.1.10.21 "sudo systemctl enable --now nuc-powerd.service && cd ~/nuc-powerd && ./scripts/validate_system.sh"
```

Validate running system:

```bash
# Baseline validation (service active, status freshness, no recent tick errors)
./scripts/validate_system.sh

# Full validation with stress workload
./scripts/validate_system.sh --with-stress --stress-sec 60
```

Uninstall:

```bash
./scripts/uninstall.sh
```

## Safety Notes

- Keep one thermal/power controller owner at a time. If `thermald`, `tlp`, or
  `auto-cpufreq` are active, they may conflict with `nuc-powerd`.
- `panic_temp_c` forces `critical` profile behavior.
- `rollback_on_error` attempts to restore prior sysfs values if write steps fail.
- API is intended for local integration only: bind to loopback and avoid exposing
  it on external interfaces.

## Footprint Snapshot

Measured on this machine on March 4, 2026, using release builds:

- `nuc-powerd` binary size:
  - unstripped: `14,966,920` bytes
  - stripped: `1,632,720` bytes
- `nuc-powerd-ui` binary size:
  - unstripped: `15,480,104` bytes
  - stripped: `2,030,048` bytes

Idle runtime sample:

- daemon (`nuc-powerd dry-run`): `VmRSS ~3.6 MB`, `%CPU ~0.0`
- UI sidecar (`nuc-powerd-ui`): `VmRSS ~3.9 MB`, `%CPU ~0.0`

This keeps the daemon itself minimal while allowing UI/API to remain optional.

## Tuning Workflow

1. Start with `dry-run` and inspect status output.
2. Enable active mode and run workload (`stress-ng` or representative robot stack).
3. Adjust hysteresis thresholds and state profiles gradually.
4. Re-run soak tests and monitor oscillation/thermal headroom.
