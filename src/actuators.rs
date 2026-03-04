use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::StateProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedControls {
    pub no_turbo: bool,
    pub max_freq_khz: u64,
}

pub trait Actuator {
    fn apply_profile(
        &mut self,
        profile: &StateProfile,
        rollback_on_error: bool,
    ) -> Result<AppliedControls>;
}

pub struct SysfsActuator {
    epp_path: PathBuf,
    max_freq_path: PathBuf,
    max_freq_cap_path: PathBuf,
    no_turbo_path: PathBuf,
    rapl_pkg_limit_path: PathBuf,
    dry_run: bool,
}

impl SysfsActuator {
    pub fn new(dry_run: bool) -> Self {
        Self {
            epp_path: PathBuf::from(
                "/sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference",
            ),
            max_freq_path: PathBuf::from(
                "/sys/devices/system/cpu/cpufreq/policy0/scaling_max_freq",
            ),
            max_freq_cap_path: PathBuf::from(
                "/sys/devices/system/cpu/cpufreq/policy0/cpuinfo_max_freq",
            ),
            no_turbo_path: PathBuf::from("/sys/devices/system/cpu/intel_pstate/no_turbo"),
            rapl_pkg_limit_path: PathBuf::from(
                "/sys/class/powercap/intel-rapl/intel-rapl:0/constraint_0_power_limit_uw",
            ),
            dry_run,
        }
    }

    pub fn with_paths(
        epp_path: PathBuf,
        max_freq_path: PathBuf,
        max_freq_cap_path: PathBuf,
        no_turbo_path: PathBuf,
        rapl_pkg_limit_path: PathBuf,
        dry_run: bool,
    ) -> Self {
        Self {
            epp_path,
            max_freq_path,
            max_freq_cap_path,
            no_turbo_path,
            rapl_pkg_limit_path,
            dry_run,
        }
    }
}

impl Actuator for SysfsActuator {
    fn apply_profile(
        &mut self,
        profile: &StateProfile,
        rollback_on_error: bool,
    ) -> Result<AppliedControls> {
        let mut rollback: Vec<(PathBuf, String)> = Vec::new();

        let max_cap_raw = fs::read_to_string(&self.max_freq_cap_path)
            .with_context(|| format!("failed reading {}", self.max_freq_cap_path.display()))?;
        let max_cap_khz: u64 = max_cap_raw
            .trim()
            .parse()
            .context("invalid cpuinfo_max_freq")?;
        let requested_max_khz = (max_cap_khz * u64::from(profile.max_freq_pct)) / 100;

        let guarded_write = |path: &PathBuf,
                             value: String,
                             rollback: &mut Vec<(PathBuf, String)>,
                             dry_run: bool,
                             rollback_on_error: bool|
         -> Result<()> {
            if dry_run {
                return Ok(());
            }
            if !path.exists() {
                return Ok(());
            }
            if rollback_on_error {
                match fs::read_to_string(path) {
                    Ok(old) => rollback.push((path.clone(), old)),
                    Err(err) => {
                        eprintln!(
                            "warning: failed reading rollback snapshot {}: {err}",
                            path.display()
                        );
                    }
                }
            }
            fs::write(path, value).with_context(|| format!("failed writing {}", path.display()))?;
            Ok(())
        };

        let result = (|| -> Result<()> {
            guarded_write(
                &self.epp_path,
                format!("{}\n", profile.epp),
                &mut rollback,
                self.dry_run,
                rollback_on_error,
            )?;

            guarded_write(
                &self.max_freq_path,
                format!("{}\n", requested_max_khz),
                &mut rollback,
                self.dry_run,
                rollback_on_error,
            )?;

            let no_turbo = if profile.turbo { "0\n" } else { "1\n" };
            guarded_write(
                &self.no_turbo_path,
                no_turbo.to_string(),
                &mut rollback,
                self.dry_run,
                rollback_on_error,
            )?;

            if let Some(watts) = profile.rapl_pkg_w {
                let microwatts = watts as u64 * 1_000_000;
                guarded_write(
                    &self.rapl_pkg_limit_path,
                    format!("{}\n", microwatts),
                    &mut rollback,
                    self.dry_run,
                    rollback_on_error,
                )?;
            }

            Ok(())
        })();

        if result.is_err() && rollback_on_error && !self.dry_run {
            for (path, value) in rollback.into_iter().rev() {
                if let Err(err) = fs::write(&path, value) {
                    eprintln!(
                        "warning: rollback write failed for {}: {err}",
                        path.display()
                    );
                }
            }
        }

        result?;
        Ok(AppliedControls {
            no_turbo: !profile.turbo,
            max_freq_khz: requested_max_khz,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::tempdir;

    #[test]
    fn applies_profile_to_fake_sysfs() {
        let dir = tempdir().expect("tempdir");
        let epp = dir.path().join("epp");
        let max_freq = dir.path().join("max_freq");
        let max_cap = dir.path().join("max_cap");
        let no_turbo = dir.path().join("no_turbo");
        let rapl = dir.path().join("rapl_limit");

        fs::write(&epp, "balance_performance\n").expect("seed epp");
        fs::write(&max_freq, "3000000\n").expect("seed max freq");
        fs::write(&max_cap, "4000000\n").expect("seed cap");
        fs::write(&no_turbo, "0\n").expect("seed turbo");
        fs::write(&rapl, "28000000\n").expect("seed rapl");

        let mut act =
            SysfsActuator::with_paths(epp, max_freq.clone(), max_cap, no_turbo, rapl, false);

        let profile = StateProfile {
            epp: "power".to_string(),
            turbo: false,
            max_freq_pct: 50,
            rapl_pkg_w: Some(15),
        };

        let applied = act.apply_profile(&profile, true).expect("apply");

        assert_eq!(
            fs::read_to_string(&max_freq).expect("read max freq").trim(),
            "2000000"
        );
        assert!(applied.no_turbo);
        assert_eq!(applied.max_freq_khz, 2_000_000);
    }

    #[test]
    fn dry_run_does_not_modify_files() {
        let dir = tempdir().expect("tempdir");
        let epp = dir.path().join("epp");
        let max_freq = dir.path().join("max_freq");
        let max_cap = dir.path().join("max_cap");
        let no_turbo = dir.path().join("no_turbo");
        let rapl = dir.path().join("rapl_limit");

        fs::write(&epp, "balance_performance\n").expect("seed epp");
        fs::write(&max_freq, "3000000\n").expect("seed max freq");
        fs::write(&max_cap, "4000000\n").expect("seed cap");
        fs::write(&no_turbo, "0\n").expect("seed turbo");
        fs::write(&rapl, "28000000\n").expect("seed rapl");

        let mut act =
            SysfsActuator::with_paths(epp.clone(), max_freq, max_cap, no_turbo, rapl, true);
        let profile = StateProfile {
            epp: "power".to_string(),
            turbo: false,
            max_freq_pct: 40,
            rapl_pkg_w: Some(12),
        };

        let applied = act.apply_profile(&profile, true).expect("apply");
        assert_eq!(
            fs::read_to_string(&epp).expect("read epp").trim(),
            "balance_performance"
        );
        assert!(applied.no_turbo);
        assert_eq!(applied.max_freq_khz, 1_600_000);
    }
}
