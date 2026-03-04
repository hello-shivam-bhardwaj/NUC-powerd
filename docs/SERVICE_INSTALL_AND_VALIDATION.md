# Service Install And Validation (Reference Machine)

This document captures what was required to install and run `nuc-powerd` as a
systemd service on a real machine, plus the issues we hit and how to fix them.

Validation date: March 3, 2026  
Host: Ubuntu 24.04-based system, `systemd 255`

## Prerequisites

- Linux host with systemd.
- Root/sudo access.
- Rust toolchain (`cargo`) if building locally.
- CPU control sysfs nodes available:
  - `/sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference`
  - `/sys/devices/system/cpu/cpufreq/policy0/scaling_max_freq`
  - `/sys/devices/system/cpu/intel_pstate/no_turbo`
- Optional RAPL control (if enabled in config):
  - `/sys/class/powercap/intel-rapl/intel-rapl:0/constraint_0_power_limit_uw`
- If using Web UI service buttons (`start/stop/restart`):
  - run `nuc-powerd-ui` as root, or
  - allow passwordless `sudo systemctl` for the UI user.

Example sudoers rule (adjust user as needed):

```bash
echo 'hello-robot ALL=(root) NOPASSWD:/bin/systemctl * nuc-powerd.service,* /usr/bin/systemctl * nuc-powerd.service,*' | sudo tee /etc/sudoers.d/nuc-powerd-ui
sudo chmod 0440 /etc/sudoers.d/nuc-powerd-ui
```

## Install Steps (What Worked Here)

1. Build daemon binary:

```bash
cd /home/hello-robot/workspace/nuc-powerd
cargo build --release --bin nuc-powerd
```

2. Install binary, unit, config:

```bash
sudo install -D -m 0755 target/release/nuc-powerd /usr/local/bin/nuc-powerd
sudo install -D -m 0644 packaging/nuc-powerd.service /etc/systemd/system/nuc-powerd.service
sudo install -D -m 0644 config/nuc-powerd.example.toml /etc/nuc-powerd.toml
sudo install -d -m 0755 /run/nuc-powerd
```

3. Load and start service:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now nuc-powerd.service
```

4. Validate service:

```bash
sudo systemctl status nuc-powerd.service --no-pager -l
sudo journalctl -u nuc-powerd.service -n 80 --no-pager
sudo jq '{mode,state,health,last_update_ms}' /run/nuc-powerd/status.json
```

## One-Command Install/Test Scripts

Use these for repeatable deployment on new machines:

```bash
# Install hardened service profile and start daemon
./scripts/install_service_hardened.sh

# Validate service end-to-end
./scripts/validate_system.sh

# Optional: validate under stress load
./scripts/validate_system.sh --with-stress --stress-sec 60
```

## Web UI Service Controls

When `nuc-powerd-ui` is running, the dashboard includes:

- live daemon service status (`active_state`, `enabled_state`, `pid`)
- `Start`, `Stop`, and `Restart` buttons for `nuc-powerd.service`

API equivalents:

```bash
curl -sS http://127.0.0.1:8788/service/status | jq .
curl -sS -X POST http://127.0.0.1:8788/service/start | jq .
curl -sS -X POST http://127.0.0.1:8788/service/stop | jq .
curl -sS -X POST http://127.0.0.1:8788/service/restart | jq .
```

## Hardened Unit Notes (Important)

The current hardened unit in `packaging/nuc-powerd.service` is validated with:

- loopback/network blocked for daemon (`PrivateNetwork=true`, `IPAddressDeny=any`)
- syscall filtering and namespace restrictions
- bounded capabilities (`CapabilityBoundingSet=CAP_SYS_ADMIN CAP_DAC_OVERRIDE`)

Two hardening details are required for runtime correctness:

1. Do not hide `/proc/stat`:
   - `nuc-powerd` reads `/proc/stat` for CPU utilization telemetry.
   - `ProtectProc=invisible` + `ProcSubset=pid` breaks this with:
     `failed to read /proc/stat: No such file or directory`.

2. Allow RAPL real path (not just class path):
   - RAPL class path resolves via symlink to `/sys/devices/virtual/powercap/...`.
   - `ReadWritePaths` must include `/sys/devices/virtual/powercap` in hardened mode.

Current working line:

```ini
ReadWritePaths=/run/nuc-powerd /sys/devices/system/cpu /sys/class/powercap /sys/devices/virtual/powercap
```

## Known Failure Modes And Fixes

1. Service flaps with `/proc/stat` missing  
Fix: remove `ProtectProc=invisible` and `ProcSubset=pid` from unit.

2. Service fails with RAPL `Read-only file system` under systemd sandbox  
Fix: include `/sys/devices/virtual/powercap` in `ReadWritePaths`.

3. `Start request repeated too quickly`  
Fix:

```bash
sudo systemctl reset-failed nuc-powerd.service
sudo systemctl restart nuc-powerd.service
```

4. Controller conflicts (`thermald`, etc.)  
Fix: run doctor and disable competing controllers if needed.

```bash
sudo /usr/local/bin/nuc-powerd --config /etc/nuc-powerd.toml doctor
```

## Quick Bring-Up Checklist For New Machines

1. Build and install daemon + unit + config.
2. Confirm sysfs control files exist.
3. Confirm unit has the tested `ReadWritePaths`.
4. `daemon-reload`, `enable --now`.
5. Verify `status.json` updates over time.
6. Check journal for actuator write failures.
