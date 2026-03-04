use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::actuators::{Actuator, AppliedControls};
use crate::config::{Config, HysteresisConfig, StateProfile};
use crate::control::{now_unix_ms, with_locked_control_state, ControlMode};
use crate::policy::{PolicyEngine, ThermalState};
use crate::sensors::SensorReader;
use crate::status::{write_status, Health, RuntimeStatus};

pub struct Controller<R: SensorReader, A: Actuator> {
    cfg: Config,
    sensor: R,
    actuator: A,
    status_path: PathBuf,
    control_path: PathBuf,
    policy: PolicyEngine,
    last_sensor_ok_at: Instant,
    last_applied: Option<AppliedControls>,
}

impl<R: SensorReader, A: Actuator> Controller<R, A> {
    pub fn new(cfg: Config, mut sensor: R, actuator: A) -> Result<Self> {
        let first = sensor.read().context("initial sensor read failed")?;
        let now = Instant::now();
        let policy = PolicyEngine::new(cfg.hysteresis.clone(), now, first.temp_cpu_c);

        Ok(Self {
            status_path: PathBuf::from(&cfg.daemon.status_path),
            control_path: PathBuf::from(&cfg.daemon.control_path),
            cfg,
            sensor,
            actuator,
            policy,
            last_sensor_ok_at: now,
            last_applied: None,
        })
    }

    pub fn tick(&mut self) -> Result<RuntimeStatus> {
        let now = Instant::now();
        let now_ms = now_unix_ms();

        let (mode, override_expires_ms, target_temp_c) =
            with_locked_control_state(&self.control_path, |control| {
                let changed = control.expire_override_if_needed(now_ms);
                let mode = control.effective_mode(now_ms);
                let override_expires_ms = control.override_expires_ms(now_ms);
                let target_temp_c = control.target_temp_c;
                Ok(((mode, override_expires_ms, target_temp_c), changed))
            })
            .context("failed loading control state")?;

        self.policy
            .set_thresholds(hysteresis_for_target(&self.cfg.hysteresis, target_temp_c));

        let telemetry = self.sensor.read();

        let status = match telemetry {
            Ok(t) => {
                self.last_sensor_ok_at = now;

                let mut state = self.policy.evaluate(t.temp_cpu_c, now).state;
                let mut health = Health::Ok;
                let mut msg = "steady".to_string();

                let panic_temp = t.temp_cpu_c >= self.cfg.safety.panic_temp_c;
                if panic_temp {
                    state = ThermalState::Critical;
                    health = Health::Panic;
                    msg = format!("panic temp {:.1}C reached", t.temp_cpu_c);
                } else if t.temp_cpu_c >= self.cfg.safety.critical_temp_c {
                    state = ThermalState::Critical;
                    msg = format!("critical temp {:.1}C reached", t.temp_cpu_c);
                }

                if mode != ControlMode::Paused || panic_temp {
                    let profile = if panic_temp {
                        self.cfg.state.critical.clone()
                    } else {
                        self.profile_for_mode(mode, state).clone()
                    };
                    let applied = self
                        .actuator
                        .apply_profile(&profile, self.cfg.safety.rollback_on_error)
                        .context("failed applying profile")?;
                    self.last_applied = Some(applied);
                } else {
                    msg = format!("paused: monitoring only at {:.1}C", t.temp_cpu_c);
                }

                let (no_turbo, max_freq_khz) = self
                    .last_applied
                    .map(|c| (Some(c.no_turbo), Some(c.max_freq_khz)))
                    .unwrap_or((None, None));

                RuntimeStatus::new(
                    mode.as_str(),
                    state,
                    Some(&t),
                    health,
                    no_turbo,
                    max_freq_khz,
                    override_expires_ms,
                    target_temp_c,
                    msg,
                )
            }
            Err(err) => {
                let stale_for = now.duration_since(self.last_sensor_ok_at).as_secs();
                let health = if stale_for >= self.cfg.safety.sensor_stale_sec {
                    Health::SensorStale
                } else {
                    Health::Error
                };

                let (no_turbo, max_freq_khz) = self
                    .last_applied
                    .map(|c| (Some(c.no_turbo), Some(c.max_freq_khz)))
                    .unwrap_or((None, None));

                RuntimeStatus::new(
                    mode.as_str(),
                    self.policy.state(),
                    None,
                    health,
                    no_turbo,
                    max_freq_khz,
                    override_expires_ms,
                    target_temp_c,
                    format!("sensor read failed: {err}"),
                )
            }
        };

        write_status(&self.status_path, &status).context("failed writing status")?;
        Ok(status)
    }

