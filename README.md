# nuc-powerd

`nuc-powerd` is a lightweight Rust daemon for Intel NUC systems.
It applies thermal-aware CPU policy limits to balance performance and efficiency.

## Scope

- Monitor CPU temperature/utilization and optional package power.
- Apply policy controls using Linux sysfs knobs.
- Run as a systemd background service.

## Quick Start

1. Install Rust toolchain (`cargo` + `rustc`).
2. Build:

```bash
cargo build --release
```

3. Run in dry mode:

```bash
./target/release/nuc-powerd --dry-run --config config/nuc-powerd.example.toml
```

## Initial Layout

- `src/main.rs`: daemon entrypoint skeleton.
- `config/nuc-powerd.example.toml`: baseline Intel NUC profile.
- `packaging/nuc-powerd.service`: example systemd unit.
- `scripts/install.sh`: install helper.
- `scripts/uninstall.sh`: uninstall helper.

## Next Build Steps

- Implement telemetry collectors (`temp`, `util`, `freq`, `power`).
- Implement sysfs actuators (`EPP`, `max_freq`, `no_turbo`, `RAPL`).
- Implement state machine controller (`cool`, `warm`, `hot`, `critical`).
