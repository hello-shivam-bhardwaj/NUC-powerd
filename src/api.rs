use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::control::{
    load_control_state, now_unix_ms, with_locked_control_state, ControlMode, ControlState,
};
use crate::policy::ThermalState;
use crate::status::{Health, RuntimeStatus};

const DASHBOARD_HTML: &str = include_str!("../web/index.html");
const DASHBOARD_JS: &str = include_str!("../web/app.js");
const DASHBOARD_CSS: &str = include_str!("../web/styles.css");
const MAX_JSON_BODY_BYTES: u64 = 65_536;
const MAX_STRESS_DURATION_SEC: u64 = 86_400;
const MAX_STRESS_WORKERS: u32 = 512;
const MAX_STRESS_CPU_LOAD: u8 = 100;

#[derive(Debug, Clone)]
pub struct UiServerConfig {
    pub bind: String,
    pub status_path: String,
    pub control_path: String,
    pub target_min_c: f64,
    pub target_max_c: f64,
    pub service_unit: String,
    pub stress_program: String,
}

#[derive(Debug, Deserialize)]
struct ModeRequest {
    mode: String,
}

#[derive(Debug, Deserialize)]
struct OverrideRequest {
    mode: String,
    ttl_sec: u64,
}

#[derive(Debug, Deserialize)]
struct TargetRequest {
    target_temp_c: f64,
}

#[derive(Debug, Deserialize)]
struct StressStartRequest {
    duration_sec: Option<u64>,
    workers: Option<u32>,
    cpu_load: Option<u8>,
}

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
}

#[derive(Debug, Serialize)]
struct ControlResponse {
    mode: String,
    effective_mode: String,
    override_expires_ms: Option<u128>,
    target_temp_c: Option<f64>,
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    #[serde(flatten)]
    status: RuntimeStatus,
    target_min_c: f64,
    target_max_c: f64,
}

#[derive(Debug, Serialize)]
struct StressStatus {
    running: bool,
    pid: Option<u32>,
    started_at_ms: Option<u128>,
    duration_sec: Option<u64>,
    workers: Option<u32>,
    cpu_load: Option<u8>,
    max_workers: u32,
    max_cpu_load: u8,
    last_exit_code: Option<i32>,
    last_message: Option<String>,
}

#[derive(Debug, Serialize)]
struct ServiceStatus {
    unit: String,
    active: bool,
    active_state: String,
    sub_state: Option<String>,
    enabled_state: Option<String>,
    main_pid: Option<u32>,
    control_available: bool,
    used_sudo: bool,
    last_message: Option<String>,
}

#[derive(Debug)]
struct StressProcess {
    child: Child,
    started_at_ms: u128,
    duration_sec: Option<u64>,
    workers: u32,
    cpu_load: u8,
}

#[derive(Debug)]
struct StressManager {
    program: String,
    process: Option<StressProcess>,
    last_exit_code: Option<i32>,
    last_message: Option<String>,
}

#[derive(Debug)]
struct ServiceManager {
    unit: String,
    last_message: Option<String>,
}

pub fn spawn_ui_server(cfg: UiServerConfig, running: Arc<AtomicBool>) -> Result<JoinHandle<()>> {
    let stress = Arc::new(Mutex::new(StressManager::new(&cfg.stress_program)));
    let service = Arc::new(Mutex::new(ServiceManager::new(&cfg.service_unit)));
    let server = Server::http(&cfg.bind)
        .map_err(|err| anyhow!("failed binding ui server at {}: {err}", cfg.bind))?;

    let handle = thread::spawn(move || {
        if let Err(err) = run_server(server, cfg, stress, service, running) {
            eprintln!("ui server stopped with error: {err:#}");
        }
    });

    Ok(handle)
}

fn run_server(
    server: Server,
    cfg: UiServerConfig,
    stress: Arc<Mutex<StressManager>>,
    service: Arc<Mutex<ServiceManager>>,
    running: Arc<AtomicBool>,
) -> Result<()> {
    while running.load(Ordering::SeqCst) {
        match server.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(request)) => {
                if let Err(err) = handle_request(request, &cfg, &stress, &service) {
                    eprintln!("ui request error: {err:#}");
                }
            }
            Ok(None) => {}
            Err(err) => {
                return Err(anyhow!(err)).context("ui server recv_timeout failed");
            }
        }
    }

    Ok(())
}

