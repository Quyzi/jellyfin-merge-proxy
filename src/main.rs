use std::path::PathBuf;

const USAGE: &str = "usage: jellyfin-merge-proxy [--config PATH] [--metrics-md] [--version]

  -c, --config PATH   config file (default: $JMP_CONFIG or /etc/jmp.toml)
      --metrics-md    print the METRICS.md metric reference and exit
  -V, --version       print version

Logging: [logging] in the config; RUST_LOG overrides the filter.
SIGHUP reloads the config (it is also polled every proxy.reload_interval_secs).";

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
        .unwrap_or_else(|| PathBuf::from("/etc/jmp.toml"));

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(path))
}

async fn run(path: PathBuf) -> anyhow::Result<()> {
    if let Some(help) = jmp::config::first_run_help(&path) {
        eprintln!("jellyfin-merge-proxy: {help}");
        std::process::exit(2);
    }

    // Set up logging from the file's [logging] section before loading the
    // config, so warnings emitted while loading (secret rewriting, key
    // permissions) are visible. The subscriber can only be installed once.
    let log = jmp::telemetry::init_logging(&jmp::config::LoggingConfig::peek(&path));

    let cache = jmp::config::PasswordCache::default();
    let cfg = match jmp::config::Config::load(&path, &cache) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(config = %path.display(), "failed to load config: {e:#}");
            std::process::exit(2);
        }
    };

    tracing::info!(version = env!("CARGO_PKG_VERSION"), git_sha = jmp::telemetry::git_sha(), config = %path.display(), "starting");
    let running = jmp::server::start(
        cfg,
        jmp::server::Options {
            config_path: Some(path),
            log: Some(log),
            password_cache: Some(cache),
            watch: true,
            ..Default::default()
        },
    )
    .await?;

    let _sighup = jmp::reload::spawn_watcher(
        Box::new(jmp::reload::SighupWatcher::new()),
        running.state.clone(),
    );

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
