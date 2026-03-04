use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("nuc-powerd (bootstrap)");
        println!("Usage: nuc-powerd [--config <path>] [--dry-run]");
        return;
    }

    let dry_run = args.iter().any(|a| a == "--dry-run");
    let config_hint = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|idx| args.get(idx + 1))
        .map_or("config/nuc-powerd.example.toml", |s| s.as_str());

    println!("nuc-powerd bootstrap daemon");
    println!("  dry_run={dry_run}");
    println!("  config={config_hint}");
    println!("  status=not implemented");
}
