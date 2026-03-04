# Phase 2 Prompt: Body Server Integration

Use this prompt in a follow-up Codex session when ready to integrate with body server controls.

```text
Extend nuc-powerd with body-server-facing integration APIs while preserving the existing daemon behavior.

Goals:
- Keep current thermal control loop intact.
- Add a local control/status interface suitable for robot body-server integration.

Implement:
1) Read-only status endpoint contract (local only):
   GET /thermal/status
   response fields:
   {
     mode, state, health,
     temp_cpu_c, cpu_util_pct, pkg_power_w, freq_mhz,
     no_turbo, max_freq_khz,
     last_update_ms, message
   }

2) Control endpoints (guarded/local only):
   POST /thermal/mode       body: {"mode":"auto|eco|performance|paused"}
   POST /thermal/override   body: {"mode":"auto|eco|performance|paused","ttl_sec":int}
   POST /thermal/target     body: {"target_temp_c":float}

3) Behavior:
- paused: stop applying sysfs writes but keep monitoring/status updates.
- eco/performance: map to predefined profile sets.
- override with TTL expires back to auto.
- status always reports effective mode and override expiration.

4) Safety:
- panic temp always wins over manual mode.
- reject invalid mode transitions cleanly.
- persist minimal runtime state under /run/nuc-powerd/control.json.

5) Testing:
- unit tests for mode transitions and override expiry.
- integration test for API/status behavior with fake sensor/actuator.

6) Docs:
- update README with API and security notes.
- include curl examples for each endpoint.

Validation before finish:
- cargo fmt --check
- cargo clippy -- -D warnings
- cargo test
```
