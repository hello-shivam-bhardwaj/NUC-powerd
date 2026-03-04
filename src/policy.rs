use std::time::{Duration, Instant};

use crate::config::HysteresisConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThermalState {
    Cool,
    Warm,
    Hot,
    Critical,
}

#[derive(Debug, Clone, Copy)]
pub struct PolicyDecision {
    pub state: ThermalState,
    pub changed: bool,
}

pub struct PolicyEngine {
    thresholds: HysteresisConfig,
    state: ThermalState,
    last_transition: Instant,
}

impl PolicyEngine {
    pub fn new(thresholds: HysteresisConfig, now: Instant, initial_temp_c: f64) -> Self {
        let state = if initial_temp_c >= thresholds.critical_on_c {
            ThermalState::Critical
        } else if initial_temp_c >= thresholds.hot_on_c {
            ThermalState::Hot
        } else if initial_temp_c >= thresholds.warm_on_c {
            ThermalState::Warm
        } else {
            ThermalState::Cool
        };

        Self {
            thresholds,
            state,
            last_transition: now,
        }
    }

    pub fn state(&self) -> ThermalState {
        self.state
    }

    pub fn evaluate(&mut self, temp_c: f64, now: Instant) -> PolicyDecision {
        let candidate = match self.state {
            ThermalState::Cool => {
                if temp_c >= self.thresholds.warm_on_c {
                    ThermalState::Warm
                } else {
                    ThermalState::Cool
                }
            }
            ThermalState::Warm => {
                if temp_c >= self.thresholds.hot_on_c {
                    ThermalState::Hot
                } else if temp_c <= self.thresholds.warm_off_c {
                    ThermalState::Cool
                } else {
                    ThermalState::Warm
                }
            }
            ThermalState::Hot => {
                if temp_c >= self.thresholds.critical_on_c {
                    ThermalState::Critical
                } else if temp_c <= self.thresholds.hot_off_c {
                    ThermalState::Warm
                } else {
                    ThermalState::Hot
                }
            }
            ThermalState::Critical => {
                if temp_c <= self.thresholds.critical_off_c {
                    ThermalState::Hot
                } else {
                    ThermalState::Critical
                }
            }
        };

        let dwell = Duration::from_secs(self.thresholds.min_dwell_sec);
        if candidate != self.state && now.duration_since(self.last_transition) >= dwell {
            self.state = candidate;
            self.last_transition = now;
            PolicyDecision {
                state: self.state,
                changed: true,
            }
        } else {
            PolicyDecision {
                state: self.state,
                changed: false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> HysteresisConfig {
        HysteresisConfig {
            warm_on_c: 72.0,
            warm_off_c: 68.0,
            hot_on_c: 80.0,
            hot_off_c: 75.0,
            critical_on_c: 88.0,
            critical_off_c: 83.0,
            min_dwell_sec: 20,
        }
    }

    #[test]
    fn warms_then_cools_with_hysteresis() {
        let t0 = Instant::now();
        let mut engine = PolicyEngine::new(thresholds(), t0, 60.0);
        assert_eq!(engine.state(), ThermalState::Cool);

        let d1 = engine.evaluate(73.0, t0 + Duration::from_secs(21));
        assert!(d1.changed);
        assert_eq!(d1.state, ThermalState::Warm);

        let d2 = engine.evaluate(70.0, t0 + Duration::from_secs(42));
        assert!(!d2.changed);
        assert_eq!(d2.state, ThermalState::Warm);

        let d3 = engine.evaluate(67.0, t0 + Duration::from_secs(63));
        assert!(d3.changed);
        assert_eq!(d3.state, ThermalState::Cool);
    }

    #[test]
    fn min_dwell_blocks_rapid_transition() {
        let t0 = Instant::now();
        let mut engine = PolicyEngine::new(thresholds(), t0, 60.0);

        let d1 = engine.evaluate(82.0, t0 + Duration::from_secs(5));
        assert!(!d1.changed);
        assert_eq!(d1.state, ThermalState::Cool);

        let d2 = engine.evaluate(82.0, t0 + Duration::from_secs(21));
        assert!(d2.changed);
        assert_eq!(d2.state, ThermalState::Warm);
    }

    #[test]
    fn critical_state_requires_critical_off_to_drop() {
        let t0 = Instant::now();
        let mut engine = PolicyEngine::new(thresholds(), t0, 89.0);
        assert_eq!(engine.state(), ThermalState::Critical);

        let d1 = engine.evaluate(86.0, t0 + Duration::from_secs(25));
        assert!(!d1.changed);
        assert_eq!(d1.state, ThermalState::Critical);

        let d2 = engine.evaluate(82.0, t0 + Duration::from_secs(50));
        assert!(d2.changed);
        assert_eq!(d2.state, ThermalState::Hot);
    }
}
