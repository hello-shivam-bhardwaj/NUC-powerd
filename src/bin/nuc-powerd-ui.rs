use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use nuc_powerd::api::{spawn_ui_server, UiServerConfig};
use nuc_powerd::config::load_config;
use nuc_powerd::controller::target_temp_bounds;

#[derive(Parser, Debug)]
#[command(name = "nuc-powerd-ui")]
#[command(about = "Local web/API UI for nuc-powerd")]
struct Cli {
    #[arg(long, default_value = "config/nuc-powerd.example.toml")]
    config: std::path::PathBuf,
}

fn main() {
    if let Err(err) = run_main() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run_main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = load_config(&cli.config).context("failed loading config")?;

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .context("failed setting signal handler")?;

    let handle = spawn_ui_server(
        {
            let (target_min_c, target_max_c) = target_temp_bounds(&cfg.hysteresis);
            UiServerConfig {
                bind: cfg.daemon.api_bind.clone(),
                status_path: cfg.daemon.status_path.clone(),
                control_path: cfg.daemon.control_path.clone(),
                target_min_c,
                target_max_c,
                service_unit: cfg.daemon.service_unit.clone(),
                stress_program: cfg.daemon.stress_program.clone(),
            }
        },
        running.clone(),
    )
    .context("failed starting ui server")?;

    println!("starting nuc-powerd-ui on {}", cfg.daemon.api_bind);
    while running.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    let _ = handle.join();
    Ok(())
}
