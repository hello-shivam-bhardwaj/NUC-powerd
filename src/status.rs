use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::control::now_unix_ms;
use crate::io::atomic_write;
use crate::policy::ThermalState;
use crate::sensors::Telemetry;

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Ok,
    SensorStale,
    Panic,
    Error,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct RuntimeStatus {
    pub mode: String,
    pub state: ThermalState,
    pub health: Health,
    pub temp_cpu_c: Option<f64>,
    pub cpu_util_pct: Option<f64>,
    pub pkg_power_w: Option<f64>,
    pub freq_mhz: Option<f64>,
    pub no_turbo: Option<bool>,
    pub max_freq_khz: Option<u64>,
    pub override_expires_ms: Option<u128>,
    pub target_temp_c: Option<f64>,
    pub last_update_ms: u128,
    pub message: String,
}

impl RuntimeStatus {
    pub fn bootstrap(mode: &str, state: ThermalState) -> Self {
        Self {
            mode: mode.to_string(),
            state,
            health: Health::Ok,
            temp_cpu_c: None,
            cpu_util_pct: None,
            pkg_power_w: None,
            freq_mhz: None,
            no_turbo: None,
            max_freq_khz: None,
            override_expires_ms: None,
            target_temp_c: None,
            last_update_ms: now_unix_ms(),
            message: "starting".to_string(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mode: &str,
        state: ThermalState,
        telemetry: Option<&Telemetry>,
        health: Health,
        no_turbo: Option<bool>,
        max_freq_khz: Option<u64>,
        override_expires_ms: Option<u128>,
        target_temp_c: Option<f64>,
        message: String,
    ) -> Self {
        Self {
            mode: mode.to_string(),
            state,
            health,
            temp_cpu_c: telemetry.map(|t| t.temp_cpu_c),
            cpu_util_pct: telemetry.map(|t| t.cpu_util_pct),
            pkg_power_w: telemetry.and_then(|t| t.pkg_power_w),
            freq_mhz: telemetry.and_then(|t| t.freq_mhz),
            no_turbo,
            max_freq_khz,
            override_expires_ms,
            target_temp_c,
            last_update_ms: now_unix_ms(),
            message,
        }
    }
}

pub fn write_status(path: &Path, status: &RuntimeStatus) -> Result<()> {
    let raw = serde_json::to_string_pretty(status)?;
    atomic_write(path, &raw)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_status_json() {
        let dir = tempdir().expect("tempdir");
        let status_path = dir.path().join("run/status.json");

        let telemetry = Telemetry {
            temp_cpu_c: 73.2,
            cpu_util_pct: 52.0,
            freq_mhz: Some(2800.0),
            pkg_power_w: Some(16.5),
            timestamp_ms: 123,
        };
        let status = RuntimeStatus::new(
            "auto",
            ThermalState::Warm,
            Some(&telemetry),
            Health::Ok,
            Some(false),
            Some(2_400_000),
            Some(42),
            Some(74.0),
            "steady".to_string(),
        );

        write_status(&status_path, &status).expect("write status");
        let raw = fs::read_to_string(status_path).expect("read status");
        assert!(raw.contains("\"state\": \"warm\""));
        assert!(raw.contains("\"health\": \"ok\""));
        assert!(raw.contains("\"max_freq_khz\": 2400000"));
    }
}