    pub fn run(&mut self, max_ticks: Option<u64>) -> Result<()> {
        let mut ticks = 0_u64;
        loop {
            self.tick()?;
            ticks += 1;
            if max_ticks.is_some() && ticks >= max_ticks.unwrap_or(0) {
                break;
            }
            thread::sleep(Duration::from_millis(self.cfg.daemon.interval_ms));
        }
        Ok(())
    }

    fn profile_for_mode(&self, mode: ControlMode, state: ThermalState) -> &StateProfile {
        match mode {
            ControlMode::Auto => profile_for_state(&self.cfg, state),
            ControlMode::Eco => match state {
                ThermalState::Cool => &self.cfg.state.warm,
                ThermalState::Warm => &self.cfg.state.hot,
                ThermalState::Hot | ThermalState::Critical => &self.cfg.state.critical,
            },
            ControlMode::Performance => match state {
                ThermalState::Cool | ThermalState::Warm => &self.cfg.state.cool,
                ThermalState::Hot => &self.cfg.state.warm,
                ThermalState::Critical => &self.cfg.state.critical,
            },
            ControlMode::Paused => profile_for_state(&self.cfg, state),
        }
    }
}

fn profile_for_state(cfg: &Config, state: ThermalState) -> &StateProfile {
    match state {
        ThermalState::Cool => &cfg.state.cool,
        ThermalState::Warm => &cfg.state.warm,
        ThermalState::Hot => &cfg.state.hot,
        ThermalState::Critical => &cfg.state.critical,
    }
}

pub fn target_temp_bounds(base: &HysteresisConfig) -> (f64, f64) {
    let min_target = base.warm_on_c + 0.5;
    let max_target = (base.critical_off_c - 0.5).min(base.critical_on_c - 0.1);
    if max_target <= min_target {
        (base.hot_on_c, base.hot_on_c)
    } else {
        (min_target, max_target)
    }
}

