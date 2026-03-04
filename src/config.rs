use std::fs;
use std::path::Path;

use anyhow::{anyhow, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub safety: SafetyConfig,
    pub hysteresis: HysteresisConfig,
    pub state: StateConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    pub interval_ms: u64,
    pub status_path: String,
    #[serde(default = "default_api_bind")]
    pub api_bind: String,
    #[serde(default = "default_control_path")]
    pub control_path: String,
    #[serde(default = "default_service_unit")]
    pub service_unit: String,
    #[serde(default = "default_stress_program")]
    pub stress_program: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SafetyConfig {
    pub critical_temp_c: f64,
    pub panic_temp_c: f64,
    pub sensor_stale_sec: u64,
    pub rollback_on_error: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HysteresisConfig {
    pub warm_on_c: f64,
    pub warm_off_c: f64,
    pub hot_on_c: f64,
    pub hot_off_c: f64,
    pub critical_on_c: f64,
    pub critical_off_c: f64,
    pub min_dwell_sec: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StateConfig {
    pub cool: StateProfile,
    pub warm: StateProfile,
    pub hot: StateProfile,
    pub critical: StateProfile,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct StateProfile {
    pub epp: String,
    pub turbo: bool,
    pub max_freq_pct: u8,
    pub rapl_pkg_w: Option<u32>,
}

pub fn load_config(path: &Path) -> Result<Config> {
    let raw = fs::read_to_string(path)?;
    let cfg: Config = toml::from_str(&raw)?;
    validate_config(&cfg)?;
    Ok(cfg)
}

pub fn validate_config(cfg: &Config) -> Result<()> {
    if cfg.daemon.interval_ms == 0 {
        return Err(anyhow!("daemon.interval_ms must be > 0"));
    }
    if cfg.daemon.api_bind.trim().is_empty() {
        return Err(anyhow!("daemon.api_bind must not be empty"));
    }
    if cfg.daemon.control_path.trim().is_empty() {
        return Err(anyhow!("daemon.control_path must not be empty"));
    }
    if cfg.daemon.service_unit.trim().is_empty() {
        return Err(anyhow!("daemon.service_unit must not be empty"));
    }
    if cfg.daemon.stress_program.trim().is_empty() {
        return Err(anyhow!("daemon.stress_program must not be empty"));
    }
    if cfg.safety.panic_temp_c <= cfg.safety.critical_temp_c {
        return Err(anyhow!(
            "safety.panic_temp_c must be > safety.critical_temp_c"
        ));
    }
    if !(cfg.hysteresis.warm_off_c < cfg.hysteresis.warm_on_c
        && cfg.hysteresis.warm_on_c < cfg.hysteresis.hot_on_c
        && cfg.hysteresis.hot_off_c < cfg.hysteresis.hot_on_c
        && cfg.hysteresis.hot_on_c < cfg.hysteresis.critical_on_c
        && cfg.hysteresis.critical_off_c < cfg.hysteresis.critical_on_c)
    {
        return Err(anyhow!("hysteresis thresholds are inconsistent"));
    }

    for profile in [
        &cfg.state.cool,
        &cfg.state.warm,
        &cfg.state.hot,
        &cfg.state.critical,
    ] {
        if profile.max_freq_pct == 0 || profile.max_freq_pct > 100 {
            return Err(anyhow!("state max_freq_pct must be in 1..=100"));
        }
        if !matches!(
            profile.epp.as_str(),
            "power" | "balance_power" | "balance_performance" | "performance"
        ) {
            return Err(anyhow!("state epp has unsupported value"));
        }
    }

    Ok(())
}

fn default_api_bind() -> String {
    "127.0.0.1:8788".to_string()
}

fn default_control_path() -> String {
    "/run/nuc-powerd/control.json".to_string()
}

fn default_service_unit() -> String {
    "nuc-powerd.service".to_string()
}

fn default_stress_program() -> String {
    "stress-ng".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_and_validates_example_config() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[daemon]
interval_ms = 1000
status_path = "/run/nuc-powerd/status.json"

[safety]
critical_temp_c = 90.0
panic_temp_c = 95.0
sensor_stale_sec = 5
rollback_on_error = true

[hysteresis]
warm_on_c = 72.0
warm_off_c = 68.0
hot_on_c = 80.0
hot_off_c = 75.0
critical_on_c = 88.0
critical_off_c = 83.0
min_dwell_sec = 20

[state.cool]
epp = "balance_performance"
turbo = true
max_freq_pct = 100
rapl_pkg_w = 28

[state.warm]
epp = "balance_power"
turbo = true
max_freq_pct = 90
rapl_pkg_w = 24

[state.hot]
epp = "power"
turbo = false
max_freq_pct = 70
rapl_pkg_w = 17

[state.critical]
epp = "power"
turbo = false
max_freq_pct = 55
rapl_pkg_w = 12
"#,
        )
        .expect("write config");

        let cfg = load_config(&path).expect("load config");
        assert_eq!(cfg.state.hot.max_freq_pct, 70);
        assert_eq!(cfg.state.cool.epp, "balance_performance");
        assert_eq!(cfg.daemon.api_bind, "127.0.0.1:8788");
        assert_eq!(cfg.daemon.control_path, "/run/nuc-powerd/control.json");
        assert_eq!(cfg.daemon.service_unit, "nuc-powerd.service");
        assert_eq!(cfg.daemon.stress_program, "stress-ng");
    }

    #[test]
    fn rejects_bad_threshold_order() {
        let cfg = Config {
            daemon: DaemonConfig {
                interval_ms: 1000,
                status_path: "/tmp/status.json".to_string(),
                api_bind: "127.0.0.1:8788".to_string(),
                control_path: "/run/nuc-powerd/control.json".to_string(),
                service_unit: "nuc-powerd.service".to_string(),
                stress_program: "stress-ng".to_string(),
            },
            safety: SafetyConfig {
                critical_temp_c: 90.0,
                panic_temp_c: 95.0,
                sensor_stale_sec: 5,
                rollback_on_error: true,
            },
            hysteresis: HysteresisConfig {
                warm_on_c: 70.0,
                warm_off_c: 71.0,
                hot_on_c: 80.0,
                hot_off_c: 75.0,
                critical_on_c: 88.0,
                critical_off_c: 83.0,
                min_dwell_sec: 20,
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
        };

        assert!(validate_config(&cfg).is_err());
    }
}
