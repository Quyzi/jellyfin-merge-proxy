use serde::Deserialize;
use std::path::PathBuf;

fn default_true() -> bool {
    true
}

fn default_fanout_limit() -> usize {
    5000
}

fn default_dedupe_types() -> Vec<String> {
    vec!["Movie".into(), "Series".into()]
}

fn default_cache_ttl() -> u64 {
    60
}

fn default_cache_max() -> usize {
    1000
}

fn default_filter() -> String {
    "info".into()
}

fn default_metrics_path() -> String {
    "/metrics".into()
}

fn default_debug_path() -> String {
    "/jmp/debug".into()
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MergeConfig {
    #[serde(default = "default_true")]
    pub merge_libraries_by_name: bool,
    #[serde(default)]
    pub dedupe: bool,
    #[serde(default = "default_dedupe_types")]
    pub dedupe_types: Vec<String>,
    #[serde(default = "default_true")]
    pub label_colliding_libraries: bool,
    #[serde(default = "default_fanout_limit")]
    pub fanout_limit: usize,
}

impl Default for MergeConfig {
    fn default() -> Self {
        MergeConfig {
            merge_libraries_by_name: true,
            dedupe: false,
            dedupe_types: default_dedupe_types(),
            label_colliding_libraries: true,
            fanout_limit: default_fanout_limit(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_cache_ttl", alias = "ttl")]
    pub ttl_secs: u64,
    #[serde(default)]
    pub refresh_interval_secs: u64,
    #[serde(default = "default_cache_max")]
    pub max_entries: usize,
    #[serde(default)]
    pub persist_path: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub serve_stale_on_error: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        CacheConfig {
            enabled: true,
            ttl_secs: default_cache_ttl(),
            refresh_interval_secs: 0,
            max_entries: default_cache_max(),
            persist_path: None,
            serve_stale_on_error: true,
        }
    }
}

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
    #[serde(default = "default_filter")]
    pub filter: String,
    #[serde(default)]
    pub format: LogFormat,
    #[serde(default)]
    pub ansi: Option<bool>,
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

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_metrics_path")]
    pub path: String,
    #[serde(default)]
    pub listen: Option<String>,
    #[serde(default)]
    pub require_auth: bool,
    #[serde(default = "default_true")]
    pub process_metrics: bool,
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
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_debug_path")]
    pub path: String,
    #[serde(default)]
    pub users: Vec<String>,
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
