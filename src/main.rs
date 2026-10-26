use std::path::PathBuf;

const USAGE: &str = "usage: jellyfin-merge-proxy [--config PATH] [--metrics-md] [--version]

  -c, --config PATH   config file (default: $JMP_CONFIG or /config/config.toml)
      --metrics-md    print the METRICS.md metric reference and exit
  -V, --version       print version

Logging: [logging] in the config; RUST_LOG overrides the filter.";

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

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(path))
}

async fn run(path: PathBuf) -> anyhow::Result<()> {
    let cache = jmp::config::PasswordCache::default();
    let cfg = match jmp::config::Config::load(&path, &cache) {
        Ok(c) => c,
        Err(e) => {
            // Logging is not configured yet: print plainly.
            eprintln!("error: config {}: {e:#}", path.display());
            std::process::exit(2);
        }
    };
    let _log = jmp::telemetry::init_logging(&cfg.logging);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), git_sha = jmp::telemetry::git_sha(), config = %path.display(), "starting");
    let running = jmp::server::start(
        cfg,
        jmp::server::Options {
            password_cache: Some(cache),
            ..Default::default()
        },
    )
    .await?;

    shutdown_signal().await;
    running.shutdown();
    running.wait().await
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutting down");
}