fn handle_request(
    mut request: Request,
    cfg: &UiServerConfig,
    stress: &Arc<Mutex<StressManager>>,
    service: &Arc<Mutex<ServiceManager>>,
) -> Result<()> {
    if !is_loopback_request(&request) {
        return respond_error(request, 403, "loopback clients only");
    }

    let path = request
        .url()
        .split('?')
        .next()
        .unwrap_or(request.url())
        .to_string();

    match (request.method(), path.as_str()) {
        (&Method::Get, "/") | (&Method::Get, "/index.html") => {
            respond_text(request, 200, DASHBOARD_HTML, b"text/html; charset=utf-8")
        }
        (&Method::Get, "/static/app.js") => respond_text(
            request,
            200,
            DASHBOARD_JS,
            b"application/javascript; charset=utf-8",
        ),
        (&Method::Get, "/static/styles.css") => {
            respond_text(request, 200, DASHBOARD_CSS, b"text/css; charset=utf-8")
        }
        (&Method::Get, "/thermal/status") => {
            let snapshot = read_status_or_bootstrap(cfg)?;
            respond_json(
                request,
                200,
                &StatusResponse {
                    status: snapshot,
                    target_min_c: cfg.target_min_c,
                    target_max_c: cfg.target_max_c,
                },
            )
        }
        (&Method::Post, "/thermal/mode") => {
            let payload: ModeRequest = match parse_json_body(&mut request) {
                Ok(value) => value,
                Err(err) => return respond_error(request, 400, &format!("invalid request: {err}")),
            };
            let mode = match ControlMode::from_str(&payload.mode) {
                Ok(value) => value,
                Err(err) => return respond_error(request, 400, &format!("invalid mode: {err}")),
            };

            match mutate_control(Path::new(&cfg.control_path), |state, _now_ms| {
                state.set_mode(mode);
                Ok(())
            }) {
                Ok(snapshot) => respond_json(request, 200, &snapshot),
                Err(err) => respond_error(request, 500, &format!("failed updating mode: {err}")),
            }
        }
        (&Method::Post, "/thermal/override") => {
            let payload: OverrideRequest = match parse_json_body(&mut request) {
                Ok(value) => value,
                Err(err) => return respond_error(request, 400, &format!("invalid request: {err}")),
            };
            let mode = match ControlMode::from_str(&payload.mode) {
                Ok(value) => value,
                Err(err) => return respond_error(request, 400, &format!("invalid mode: {err}")),
            };

            match mutate_control(Path::new(&cfg.control_path), |state, now_ms| {
                state.set_override(mode, payload.ttl_sec, now_ms)
            }) {
                Ok(snapshot) => respond_json(request, 200, &snapshot),
                Err(err) => {
                    if err.to_string().contains("ttl_sec") {
                        respond_error(request, 400, &format!("invalid override: {err}"))
                    } else {
                        respond_error(request, 500, &format!("failed updating override: {err}"))
                    }
                }
            }
        }
        (&Method::Post, "/thermal/target") => {
            let payload: TargetRequest = match parse_json_body(&mut request) {
                Ok(value) => value,
                Err(err) => return respond_error(request, 400, &format!("invalid request: {err}")),
            };
            if payload.target_temp_c < cfg.target_min_c || payload.target_temp_c > cfg.target_max_c
            {
                return respond_error(
                    request,
                    400,
                    &format!(
                        "target_temp_c must be in [{:.1}, {:.1}]",
                        cfg.target_min_c, cfg.target_max_c
                    ),
                );
            }

            match mutate_control(Path::new(&cfg.control_path), |state, _now_ms| {
                state.set_target_temp(payload.target_temp_c)
            }) {
                Ok(snapshot) => respond_json(request, 200, &snapshot),
                Err(err) => respond_error(request, 500, &format!("failed updating target: {err}")),
            }
        }
        (&Method::Get, "/stress/status") => {
            let payload = match stress.lock() {
                Ok(mut manager) => manager.status(),
                Err(_) => {
                    return respond_error(request, 500, "stress manager lock poisoned");
                }
            };
            respond_json(request, 200, &payload)
        }
        (&Method::Post, "/stress/start") => {
            let payload: StressStartRequest = match parse_json_body(&mut request) {
                Ok(value) => value,
                Err(err) => return respond_error(request, 400, &format!("invalid request: {err}")),
            };
            let response = match stress.lock() {
                Ok(mut manager) => match manager.start(payload) {
                    Ok(s) => s,
                    Err(err) => {
                        if err.to_string().contains("must be")
                            || err.to_string().contains("already")
                        {
                            return respond_error(
                                request,
                                400,
                                &format!("invalid stress config: {err}"),
                            );
                        }
                        return respond_error(
                            request,
                            500,
                            &format!("failed starting stress: {err}"),
                        );
                    }
                },
                Err(_) => return respond_error(request, 500, "stress manager lock poisoned"),
            };
            respond_json(request, 200, &response)
        }
        (&Method::Post, "/stress/stop") => {
            let response = match stress.lock() {
                Ok(mut manager) => match manager.stop() {
                    Ok(s) => s,
                    Err(err) => {
                        return respond_error(
                            request,
                            500,
                            &format!("failed stopping stress: {err}"),
                        );
                    }
                },
                Err(_) => return respond_error(request, 500, "stress manager lock poisoned"),
            };
            respond_json(request, 200, &response)
        }
        (&Method::Get, "/service/status") => {
            let payload = match service.lock() {
                Ok(mut manager) => manager.status(),
                Err(_) => {
                    return respond_error(request, 500, "service manager lock poisoned");
                }
            };
            respond_json(request, 200, &payload)
        }
        (&Method::Post, "/service/start") => {
            let response = match service.lock() {
                Ok(mut manager) => match manager.start() {
                    Ok(s) => s,
                    Err(err) => {
                        return respond_error(
                            request,
                            500,
                            &format!("failed starting service: {err}"),
                        );
                    }
                },
                Err(_) => return respond_error(request, 500, "service manager lock poisoned"),
            };
            respond_json(request, 200, &response)
        }
        (&Method::Post, "/service/stop") => {
            let response = match service.lock() {
                Ok(mut manager) => match manager.stop() {
                    Ok(s) => s,
                    Err(err) => {
                        return respond_error(
                            request,
                            500,
                            &format!("failed stopping service: {err}"),
                        );
                    }
                },
                Err(_) => return respond_error(request, 500, "service manager lock poisoned"),
            };
            respond_json(request, 200, &response)
        }
        (&Method::Post, "/service/restart") => {
            let response = match service.lock() {
                Ok(mut manager) => match manager.restart() {
                    Ok(s) => s,
                    Err(err) => {
                        return respond_error(
                            request,
                            500,
                            &format!("failed restarting service: {err}"),
                        );
                    }
                },
                Err(_) => return respond_error(request, 500, "service manager lock poisoned"),
            };
            respond_json(request, 200, &response)
        }
        _ => respond_error(request, 404, "not found"),
    }
}

