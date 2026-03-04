use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::policy::ThermalState;
use crate::sensors::Telemetry;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Ok,
    SensorStale,
    Panic,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatus {
    pub mode: String,
    pub state: ThermalState,
    pub telemetry: Option<Telemetry>,
    pub health: Health,
    pub last_update_ms: u128,
    pub message: String,
}

impl RuntimeStatus {
    pub fn new(
        mode: &str,
        state: ThermalState,
        telemetry: Option<Telemetry>,
        health: Health,
        message: String,
    ) -> Self {
        let last_update_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        Self {
            mode: mode.to_string(),
            state,
            telemetry,
            health,
            last_update_ms,
            message,
        }
    }
}

pub fn write_status(path: &Path, status: &RuntimeStatus) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed creating status dir {}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(status)?;
    fs::write(path, raw).with_context(|| format!("failed writing status {}", path.display()))?;
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

        let status = RuntimeStatus::new(
            "auto",
            ThermalState::Warm,
            Some(Telemetry {
                temp_cpu_c: 73.2,
                cpu_util_pct: 52.0,
                freq_mhz: Some(2800.0),
                pkg_power_w: Some(16.5),
                timestamp_ms: 123,
            }),
            Health::Ok,
            "steady".to_string(),
        );

        write_status(&status_path, &status).expect("write status");
        let raw = fs::read_to_string(status_path).expect("read status");
        assert!(raw.contains("\"state\": \"warm\""));
        assert!(raw.contains("\"health\": \"ok\""));
    }
}
