//! `[logging]`, `[metrics]` and `[debug]` sections.

use super::*;

// ---------------------------------------------------------------------------
// Logging / metrics / debug
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Pretty,
    Compact,
    Json,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// EnvFilter directive, e.g. "info" or "info,jmp::upstream=debug".
    /// RUST_LOG overrides it. Hot-reloadable.
    #[serde(default = "default_filter")]
    pub filter: String,
    /// Output format (restart required to change).
    #[serde(default)]
    pub format: LogFormat,
    /// ANSI colours (restart required). Default: on for pretty/compact.
    #[serde(default)]
    pub ansi: Option<bool>,
}

fn default_filter() -> String {
    "info".into()
}

impl Default for LoggingConfig {
    fn default() -> Self {
        LoggingConfig {
            filter: default_filter(),
            format: LogFormat::default(),
            ansi: None,
        }
    }
}

impl LoggingConfig {
    /// Best-effort read of just the `[logging]` table of the config file, so
    /// logging can be set up before [`Config::load`] runs (which may log
    /// warnings, e.g. while rewriting secrets). Falls back to defaults when
    /// the file or the section cannot be parsed; `Config::load` reports the
    /// real error afterwards.
    pub fn peek(path: &Path) -> LoggingConfig {
        #[derive(Deserialize)]
        struct OnlyLogging {
            #[serde(default)]
            logging: LoggingConfig,
        }
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str::<OnlyLogging>(&text).ok())
            .map(|o| o.logging)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Path on the main listener.
    #[serde(default = "default_metrics_path")]
    pub path: String,
    /// Optional separate plain-HTTP listener (e.g. "0.0.0.0:9090"). When set,
    /// metrics are served only there.
    #[serde(default)]
    pub listen: Option<String>,
    /// Require a proxy access token to scrape (default false).
    #[serde(default)]
    pub require_auth: bool,
    /// Also export process metrics (cpu, memory, fds).
    #[serde(default = "default_true")]
    pub process_metrics: bool,
}

fn default_metrics_path() -> String {
    "/metrics".into()
}

impl Default for MetricsConfig {
    fn default() -> Self {
        MetricsConfig {
            enabled: true,
            path: default_metrics_path(),
            listen: None,
            require_auth: false,
            process_metrics: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DebugConfig {
    /// Expose a JSON state dump (requires a proxy login token). Default off.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_debug_path")]
    pub path: String,
    /// Proxy users allowed to read it (empty = any logged-in proxy user).
    #[serde(default)]
    pub users: Vec<String>,
}

fn default_debug_path() -> String {
    "/jmp/debug".into()
}

impl Default for DebugConfig {
    fn default() -> Self {
        DebugConfig {
            enabled: false,
            path: default_debug_path(),
            users: vec![],
        }
    }
}
