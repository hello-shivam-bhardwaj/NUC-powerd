# Stretch Body II Adoption Plan (nuc-powerd)

Last updated: March 3, 2026

## Goal

Adopt `nuc-powerd` as a default background optimization service on Stretch robots while minimizing risk to existing runtime behavior in `stretch_body_ii`.

## Safety Assessment

Current assessment: **safe for staged rollout**, not yet fleet-wide default without canary validation.

Why this is reasonably safe:

- The core daemon is lightweight and separated from the optional UI sidecar.
- The daemon is already packaged as a hardened root systemd service.
- Existing `stretch_body_ii` fan logic for SE4 is currently mostly monitoring-oriented in `power_periph.step_sentry` (fan control block is commented), reducing direct fan-control conflict risk.
- Main risk is not protocol safety; it is runtime performance/timing side effects from CPU power policy changes.

## Integration Architecture

Keep a strict split:

1. **Always-on component**: `nuc-powerd` daemon (`nuc-powerd.service`)
2. **Optional debug tool**: `nuc-powerd-ui` (for testing and tuning only)

Do not make the UI part of production startup.

## What To Add In `stretch_body_ii`

Add wrapper tooling only (no protocol or control-loop changes):

- `src/tools/stretch_nuc_powerd_install.sh`
- `src/tools/stretch_nuc_powerd_service.sh`
- `src/tools/stretch_nuc_powerd_validate.sh`
- optional `src/tools/stretch_nuc_powerd_status.py`

Purpose of wrappers:

- Standardize install/start/stop/status from the Stretch workflow.
- Keep operators off raw systemd commands.
- Reuse existing `nuc-powerd` install/validate scripts.

## Phased Rollout Plan

### Phase 0: Preflight (single robot)

1. Verify conflicts are absent (`thermald`, `tlp`, `auto-cpufreq`, `power-profiles-daemon`).
2. Install daemon using hardened installer.
3. Run baseline validation.
4. Run stress validation.

Commands:

```bash
./scripts/install_service_hardened.sh
./scripts/validate_system.sh
./scripts/validate_system.sh --with-stress --stress-sec 60
```

### Phase 1: Fleet install without activation

Install binaries/config/service files everywhere first, but do not enable service yet:

```bash
./scripts/install_fleet_remote.sh --inventory /tmp/robots.inventory --no-enable
```

### Phase 2: Canary activation

1. Enable/start on one robot.
2. Soak 24-48 hours with representative workloads.
3. Confirm no regression in control stability, thermal safety, and compute performance.

### Phase 3: Progressive rollout

- 10% robots -> 50% robots -> 100% robots.
- At each stage, gate on validation pass criteria below.

## Validation Gates (Go/No-Go)

Required before each rollout expansion:

1. `nuc-powerd.service` stays active without restart loops.
2. `/run/nuc-powerd/status.json` timestamp advances continuously.
3. No repeated `controller tick failed` in `journalctl -u nuc-powerd.service`.
4. Stress validation passes on representative workloads.
5. No observed regression in robot control responsiveness or loop stability.

## Required Install Dependencies On Robot

Required:

- Linux with `systemd`
- `sudo`/root access
- CPU sysfs nodes for cpufreq/intel_pstate controls
- `jq` (status parsing in validation script)

Optional:

- `stress-ng` (stress validation)
- `cargo` (only if building on robot; not needed when shipping prebuilt binary)

## Runtime Footprint (Reference)

Measured on reference machine:

- `nuc-powerd` stripped binary: ~1.63 MB
- `nuc-powerd-ui` stripped binary: ~2.03 MB
- daemon idle RSS: ~3.6 MB, ~0.0% CPU
- UI idle RSS: ~3.9 MB, ~0.0% CPU

Conclusion: daemon is lightweight enough for always-on background use, UI should remain optional.

## Conflict and Risk Controls

1. Single owner policy for CPU power/thermal controls.
2. Keep loopback-only API binding for UI (`127.0.0.1`).
3. Keep hardened daemon unit settings.
4. Keep panic thermal override enabled.
5. Maintain documented rollback path.

## Rollback Plan

Immediate rollback on any regression:

```bash
sudo systemctl disable --now nuc-powerd.service
sudo systemctl status nuc-powerd.service --no-pager
```

If needed, restore backups created by fleet installer (`/var/backups/nuc-powerd-*`).

## Decision

Proceed with **canary-first adoption**. Mark as default in `stretch_body_ii` only after staged rollout gates pass.
