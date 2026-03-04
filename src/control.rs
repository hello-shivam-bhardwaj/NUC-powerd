use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::io::atomic_write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlMode {
    Auto,
    Eco,
    Performance,
    Paused,
}

impl ControlMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Eco => "eco",
            Self::Performance => "performance",
            Self::Paused => "paused",
        }
    }
}

impl FromStr for ControlMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "eco" => Ok(Self::Eco),
            "performance" => Ok(Self::Performance),
            "paused" => Ok(Self::Paused),
            _ => Err(anyhow!("invalid mode '{value}'")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeOverride {
    pub mode: ControlMode,
    pub expires_at_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlState {
    #[serde(default = "default_mode")]
    pub mode: ControlMode,
    #[serde(default)]
    pub override_mode: Option<ModeOverride>,
    #[serde(default)]
    pub target_temp_c: Option<f64>,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            mode: ControlMode::Auto,
            override_mode: None,
            target_temp_c: None,
        }
    }
}

fn default_mode() -> ControlMode {
    ControlMode::Auto
}

impl ControlState {
    pub fn effective_mode(&self, now_ms: u128) -> ControlMode {
        match &self.override_mode {
            Some(ovr) if now_ms < ovr.expires_at_ms => ovr.mode,
            _ => self.mode,
        }
    }

    pub fn override_expires_ms(&self, now_ms: u128) -> Option<u128> {
        match &self.override_mode {
            Some(ovr) if now_ms < ovr.expires_at_ms => Some(ovr.expires_at_ms),
            _ => None,
        }
    }

    pub fn set_mode(&mut self, mode: ControlMode) {
        self.mode = mode;
    }

    pub fn set_override(&mut self, mode: ControlMode, ttl_sec: u64, now_ms: u128) -> Result<()> {
        if ttl_sec == 0 {
            return Err(anyhow!("ttl_sec must be > 0"));
        }

        let ttl_ms = u128::from(ttl_sec) * 1000;
        let expires_at_ms = now_ms
            .checked_add(ttl_ms)
            .ok_or_else(|| anyhow!("ttl_sec is too large"))?;

        self.override_mode = Some(ModeOverride {
            mode,
            expires_at_ms,
        });
        Ok(())
    }

    pub fn set_target_temp(&mut self, target_temp_c: f64) -> Result<()> {
        if !target_temp_c.is_finite() {
            return Err(anyhow!("target_temp_c must be finite"));
        }
        self.target_temp_c = Some(target_temp_c);
        Ok(())
    }

    pub fn expire_override_if_needed(&mut self, now_ms: u128) -> bool {
        let should_expire = self
            .override_mode
            .as_ref()
            .map(|ovr| now_ms >= ovr.expires_at_ms)
            .unwrap_or(false);

        if should_expire {
            self.override_mode = None;
            self.mode = ControlMode::Auto;
            return true;
        }

        false
    }
}

pub fn load_control_state(path: &Path) -> Result<ControlState> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            if raw.trim().is_empty() {
                return Ok(ControlState::default());
            }
            let mut state: ControlState = serde_json::from_str(&raw)
                .with_context(|| format!("failed parsing {}", path.display()))?;
            if let Some(target) = state.target_temp_c {
                state.set_target_temp(target)?;
            }
            Ok(state)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(ControlState::default()),
        Err(err) => Err(err).with_context(|| format!("failed reading {}", path.display())),
    }
}

pub fn persist_control_state(path: &Path, state: &ControlState) -> Result<()> {
    let raw = serde_json::to_string_pretty(state)?;
    atomic_write(path, &raw)?;
    Ok(())
}

pub fn with_locked_control_state<T, F>(path: &Path, mutator: F) -> Result<T>
where
    F: FnOnce(&mut ControlState) -> Result<(T, bool)>,
{
    with_control_lock(path, || {
        let mut state = load_control_state(path)?;
        let (out, changed) = mutator(&mut state)?;
        if changed {
            persist_control_state(path, &state)?;
        }
        Ok(out)
    })
}

fn with_control_lock<T, F>(path: &Path, op: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let lock_path = control_lock_path(path);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed creating lock dir {}", parent.display()))?;
    }

    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed opening lock file {}", lock_path.display()))?;

    lock_file
        .lock_exclusive()
        .with_context(|| format!("failed locking {}", lock_path.display()))?;

    let result = op();

    if let Err(err) = lock_file.unlock() {
        eprintln!("warning: failed unlocking {}: {err}", lock_path.display());
    }
    result
}

fn control_lock_path(path: &Path) -> PathBuf {
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "control".to_string());
    let lock_name = format!(".{file}.lock");
    match path.parent() {
        Some(parent) => parent.join(lock_name),
        None => Path::new(".").join(lock_name),
    }
}

pub fn now_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn mode_transitions_are_applied() {
        let mut control = ControlState::default();
        assert_eq!(control.mode, ControlMode::Auto);

        control.set_mode(ControlMode::Eco);
        assert_eq!(control.effective_mode(100), ControlMode::Eco);

        control.set_mode(ControlMode::Performance);
        assert_eq!(control.effective_mode(100), ControlMode::Performance);

        control.set_mode(ControlMode::Paused);
        assert_eq!(control.effective_mode(100), ControlMode::Paused);
    }

    #[test]
    fn override_expires_back_to_auto() {
        let mut control = ControlState::default();
        control.set_mode(ControlMode::Eco);
        control
            .set_override(ControlMode::Performance, 5, 1_000)
            .expect("set override");

        assert_eq!(control.effective_mode(2_000), ControlMode::Performance);
        assert_eq!(control.override_expires_ms(2_000), Some(6_000));

        let expired = control.expire_override_if_needed(6_000);
        assert!(expired);
        assert_eq!(control.effective_mode(6_000), ControlMode::Auto);
        assert_eq!(control.override_expires_ms(6_000), None);
    }

    #[test]
    fn persists_and_loads_runtime_state() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("control.json");

        let mut control = ControlState::default();
        control.set_mode(ControlMode::Paused);
        control.set_target_temp(73.5).expect("set target");
        control
            .set_override(ControlMode::Eco, 2, 100)
            .expect("set override");

        persist_control_state(&path, &control).expect("persist");
        let loaded = load_control_state(&path).expect("load");

        assert_eq!(loaded.mode, ControlMode::Paused);
        assert_eq!(loaded.target_temp_c, Some(73.5));
        assert_eq!(
            loaded.override_mode.expect("override").mode,
            ControlMode::Eco
        );
    }

    #[test]
    fn empty_control_file_defaults() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("control.json");
        std::fs::write(&path, "").expect("write empty file");

        let loaded = load_control_state(&path).expect("load empty file");
        assert_eq!(loaded.mode, ControlMode::Auto);
        assert!(loaded.override_mode.is_none());
    }

    #[test]
    fn locked_mutation_serializes_control_write() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("control.json");

        let mode = with_locked_control_state(&path, |state| {
            state.set_mode(ControlMode::Eco);
            Ok((state.mode, true))
        })
        .expect("locked mutate");
        assert_eq!(mode, ControlMode::Eco);

        let loaded = load_control_state(&path).expect("load");
        assert_eq!(loaded.mode, ControlMode::Eco);
    }
}
