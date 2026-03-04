# nuc-powerd

`nuc-powerd` is a lightweight Rust daemon for Intel NUC systems that continuously
applies thermal-aware CPU policy limits for efficiency and safety.

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

## Repository Layout

- `src/config.rs`: TOML config loading/validation
- `src/sensors.rs`: Linux telemetry readers
- `src/actuators.rs`: sysfs writers + rollback support
- `src/policy.rs`: state-machine transitions and hysteresis
- `src/controller.rs`: control loop tick logic
- `src/status.rs`: runtime status JSON types/writer
- `src/main.rs`: CLI (`run`, `dry-run`, `status`, `doctor`)
- `config/nuc-powerd.example.toml`: Intel NUC baseline profile
- `packaging/nuc-powerd.service`: systemd unit
- `scripts/install.sh`: build/install/start helper
- `scripts/uninstall.sh`: remove helper

## Prerequisites

- Linux on Intel NUC
- `rustc` + `cargo`
- permission to write relevant sysfs nodes (typically root/systemd service)

## Build And Validate

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
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

## Systemd Install

```bash
./scripts/install.sh
sudo systemctl status nuc-powerd.service --no-pager
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

## Tuning Workflow

1. Start with `dry-run` and inspect status output.
2. Enable active mode and run workload (`stress-ng` or representative robot stack).
3. Adjust hysteresis thresholds and state profiles gradually.
4. Re-run soak tests and monitor oscillation/thermal headroom.
