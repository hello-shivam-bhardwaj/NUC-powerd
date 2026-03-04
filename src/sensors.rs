use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Telemetry {
    pub temp_cpu_c: f64,
    pub cpu_util_pct: f64,
    pub freq_mhz: Option<f64>,
    pub pkg_power_w: Option<f64>,
    pub timestamp_ms: u128,
}

pub trait SensorReader {
    fn read(&mut self) -> Result<Telemetry>;
}

#[derive(Debug, Clone)]
struct CpuStat {
    total: u64,
    idle: u64,
}

pub struct LinuxSensors {
    proc_stat_path: PathBuf,
    cpufreq_cur_path: PathBuf,
    rapl_energy_path: PathBuf,
    thermal_root: PathBuf,
    prev_cpu: Option<CpuStat>,
    prev_energy_uj: Option<u64>,
    prev_energy_ts: Option<Instant>,
}

impl LinuxSensors {
    pub fn new() -> Self {
        Self {
            proc_stat_path: PathBuf::from("/proc/stat"),
            cpufreq_cur_path: PathBuf::from(
                "/sys/devices/system/cpu/cpufreq/policy0/scaling_cur_freq",
            ),
            rapl_energy_path: PathBuf::from(
                "/sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj",
            ),
            thermal_root: PathBuf::from("/sys/class/thermal"),
            prev_cpu: None,
            prev_energy_uj: None,
            prev_energy_ts: None,
        }
    }

    #[cfg(test)]
    pub fn with_paths(
        proc_stat_path: PathBuf,
        cpufreq_cur_path: PathBuf,
        rapl_energy_path: PathBuf,
        thermal_root: PathBuf,
    ) -> Self {
        Self {
            proc_stat_path,
            cpufreq_cur_path,
            rapl_energy_path,
            thermal_root,
            prev_cpu: None,
            prev_energy_uj: None,
            prev_energy_ts: None,
        }
    }

    fn read_temp_c(&self) -> Result<f64> {
        let mut best = None::<f64>;
        let entries = fs::read_dir(&self.thermal_root).with_context(|| {
            format!(
                "failed to read thermal root {}",
                self.thermal_root.display()
            )
        })?;

        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with("thermal_zone") {
                continue;
            }
            let temp_path = entry.path().join("temp");
            if let Ok(raw) = fs::read_to_string(&temp_path) {
                if let Ok(mut v) = raw.trim().parse::<f64>() {
                    if v > 1000.0 {
                        v /= 1000.0;
                    }
                    if v > 0.0 {
                        best = Some(best.map_or(v, |curr| curr.max(v)));
                    }
                }
            }
        }

        best.ok_or_else(|| anyhow!("no valid thermal zone temperature found"))
    }

    fn read_cpu_stat(path: &Path) -> Result<CpuStat> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let line = raw
            .lines()
            .find(|l| l.starts_with("cpu "))
            .ok_or_else(|| anyhow!("missing aggregate cpu line in /proc/stat"))?;
        let mut nums = line
            .split_whitespace()
            .skip(1)
            .filter_map(|s| s.parse::<u64>().ok());

        let user = nums.next().unwrap_or(0);
        let nice = nums.next().unwrap_or(0);
        let system = nums.next().unwrap_or(0);
        let idle = nums.next().unwrap_or(0);
        let iowait = nums.next().unwrap_or(0);
        let irq = nums.next().unwrap_or(0);
        let softirq = nums.next().unwrap_or(0);
        let steal = nums.next().unwrap_or(0);

        let total = user + nice + system + idle + iowait + irq + softirq + steal;
        Ok(CpuStat {
            total,
            idle: idle + iowait,
        })
    }

    fn read_freq_mhz(&self) -> Option<f64> {
        let raw = fs::read_to_string(&self.cpufreq_cur_path).ok()?;
        let khz = raw.trim().parse::<f64>().ok()?;
        Some(khz / 1000.0)
    }

    fn read_pkg_power_w(&mut self) -> Option<f64> {
        let raw = fs::read_to_string(&self.rapl_energy_path).ok()?;
        let energy_uj = raw.trim().parse::<u64>().ok()?;
        let now = Instant::now();

        let power =
            if let (Some(prev_uj), Some(prev_ts)) = (self.prev_energy_uj, self.prev_energy_ts) {
                let dt = now.duration_since(prev_ts).as_secs_f64();
                if dt > 0.0 && energy_uj >= prev_uj {
                    let joules = (energy_uj - prev_uj) as f64 / 1_000_000.0;
                    Some(joules / dt)
                } else {
                    None
                }
            } else {
                None
            };

        self.prev_energy_uj = Some(energy_uj);
        self.prev_energy_ts = Some(now);
        power
    }
}

impl Default for LinuxSensors {
    fn default() -> Self {
        Self::new()
    }
}

impl SensorReader for LinuxSensors {
    fn read(&mut self) -> Result<Telemetry> {
        let temp_cpu_c = self.read_temp_c()?;

        let now_cpu = Self::read_cpu_stat(&self.proc_stat_path)?;
        let cpu_util_pct = if let Some(prev) = &self.prev_cpu {
            let total_delta = now_cpu.total.saturating_sub(prev.total);
            let idle_delta = now_cpu.idle.saturating_sub(prev.idle);
            if total_delta == 0 {
                0.0
            } else {
                (1.0 - (idle_delta as f64 / total_delta as f64)) * 100.0
            }
        } else {
            0.0
        };
        self.prev_cpu = Some(now_cpu);

        Ok(Telemetry {
            temp_cpu_c,
            cpu_util_pct,
            freq_mhz: self.read_freq_mhz(),
            pkg_power_w: self.read_pkg_power_w(),
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::thread;
    use std::time::Duration;

    use super::*;
    use tempfile::tempdir;

    #[test]
    fn reads_basic_telemetry_from_fake_fs() {
        let dir = tempdir().expect("tempdir");
        let thermal_root = dir.path().join("thermal");
        let zone0 = thermal_root.join("thermal_zone0");
        fs::create_dir_all(&zone0).expect("mk zone");
        fs::write(zone0.join("temp"), "65000\n").expect("write temp");

        let proc_stat = dir.path().join("proc_stat");
        fs::write(&proc_stat, "cpu  100 0 100 1000 0 0 0 0 0 0\n").expect("write proc stat");

        let freq = dir.path().join("freq");
        fs::write(&freq, "2400000\n").expect("write freq");

        let rapl = dir.path().join("energy_uj");
        fs::write(&rapl, "1000000\n").expect("write rapl");

        let mut sensors =
            LinuxSensors::with_paths(proc_stat.clone(), freq, rapl.clone(), thermal_root);
        let first = sensors.read().expect("first read");
        assert_eq!(first.temp_cpu_c, 65.0);
        assert_eq!(first.cpu_util_pct, 0.0);

        thread::sleep(Duration::from_millis(20));
        fs::write(&proc_stat, "cpu  200 0 200 1100 0 0 0 0 0 0\n").expect("write proc stat2");
        fs::write(&rapl, "1200000\n").expect("write rapl2");
        let second = sensors.read().expect("second read");
        assert!(second.cpu_util_pct > 50.0);
        assert!(second.pkg_power_w.is_some());
    }
}
