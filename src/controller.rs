use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::actuators::Actuator;
use crate::config::Config;
use crate::policy::{PolicyEngine, ThermalState};
use crate::sensors::SensorReader;
use crate::status::{write_status, Health, RuntimeStatus};

pub struct Controller<R: SensorReader, A: Actuator> {
    cfg: Config,
    sensor: R,
    actuator: A,
    status_path: PathBuf,
    policy: PolicyEngine,
    mode: String,
    last_sensor_ok_at: Instant,
}

impl<R: SensorReader, A: Actuator> Controller<R, A> {
    pub fn new(cfg: Config, mut sensor: R, actuator: A, mode: &str) -> Result<Self> {
        let first = sensor.read().context("initial sensor read failed")?;
        let now = Instant::now();
        let policy = PolicyEngine::new(cfg.hysteresis.clone(), now, first.temp_cpu_c);

        Ok(Self {
            status_path: PathBuf::from(&cfg.daemon.status_path),
            cfg,
            sensor,
            actuator,
            policy,
            mode: mode.to_string(),
            last_sensor_ok_at: now,
        })
    }

    pub fn tick(&mut self) -> Result<RuntimeStatus> {
        let now = Instant::now();
        let telemetry = self.sensor.read();

        let status = match telemetry {
            Ok(t) => {
                self.last_sensor_ok_at = now;

                let mut state = self.policy.evaluate(t.temp_cpu_c, now).state;
                let mut health = Health::Ok;
                let mut msg = "steady".to_string();

                if t.temp_cpu_c >= self.cfg.safety.panic_temp_c {
                    state = ThermalState::Critical;
                    health = Health::Panic;
                    msg = format!("panic temp {:.1}C reached", t.temp_cpu_c);
                } else if t.temp_cpu_c >= self.cfg.safety.critical_temp_c {
                    state = ThermalState::Critical;
                    msg = format!("critical temp {:.1}C reached", t.temp_cpu_c);
                }

                let profile = match state {
                    ThermalState::Cool => &self.cfg.state.cool,
                    ThermalState::Warm => &self.cfg.state.warm,
                    ThermalState::Hot => &self.cfg.state.hot,
                    ThermalState::Critical => &self.cfg.state.critical,
                };

                self.actuator
                    .apply_profile(profile, self.cfg.safety.rollback_on_error)
                    .context("failed applying profile")?;

                RuntimeStatus::new(&self.mode, state, Some(t), health, msg)
            }
            Err(err) => {
                let stale_for = now.duration_since(self.last_sensor_ok_at).as_secs();
                let health = if stale_for >= self.cfg.safety.sensor_stale_sec {
                    Health::SensorStale
                } else {
                    Health::Error
                };
                RuntimeStatus::new(
                    &self.mode,
                    self.policy.state(),
                    None,
                    health,
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
            _profile: &StateProfile,
            _rollback_on_error: bool,
        ) -> Result<()> {
            self.calls += 1;
            Ok(())
        }
    }

    fn config(status_path: String) -> Config {
        Config {
            daemon: DaemonConfig {
                interval_ms: 10,
                status_path,
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

        let sensor = FakeSensor::new(vec![Ok(sample(70.0)), Ok(sample(96.0))]);
        let actuator = FakeActuator::default();
        let mut ctrl = Controller::new(
            config(status_path.display().to_string()),
            sensor,
            actuator,
            "auto",
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
        let sensor = FakeSensor::new(vec![Ok(sample(70.0)), Err(anyhow!("boom"))]);
        let actuator = FakeActuator::default();

        let mut cfg = config(status_path.display().to_string());
        cfg.safety.sensor_stale_sec = 0;

        let mut ctrl = Controller::new(cfg, sensor, actuator, "auto").expect("new controller");
        let status = ctrl.tick().expect("tick");
        assert!(matches!(status.health, Health::SensorStale));
    }
}