fn read_status_or_bootstrap(cfg: &UiServerConfig) -> Result<RuntimeStatus> {
    let status_path = Path::new(&cfg.status_path);
    match fs::read_to_string(status_path) {
        Ok(raw) => serde_json::from_str(&raw)
            .with_context(|| format!("failed parsing status {}", status_path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let now_ms = now_unix_ms();
            let control = load_control_state(Path::new(&cfg.control_path)).unwrap_or_default();
            let mut status = RuntimeStatus::bootstrap(
                control.effective_mode(now_ms).as_str(),
                ThermalState::Cool,
            );
            status.health = Health::Error;
            status.override_expires_ms = control.override_expires_ms(now_ms);
            status.target_temp_c = control.target_temp_c;
            status.message = "daemon status file not found".to_string();
            Ok(status)
        }
        Err(err) => Err(err).with_context(|| format!("failed reading {}", status_path.display())),
    }
}

fn mutate_control<F>(control_path: &Path, mutator: F) -> Result<ControlResponse>
where
    F: FnOnce(&mut ControlState, u128) -> Result<()>,
{
    let now_ms = now_unix_ms();
    with_locked_control_state(control_path, |state| {
        mutator(state, now_ms)?;
        Ok((
            ControlResponse {
                mode: state.mode.as_str().to_string(),
                effective_mode: state.effective_mode(now_ms).as_str().to_string(),
                override_expires_ms: state.override_expires_ms(now_ms),
                target_temp_c: state.target_temp_c,
            },
            true,
        ))
    })
}

impl StressManager {
    fn new(program: &str) -> Self {
        Self {
            program: program.to_string(),
            process: None,
            last_exit_code: None,
            last_message: None,
        }
    }

    fn start(&mut self, req: StressStartRequest) -> Result<StressStatus> {
        self.poll()?;
        if self.process.is_some() {
            return Err(anyhow!("stress test already running"));
        }

        let duration_sec = req.duration_sec.unwrap_or(0);
        if duration_sec > MAX_STRESS_DURATION_SEC {
            return Err(anyhow!(
                "duration_sec must be in 0..={} (0 keeps running)",
                MAX_STRESS_DURATION_SEC
            ));
        }

        let workers = req.workers.unwrap_or(default_workers());
        if workers == 0 || workers > MAX_STRESS_WORKERS {
            return Err(anyhow!("workers must be in 1..={MAX_STRESS_WORKERS}"));
        }

        let cpu_load = req.cpu_load.unwrap_or(MAX_STRESS_CPU_LOAD);
        if cpu_load == 0 || cpu_load > MAX_STRESS_CPU_LOAD {
            return Err(anyhow!("cpu_load must be in 1..={MAX_STRESS_CPU_LOAD}"));
        }

        let mut command = Command::new(&self.program);
        command
            .arg("--cpu")
            .arg(workers.to_string())
            .arg("--cpu-load")
            .arg(cpu_load.to_string())
            .arg("--metrics-brief")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let duration_value = if duration_sec == 0 {
            None
        } else {
            command.arg("--timeout").arg(format!("{duration_sec}s"));
            Some(duration_sec)
        };

        let child = command
            .spawn()
            .with_context(|| format!("failed spawning {}", self.program))?;
        let started_at_ms = now_unix_ms();
        let pid = child.id();
        self.last_message = Some(match duration_value {
            Some(duration) => format!("started stress-ng pid={pid} for {duration}s"),
            None => format!("started stress-ng pid={pid} (keep running)"),
        });
        self.process = Some(StressProcess {
            child,
            started_at_ms,
            duration_sec: duration_value,
            workers,
            cpu_load,
        });
        Ok(self.snapshot())
    }

    fn stop(&mut self) -> Result<StressStatus> {
        self.poll()?;
        if let Some(mut proc) = self.process.take() {
            proc.child.kill().context("failed killing stress process")?;
            let status = proc.child.wait().context("failed waiting stress process")?;
            self.last_exit_code = status.code();
            self.last_message = Some("stopped by user".to_string());
        } else {
            self.last_message = Some("no running stress process".to_string());
        }

        Ok(self.snapshot())
    }

    fn status(&mut self) -> StressStatus {
        if let Err(err) = self.poll() {
            self.last_message = Some(format!("poll error: {err}"));
        }
        self.snapshot()
    }

    fn poll(&mut self) -> Result<()> {
        if let Some(proc) = &mut self.process {
            if let Some(status) = proc
                .child
                .try_wait()
                .context("failed reading stress status")?
            {
                self.last_exit_code = status.code();
                self.last_message = Some(format!(
                    "stress process exited with code {}",
                    status.code().unwrap_or(-1)
                ));
                self.process = None;
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> StressStatus {
        match &self.process {
            Some(proc) => StressStatus {
                running: true,
                pid: Some(proc.child.id()),
                started_at_ms: Some(proc.started_at_ms),
                duration_sec: proc.duration_sec,
                workers: Some(proc.workers),
                cpu_load: Some(proc.cpu_load),
                max_workers: default_workers().min(MAX_STRESS_WORKERS),
                max_cpu_load: MAX_STRESS_CPU_LOAD,
                last_exit_code: self.last_exit_code,
                last_message: self.last_message.clone(),
            },
            None => StressStatus {
                running: false,
                pid: None,
                started_at_ms: None,
                duration_sec: None,
                workers: None,
                cpu_load: None,
                max_workers: default_workers().min(MAX_STRESS_WORKERS),
                max_cpu_load: MAX_STRESS_CPU_LOAD,
                last_exit_code: self.last_exit_code,
                last_message: self.last_message.clone(),
            },
        }
    }
}

impl ServiceManager {
    fn new(unit: &str) -> Self {
        Self {
            unit: unit.to_string(),
            last_message: None,
        }
    }

    fn status(&mut self) -> ServiceStatus {
        let active = match run_systemctl(&["is-active", &self.unit], true) {
            Ok(result) => result,
            Err(err) => {
                return ServiceStatus {
                    unit: self.unit.clone(),
                    active: false,
                    active_state: "unknown".to_string(),
                    sub_state: None,
                    enabled_state: None,
                    main_pid: None,
                    control_available: false,
                    used_sudo: false,
                    last_message: Some(format!("systemctl unavailable: {err}")),
                };
            }
        };
        let enabled = run_systemctl(&["is-enabled", &self.unit], true).ok();
        let show = run_systemctl(
            &[
                "show", "-p", "SubState", "-p", "MainPID", "--value", &self.unit,
            ],
            true,
        )
        .ok();

        let active_state = normalized_status_text(&active.stdout).unwrap_or("unknown");
        let active_bool = active_state == "active";
        let enabled_state = enabled
            .as_ref()
            .and_then(|v| normalized_status_text(&v.stdout))
            .map(str::to_string);

        let (sub_state, main_pid) = show.as_ref().map(parse_show_output).unwrap_or((None, None));

        let used_sudo = active.used_sudo
            || enabled.as_ref().map(|v| v.used_sudo).unwrap_or(false)
            || show.as_ref().map(|v| v.used_sudo).unwrap_or(false);

        let mut message = self.last_message.clone();
        if message.is_none() {
            message = first_error_message(&active, enabled.as_ref(), show.as_ref());
        }
        if message.is_none() && is_permission_denied(&active.stderr) && !used_sudo {
            message = Some(
                "service control unavailable: run UI as root or allow passwordless sudo for systemctl"
                    .to_string(),
            );
        }

        ServiceStatus {
            unit: self.unit.clone(),
            active: active_bool,
            active_state: active_state.to_string(),
            sub_state,
            enabled_state,
            main_pid,
            control_available: !is_permission_denied(&active.stderr) || used_sudo,
            used_sudo,
            last_message: message,
        }
    }

    fn start(&mut self) -> Result<ServiceStatus> {
        let _ = run_systemctl(&["reset-failed", &self.unit], true);
        run_systemctl(&["start", &self.unit], false)?;
        self.last_message = Some("service start requested".to_string());
        Ok(self.status())
    }

    fn stop(&mut self) -> Result<ServiceStatus> {
        run_systemctl(&["stop", &self.unit], false)?;
        self.last_message = Some("service stop requested".to_string());
        Ok(self.status())
    }

    fn restart(&mut self) -> Result<ServiceStatus> {
        run_systemctl(&["restart", &self.unit], false)?;
        self.last_message = Some("service restart requested".to_string());
        Ok(self.status())
    }
}

#[derive(Debug)]
struct SystemctlResult {
    stdout: String,
    stderr: String,
    code: i32,
    used_sudo: bool,
}

fn run_systemctl(args: &[&str], allow_nonzero: bool) -> Result<SystemctlResult> {
    let mut output = Command::new("systemctl")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed executing systemctl {}", args.join(" ")))?;
    let mut used_sudo = false;

    if is_permission_denied(&output_string(&output)) {
        if let Ok(sudo_output) = Command::new("sudo")
            .arg("-n")
            .arg("systemctl")
            .args(args)
            .stdin(Stdio::null())
            .output()
        {
            output = sudo_output;
            used_sudo = true;
        }
    }

    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !allow_nonzero && !output.status.success() {
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        return Err(anyhow!(
            "systemctl {} failed (exit={}): {}",
            args.join(" "),
            code,
            msg
        ));
    }

    Ok(SystemctlResult {
        stdout,
        stderr,
        code,
        used_sudo,
    })
}

fn parse_show_output(show: &SystemctlResult) -> (Option<String>, Option<u32>) {
    let mut sub_state: Option<String> = None;
    let mut main_pid: Option<u32> = None;

    for line in show.stdout.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if main_pid.is_none() {
            if let Ok(pid) = line.parse::<u32>() {
                if pid > 0 {
                    main_pid = Some(pid);
                    continue;
                }
            }
        }

        if sub_state.is_none() {
            sub_state = Some(line.to_string());
        }
    }

    (sub_state, main_pid)
}

fn first_error_message(
    active: &SystemctlResult,
    enabled: Option<&SystemctlResult>,
    show: Option<&SystemctlResult>,
) -> Option<String> {
    for item in [Some(active), enabled, show].into_iter().flatten() {
        if item.code == 0 {
            continue;
        }
        if !item.stderr.is_empty() {
            return Some(item.stderr.clone());
        }

        let text = item.stdout.trim();
        if text.is_empty() {
            continue;
        }
        let lower = text.to_ascii_lowercase();
        if lower.contains("failed")
            || lower.contains("error")
            || lower.contains("denied")
            || lower.contains("not found")
            || lower.contains("unknown")
        {
            return Some(text.to_string());
        }
    }
    None
}

fn normalized_status_text(s: &str) -> Option<&str> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn output_string(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{stdout}\n{stderr}")
}

fn is_permission_denied(message: &str) -> bool {
    let msg = message.to_ascii_lowercase();
    msg.contains("operation not permitted")
        || msg.contains("access denied")
        || msg.contains("failed to connect to bus")
        || msg.contains("interactive authentication required")
}

fn default_workers() -> u32 {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1) as u32
}

fn parse_json_body<T: for<'de> Deserialize<'de>>(request: &mut Request) -> Result<T> {
    let mut raw = String::new();
    let limit = MAX_JSON_BODY_BYTES + 1;
    let mut limited = request.as_reader().take(limit);
    limited
        .read_to_string(&mut raw)
        .context("failed reading request body")?;
    if raw.len() > MAX_JSON_BODY_BYTES as usize {
        return Err(anyhow!(
            "request body too large (max {} bytes)",
            MAX_JSON_BODY_BYTES
        ));
    }

    serde_json::from_str::<T>(&raw).context("invalid JSON request body")
}

fn is_loopback_request(request: &Request) -> bool {
    request
        .remote_addr()
        .map(|addr| addr.ip().is_loopback())
        .unwrap_or(false)
}

fn respond_json<T: Serialize>(request: Request, code: u16, value: &T) -> Result<()> {
    let body = serde_json::to_string(value)?;
    let response = Response::from_string(body)
        .with_status_code(StatusCode(code))
        .with_header(content_type_header(b"application/json"));
    request.respond(response).context("failed responding")?;
    Ok(())
}

fn respond_text(request: Request, code: u16, body: &str, content_type: &[u8]) -> Result<()> {
    let response = Response::from_string(body)
        .with_status_code(StatusCode(code))
        .with_header(content_type_header(content_type));
    request.respond(response).context("failed responding")?;
    Ok(())
}

fn respond_error(request: Request, code: u16, message: &str) -> Result<()> {
    respond_json(
        request,
        code,
        &ApiError {
            error: message.to_string(),
        },
    )
}

fn content_type_header(content_type: &[u8]) -> Header {
    Header::from_bytes(b"Content-Type", content_type).expect("valid static content-type header")
}
