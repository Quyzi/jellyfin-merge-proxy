use serde::Deserialize;
use std::path::PathBuf;

use super::tls::UpstreamTlsConfig;
use super::types::Secret;

fn default_listen() -> String {
    "0.0.0.0:8096".into()
}
fn default_name() -> String {
    "Jellyfin Merge Proxy".into()
}
fn default_version() -> String {
    "10.10.7".into()
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("/var/lib/jmp")
}
fn default_unknown() -> UnknownRoutes {
    UnknownRoutes::Primary
}
fn default_timeout() -> u64 {
    30
}
fn default_reload() -> u64 {
    3
}
fn default_drain() -> u64 {
    300
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UnknownRoutes {
    Primary,
    Empty,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default = "default_version")]
    pub reported_version: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_unknown")]
    pub unknown_routes: UnknownRoutes,
    #[serde(default = "default_timeout")]
    pub upstream_timeout_secs: u64,
    #[serde(default = "default_reload")]
    pub reload_interval_secs: u64,
    #[serde(default = "default_drain")]
    pub drain_timeout_secs: u64,
    #[serde(default = "default_true")]
    pub rewrite_secrets: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        ProxyConfig {
            listen: default_listen(),
            name: default_name(),
            id: None,
            reported_version: default_version(),
            data_dir: default_data_dir(),
            unknown_routes: default_unknown(),
            upstream_timeout_secs: default_timeout(),
            reload_interval_secs: default_reload(),
            drain_timeout_secs: default_drain(),
            rewrite_secrets: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamConfig {
    pub name: String,
    #[serde(rename = "server", alias = "url")]
    pub url: String,
    #[serde(default)]
    pub api_key: Option<Secret>,
    #[serde(default, rename = "user", alias = "username")]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<Secret>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub tls: Option<UpstreamTlsConfig>,
}
