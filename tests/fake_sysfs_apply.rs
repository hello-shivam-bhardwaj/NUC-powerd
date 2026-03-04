use std::fs;

use nuc_powerd::actuators::{Actuator, SysfsActuator};
use nuc_powerd::config::StateProfile;
use tempfile::tempdir;

#[test]
fn integration_fake_sysfs_profile_apply_and_status_output() {
    let dir = tempdir().expect("tempdir");
    let epp = dir.path().join("epp");
    let max_freq = dir.path().join("max_freq");
    let max_cap = dir.path().join("max_cap");
    let no_turbo = dir.path().join("no_turbo");
    let rapl = dir.path().join("rapl_limit");

    fs::write(&epp, "balance_performance\n").expect("seed epp");
    fs::write(&max_freq, "3200000\n").expect("seed max freq");
    fs::write(&max_cap, "4000000\n").expect("seed cap");
    fs::write(&no_turbo, "0\n").expect("seed turbo");
    fs::write(&rapl, "24000000\n").expect("seed rapl");

    let mut act = SysfsActuator::with_paths(
        epp.clone(),
        max_freq.clone(),
        max_cap,
        no_turbo,
        rapl.clone(),
        false,
    );
    let profile = StateProfile {
        epp: "power".to_string(),
        turbo: false,
        max_freq_pct: 65,
        rapl_pkg_w: Some(18),
    };

    act.apply_profile(&profile, true).expect("apply profile");

    assert_eq!(fs::read_to_string(&epp).expect("read epp").trim(), "power");
    assert_eq!(
        fs::read_to_string(&max_freq).expect("read max freq").trim(),
        "2600000"
    );
    assert_eq!(
        fs::read_to_string(&rapl).expect("read rapl").trim(),
        "18000000"
    );
}
