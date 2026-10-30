//! Logging setup and the runtime-reloadable log filter.

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

use tracing_subscriber::{
    layer::SubscriberExt, reload, util::SubscriberInitExt, EnvFilter, Layer, Registry,
};

/// Handle to change the log filter at runtime.
#[derive(Clone)]
pub struct LogHandle {
    handle: reload::Handle<EnvFilter, Registry>,
    env_override: bool,
}

impl LogHandle {
    pub fn set_filter(&self, filter: &str) -> anyhow::Result<()> {
        if self.env_override {
            tracing::debug!("RUST_LOG is set; ignoring logging.filter from config");
            return Ok(());
        }
        let f = EnvFilter::try_new(filter)?;
        self.handle.reload(f)?;
        Ok(())
    }
}

pub fn init_logging(cfg: &crate::config::LoggingConfig) -> LogHandle {
    use crate::config::LogFormat;
    let env = std::env::var("RUST_LOG").ok().filter(|s| !s.is_empty());
    let filter = env
        .as_deref()
        .and_then(|s| EnvFilter::try_new(s).ok())
        .unwrap_or_else(|| {
            EnvFilter::try_new(&cfg.filter).unwrap_or_else(|_| EnvFilter::new("info"))
        });
    let (filter_layer, handle) = reload::Layer::new(filter);
    let ansi = cfg.ansi.unwrap_or(cfg.format != LogFormat::Json);
    type L = Box<
        dyn Layer<tracing_subscriber::layer::Layered<reload::Layer<EnvFilter, Registry>, Registry>>
            + Send
            + Sync,
    >;
    let fmt_layer: L = match cfg.format {
        LogFormat::Pretty => tracing_subscriber::fmt::layer().with_ansi(ansi).boxed(),
        LogFormat::Compact => tracing_subscriber::fmt::layer()
            .compact()
            .with_ansi(ansi)
            .boxed(),
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .with_ansi(false)
            .boxed(),
    };
    let _ = tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .try_init();
    LogHandle {
        handle,
        env_override: env.is_some(),
    }
}
