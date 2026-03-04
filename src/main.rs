use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nuc_powerd::actuators::SysfsActuator;
use nuc_powerd::config::load_config;
use nuc_powerd::control::{load_control_state, persist_control_state};
use nuc_powerd::controller::Controller;
use nuc_powerd::sensors::LinuxSensors;

#[derive(Parser, Debug)]
#[command(name = "nuc-powerd")]
#[command(about = "Thermal-aware CPU policy daemon for Intel NUC systems")]
struct Cli {
    #[arg(long, default_value = "config/nuc-powerd.example.toml")]
    config: PathBuf,

    #[command(subcommand)]
    cmd: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run daemon in active mode (writes sysfs controls).
    Run,
    /// Run daemon in dry-run mode (no sysfs writes).
    DryRun,
    /// Print current status JSON.
    Status,
    /// Check environment and controller conflicts.
    Doctor,
}

fn main() {
    if let Err(err) = run_main() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run_main() -> Result<()> {
    let cli = Cli::parse();
    let cmd = cli.cmd.unwrap_or(Command::Run);

    match cmd {
        Command::Run => run_controller(&cli.config, false),
        Command::DryRun => run_controller(&cli.config, true),
        Command::Status => print_status(&cli.config),
        Command::Doctor => doctor(&cli.config),
    }
}

fn run_controller(config_path: &Path, dry_run: bool) -> Result<()> {
    let cfg = load_config(config_path).context("failed loading config")?;
    let status_path = cfg.daemon.status_path.clone();
    let interval_ms = cfg.daemon.interval_ms;

    let control_path = PathBuf::from(&cfg.daemon.control_path);
    let initial_control =
        load_control_state(&control_path).context("failed loading control state")?;
    persist_control_state(&control_path, &initial_control)
        .context("failed persisting initial control state")?;

    let mut controller = Controller::new(cfg, LinuxSensors::new(), SysfsActuator::new(dry_run))
        .context("failed creating controller")?;

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .context("failed setting signal handler")?;

    println!(
        "starting nuc-powerd in {} mode (interval={}ms)",
        if dry_run { "dry-run" } else { "auto" },
        interval_ms
    );

    let loop_result = (|| -> Result<()> {
        while running.load(Ordering::SeqCst) {
            controller.tick().context("controller tick failed")?;
            std::thread::sleep(std::time::Duration::from_millis(interval_ms));
        }
        Ok(())
    })();

    running.store(false, Ordering::SeqCst);

    loop_result?;
    println!("stopped. latest status: {status_path}");
    Ok(())
}

fn print_status(config_path: &Path) -> Result<()> {
    let cfg = load_config(config_path).context("failed loading config")?;
    let status_path = Path::new(&cfg.daemon.status_path);

    match fs::read_to_string(status_path) {
        Ok(raw) => {
            println!("{raw}");
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "status file not found at {} (daemon may not have run yet)",
                status_path.display()
            );
            Ok(())
        }
        Err(err) => Err(err).with_context(|| format!("failed reading {}", status_path.display())),
    }
}

fn doctor(config_path: &Path) -> Result<()> {
    let cfg = load_config(config_path).context("failed loading config")?;

    println!("[doctor] config path: {}", config_path.display());
    println!("[doctor] status path: {}", cfg.daemon.status_path);
    println!("[doctor] api bind: {}", cfg.daemon.api_bind);

    if let Some(parent) = Path::new(&cfg.daemon.status_path).parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            println!(
                "[doctor] status dir create failed: {} ({err})",
                parent.display()
            );
        } else {
            println!("[doctor] status dir ready: {}", parent.display());
        }
    }

    if let Some(parent) = Path::new(&cfg.daemon.control_path).parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            println!(
                "[doctor] control dir create failed: {} ({err})",
                parent.display()
            );
        } else {
            println!("[doctor] control dir ready: {}", parent.display());
        }
    }

    for (label, path) in [
        (
            "epp",
            "/sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference",
        ),
        (
            "max_freq",
            "/sys/devices/system/cpu/cpufreq/policy0/scaling_max_freq",
        ),
        (
            "intel_pstate",
            "/sys/devices/system/cpu/intel_pstate/no_turbo",
        ),
        (
            "rapl",
            "/sys/class/powercap/intel-rapl/intel-rapl:0/constraint_0_power_limit_uw",
        ),
    ] {
        let p = Path::new(path);
        if p.exists() {
            match fs::metadata(p) {
                Ok(meta) => {
                    if meta.permissions().readonly() {
                        println!("[doctor] {label}: present but readonly ({path})");
                    } else {
                        println!("[doctor] {label}: present and writable ({path})");
                    }
                }
                Err(_) => println!("[doctor] {label}: present ({path})"),
            }
        } else {
            println!("[doctor] {label}: missing ({path})");
        }
    }

    for svc in ["thermald", "tlp", "auto-cpufreq"] {
        let output = std::process::Command::new("systemctl")
            .args(["is-active", svc])
            .output();
        match output {
            Ok(out) => {
                let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if state == "active" {
                    println!("[doctor] warning: {svc} is active (potential controller conflict)");
                } else {
                    println!("[doctor] {svc}: {state}");
                }
            }
            Err(_) => println!("[doctor] {svc}: unable to query"),
        }
    }

    println!("[doctor] done");
    Ok(())
}