fn hysteresis_for_target(base: &HysteresisConfig, target_temp_c: Option<f64>) -> HysteresisConfig {
    let Some(target) = target_temp_c else {
        return base.clone();
    };

    let (min_target, max_target) = target_temp_bounds(base);
    let target = target.clamp(min_target, max_target);
    let delta = target - base.hot_on_c;
    let eps = 0.1;

    let mut shifted = HysteresisConfig {
        warm_on_c: base.warm_on_c + delta,
        warm_off_c: base.warm_off_c + delta,
        hot_on_c: target,
        hot_off_c: base.hot_off_c + delta,
        critical_on_c: base.critical_on_c,
        critical_off_c: base.critical_off_c,
        min_dwell_sec: base.min_dwell_sec,
    };

    shifted.warm_on_c = shifted.warm_on_c.min(shifted.hot_on_c - eps);
    shifted.warm_off_c = shifted.warm_off_c.min(shifted.warm_on_c - eps);
    shifted.hot_off_c = shifted
        .hot_off_c
        .max(shifted.warm_on_c + eps)
        .min(shifted.hot_on_c - eps);

    shifted
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use anyhow::{anyhow, Result};
    use tempfile::tempdir;

    use super::*;
    use crate::config::{
        Config, DaemonConfig, HysteresisConfig, SafetyConfig, StateConfig, StateProfile,
    };
    use crate::control::{persist_control_state, ControlState};
    use crate::sensors::Telemetry;

    struct FakeSensor {
        seq: VecDeque<Result<Telemetry>>,
    }

    impl FakeSensor {
        fn new(seq: Vec<Result<Telemetry>>) -> Self {
            Self { seq: seq.into() }
        }
    }

    impl SensorReader for FakeSensor {
        fn read(&mut self) -> Result<Telemetry> {
            self.seq
                .pop_front()
                .unwrap_or_else(|| Err(anyhow!("no sample")))
        }
    }

    #[derive(Default)]
    struct FakeActuator {
        calls: usize,
    }

    impl Actuator for FakeActuator {
        fn apply_profile(
            &mut self,
            profile: &StateProfile,
            _rollback_on_error: bool,
        ) -> Result<AppliedControls> {
            self.calls += 1;
            Ok(AppliedControls {
                no_turbo: !profile.turbo,
                max_freq_khz: 2_400_000,
            })
        }
    }

    fn config(status_path: String, control_path: String) -> Config {
        Config {
            daemon: DaemonConfig {
                interval_ms: 10,
                status_path,
                api_bind: "127.0.0.1:8788".to_string(),
                control_path,
                service_unit: "nuc-powerd.service".to_string(),
                stress_program: "stress-ng".to_string(),
            },
            safety: SafetyConfig {
                critical_temp_c: 90.0,
                panic_temp_c: 95.0,
                sensor_stale_sec: 1,
                rollback_on_error: true,
            },
            hysteresis: HysteresisConfig {
                warm_on_c: 72.0,
                warm_off_c: 68.0,
                hot_on_c: 80.0,
                hot_off_c: 75.0,
                critical_on_c: 88.0,
                critical_off_c: 83.0,
                min_dwell_sec: 0,
            },
            state: StateConfig {
                cool: StateProfile {
                    epp: "balance_performance".to_string(),
                    turbo: true,
                    max_freq_pct: 100,
                    rapl_pkg_w: Some(28),
                },
                warm: StateProfile {
                    epp: "balance_power".to_string(),
                    turbo: true,
                    max_freq_pct: 90,
                    rapl_pkg_w: Some(24),
                },
                hot: StateProfile {
                    epp: "power".to_string(),
                    turbo: false,
                    max_freq_pct: 70,
                    rapl_pkg_w: Some(17),
                },
                critical: StateProfile {
                    epp: "power".to_string(),
                    turbo: false,
                    max_freq_pct: 55,
                    rapl_pkg_w: Some(12),
                },
            },
        }
    }

    fn sample(temp: f64) -> Telemetry {
        Telemetry {
            temp_cpu_c: temp,
            cpu_util_pct: 30.0,
            freq_mhz: Some(2200.0),
            pkg_power_w: Some(12.0),
            timestamp_ms: 0,
        }
    }

    #[test]
    fn controller_forces_critical_on_panic_temp() {
        let dir = tempdir().expect("tempdir");
        let status_path = dir.path().join("status.json");
        let control_path = dir.path().join("control.json");

        let sensor = FakeSensor::new(vec![Ok(sample(70.0)), Ok(sample(96.0))]);
        let actuator = FakeActuator::default();
        let mut ctrl = Controller::new(
            config(
                status_path.display().to_string(),
                control_path.display().to_string(),
            ),
            sensor,
            actuator,
        )
        .expect("new controller");

        let status = ctrl.tick().expect("tick");
        assert_eq!(status.state, ThermalState::Critical);
        assert!(matches!(status.health, Health::Panic));
    }

    #[test]
    fn controller_reports_sensor_stale_after_threshold() {
        let dir = tempdir().expect("tempdir");
        let status_path = dir.path().join("status.json");
        let control_path = dir.path().join("control.json");
        let sensor = FakeSensor::new(vec![Ok(sample(70.0)), Err(anyhow!("boom"))]);
        let actuator = FakeActuator::default();

        let mut cfg = config(
            status_path.display().to_string(),
            control_path.display().to_string(),
        );
        cfg.safety.sensor_stale_sec = 0;

        let mut ctrl = Controller::new(cfg, sensor, actuator).expect("new controller");
        let status = ctrl.tick().expect("tick");
        assert!(matches!(status.health, Health::SensorStale));
    }

    #[test]
    fn paused_mode_stops_writes_outside_panic() {
        let dir = tempdir().expect("tempdir");
        let status_path = dir.path().join("status.json");
        let control_path = dir.path().join("control.json");

        let sensor = FakeSensor::new(vec![Ok(sample(70.0)), Ok(sample(89.0))]);
        let actuator = FakeActuator::default();

        let mut state = ControlState::default();
        state.set_mode(ControlMode::Paused);
        persist_control_state(&control_path, &state).expect("persist control");

        let mut ctrl = Controller::new(
            config(
                status_path.display().to_string(),
                control_path.display().to_string(),
            ),
            sensor,
            actuator,
        )
        .expect("new controller");

        let status = ctrl.tick().expect("tick");
        assert_eq!(status.mode, "paused");
        assert!(status.message.contains("paused"));
        assert_eq!(ctrl.last_applied, None);
    }

    #[test]
    fn target_thresholds_stay_ordered_after_large_shift() {
        let base = HysteresisConfig {
            warm_on_c: 72.0,
            warm_off_c: 68.0,
            hot_on_c: 80.0,
            hot_off_c: 75.0,
            critical_on_c: 88.0,
            critical_off_c: 83.0,
            min_dwell_sec: 0,
        };

        let shifted = hysteresis_for_target(&base, Some(40.0));
        assert!(shifted.warm_off_c < shifted.warm_on_c);
        assert!(shifted.warm_on_c < shifted.hot_off_c);
        assert!(shifted.hot_off_c < shifted.hot_on_c);
        assert!(shifted.hot_on_c < shifted.critical_off_c);
        assert!(shifted.critical_off_c < shifted.critical_on_c);
    }
}
