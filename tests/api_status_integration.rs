#![cfg(feature = "ui")]

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use nuc_powerd::actuators::{Actuator, AppliedControls};
use nuc_powerd::api::{spawn_ui_server, UiServerConfig};
use nuc_powerd::config::{
    Config, DaemonConfig, HysteresisConfig, SafetyConfig, StateConfig, StateProfile,
};
use nuc_powerd::controller::{target_temp_bounds, Controller};
use nuc_powerd::sensors::{SensorReader, Telemetry};
use serde_json::Value;
use tempfile::tempdir;

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

struct FakeActuator {
    calls: Arc<AtomicUsize>,
}

impl Actuator for FakeActuator {
    fn apply_profile(
        &mut self,
        profile: &StateProfile,
        _rollback_on_error: bool,
    ) -> Result<AppliedControls> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(AppliedControls {
            no_turbo: !profile.turbo,
            max_freq_khz: 2_800_000,
        })
    }
}

fn sample(temp: f64) -> Telemetry {
    Telemetry {
        temp_cpu_c: temp,
        cpu_util_pct: 20.0,
        freq_mhz: Some(2400.0),
        pkg_power_w: Some(15.0),
        timestamp_ms: 0,
    }
}

fn next_local_bind() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let addr = listener.local_addr().expect("local addr");
    format!("127.0.0.1:{}", addr.port())
}

fn test_config(status_path: String, control_path: String, api_bind: String) -> Config {
    Config {
        daemon: DaemonConfig {
            interval_ms: 10,
            status_path,
            api_bind,
            control_path,
            service_unit: "nuc-powerd.service".to_string(),
            stress_program: "stress-ng".to_string(),
        },
        safety: SafetyConfig {
            critical_temp_c: 90.0,
            panic_temp_c: 95.0,
            sensor_stale_sec: 2,
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

fn http_json(addr: &str, method: &str, path: &str, body: Option<&str>) -> Value {
    let body_raw = body.unwrap_or("");
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set timeout");

    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_raw}",
        body_raw.len()
    );
    stream.write_all(req.as_bytes()).expect("write request");

    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read response");

    let mut parts = raw.splitn(2, "\r\n\r\n");
    let head = parts.next().expect("response head");
    let body = parts.next().unwrap_or_default();

    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    assert!(
        (200..300).contains(&status),
        "unexpected status {status}, response: {raw}"
    );

    serde_json::from_str(body).expect("json body")
}

fn http_text(addr: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set timeout");

    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).expect("write request");

    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read response");

    let mut parts = raw.splitn(2, "\r\n\r\n");
    let head = parts.next().expect("response head");
    let body = parts.next().unwrap_or_default();

    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    assert!(
        (200..300).contains(&status),
        "unexpected status {status}, response: {raw}"
    );

    body.to_string()
}

#[test]
fn integration_api_status_with_fake_controller() {
    let dir = tempdir().expect("tempdir");
    let status_path = dir.path().join("status.json");
    let control_path = dir.path().join("control.json");
    let api_bind = next_local_bind();

    let cfg = test_config(
        status_path.display().to_string(),
        control_path.display().to_string(),
        api_bind.clone(),
    );

    let sensor = FakeSensor::new(vec![
        Ok(sample(70.0)),
        Ok(sample(70.0)),
        Ok(sample(70.0)),
        Ok(sample(70.0)),
        Ok(sample(70.0)),
    ]);

    let calls = Arc::new(AtomicUsize::new(0));
    let actuator = FakeActuator {
        calls: calls.clone(),
    };

    let mut controller = Controller::new(cfg.clone(), sensor, actuator).expect("controller");

    let running = Arc::new(AtomicBool::new(true));
    let (target_min_c, target_max_c) = target_temp_bounds(&cfg.hysteresis);
    let ui_thread = spawn_ui_server(
        UiServerConfig {
            bind: api_bind.clone(),
            status_path: cfg.daemon.status_path.clone(),
            control_path: cfg.daemon.control_path.clone(),
            target_min_c,
            target_max_c,
            service_unit: cfg.daemon.service_unit.clone(),
            stress_program: cfg.daemon.stress_program.clone(),
        },
        running.clone(),
    )
    .expect("ui server");

    thread::sleep(Duration::from_millis(120));

    let html = http_text(&api_bind, "/");
    assert!(html.contains("Thermal Control Console"));
    let js = http_text(&api_bind, "/static/app.js");
    assert!(js.contains("fetchStatus"));
    let css = http_text(&api_bind, "/static/styles.css");
    assert!(css.contains(".app-shell"));

    let stress = http_json(&api_bind, "GET", "/stress/status", None);
    assert_eq!(stress["running"], false);
    let service = http_json(&api_bind, "GET", "/service/status", None);
    assert_eq!(service["unit"], cfg.daemon.service_unit);
    assert!(service["active"].is_boolean());

    controller.tick().expect("tick 1");

    let mode_resp = http_json(
        &api_bind,
        "POST",
        "/thermal/mode",
        Some(r#"{"mode":"performance"}"#),
    );
    assert_eq!(mode_resp["effective_mode"], "performance");

    let target_resp = http_json(
        &api_bind,
        "POST",
        "/thermal/target",
        Some(r#"{"target_temp_c":76.0}"#),
    );
    assert_eq!(target_resp["target_temp_c"], 76.0);

    let override_resp = http_json(
        &api_bind,
        "POST",
        "/thermal/override",
        Some(r#"{"mode":"eco","ttl_sec":1}"#),
    );
    assert_eq!(override_resp["effective_mode"], "eco");

    controller.tick().expect("tick 2");
    let status = http_json(&api_bind, "GET", "/thermal/status", None);

    assert_eq!(status["mode"], "eco");
    assert_eq!(status["state"], "warm");
    assert_eq!(status["health"], "ok");
    assert_eq!(status["max_freq_khz"], 2_800_000_u64);
    assert!(status["override_expires_ms"].is_number());
    assert!(status["target_min_c"].is_number());
    assert!(status["target_max_c"].is_number());

    thread::sleep(Duration::from_millis(1_100));
    controller.tick().expect("tick 3");

    let expired_status = http_json(&api_bind, "GET", "/thermal/status", None);
    assert_eq!(expired_status["mode"], "auto");
    assert!(expired_status["override_expires_ms"].is_null());

    let paused = http_json(
        &api_bind,
        "POST",
        "/thermal/mode",
        Some(r#"{"mode":"paused"}"#),
    );
    assert_eq!(paused["effective_mode"], "paused");

    let before_pause_calls = calls.load(Ordering::SeqCst);
    controller.tick().expect("tick 4");
    assert_eq!(calls.load(Ordering::SeqCst), before_pause_calls);

    let status_file = fs::read_to_string(status_path).expect("status file");
    assert!(status_file.contains("\"mode\": \"paused\""));

    running.store(false, Ordering::SeqCst);
    let _ = ui_thread.join();
}
