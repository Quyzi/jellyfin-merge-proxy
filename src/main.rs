use std::path::PathBuf;

const USAGE: &str = "usage: jellyfin-merge-proxy [--config PATH] [--metrics-md] [--version]

  -c, --config PATH   config file (default: $JMP_CONFIG or /config/config.toml)
      --metrics-md    print the METRICS.md metric reference and exit
  -V, --version       print version";

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-c" | "--config" => path = args.next().map(PathBuf::from),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!(
                    "jellyfin-merge-proxy {} ({})",
                    env!("CARGO_PKG_VERSION"),
                    jmp::telemetry::git_sha()
                );
                return Ok(());
            }
            "--metrics-md" => {
                print!("{}", jmp::telemetry::metrics_markdown());
                return Ok(());
            }
            other if !other.starts_with('-') && path.is_none() => path = Some(PathBuf::from(other)),
            other => anyhow::bail!("unknown argument: {other}\n\n{USAGE}"),
        }
    }
    let path = path
        .or_else(|| std::env::var_os("JMP_CONFIG").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/config/config.toml"));

    let cache = jmp::config::PasswordCache::default();
    match jmp::config::Config::load(&path, &cache) {
        Ok(cfg) => {
            println!(
                "config {} OK: {} server(s), {} user(s)",
                path.display(),
                cfg.upstreams.len(),
                cfg.users.len()
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("error: config {}: {e:#}", path.display());
            std::process::exit(2);
        }
    }
}
