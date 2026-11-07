//! TOML configuration model.
//!
//! Layout: `[proxy]` (the proxy's own listener/identity), `[[user]]`
//! (proxy logins), `[[server]]` (upstream Jellyfin servers, each with an
//! optional `[server.tls]`), `[merge]`, `[cache]`, `[tls]`, `[upstream_tls]`,
//! `[database]`, `[index]`, `[userdata]`, `[geo]`, `[logging]`, `[metrics]`, `[debug]`.
//!
//! The file is deserialised into `Raw*` types and then converted into
//! [`Config`]. During conversion every proxy user password is normalised to
//! an argon2id PHC string ([`HashedPassword`]); the plaintext is zeroized and
//! never stored. Upstream credentials are wrapped in [`Secret`] whose `Debug`
//! impl redacts.

use crate::error::ConfigError;
use crate::secrets::KeyStore;
use metrics::counter;
use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

pub use crate::password::{HashedPassword, PasswordCache};

// ---------------------------------------------------------------------------
// Final config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub proxy: ProxyConfig,
    pub users: Vec<UserConfig>,
    pub upstreams: Vec<UpstreamConfig>,
    pub merge: MergeConfig,
    pub cache: CacheConfig,
    pub tls: Option<TlsConfig>,
    pub upstream_tls: UpstreamTlsConfig,
    pub database: DatabaseConfig,
    pub index: IndexConfig,
    pub userdata: UserDataConfig,
    pub geo: GeoConfig,
    pub logging: LoggingConfig,
    pub metrics: MetricsConfig,
    pub debug: DebugConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserConfig {
    pub name: String,
    pub password: HashedPassword,
    /// Upstream accounts this user acts as (`[[user.account]]`); servers
    /// without one use the server's shared account.
    pub accounts: Vec<UpstreamAccount>,
    /// Hide items rated above this (Jellyfin's MaxParentalRating).
    pub max_parental_rating: Option<RatingLimit>,
    /// With a limit set, also hide items that have no rating.
    pub block_unrated: bool,
    /// Cap on concurrent auth sessions (Jellyfin MaxActiveSessions).
    /// `0` = unlimited.
    pub max_active_sessions: u32,
    /// ISO 3166-1 alpha-2 allowlist; empty = any country.
    pub allowed_countries: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUserConfig {
    #[serde(alias = "username")]
    name: String,
    password: RawPassword,
    #[serde(default, rename = "account", alias = "accounts")]
    accounts: Vec<UpstreamAccount>,
    #[serde(default)]
    max_parental_rating: Option<RatingLimit>,
    #[serde(default)]
    block_unrated: bool,
    #[serde(default)]
    max_active_sessions: u32,
    #[serde(default)]
    allowed_countries: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default, alias = "listen")]
    proxy: ProxyConfig,
    #[serde(default, rename = "user", alias = "users")]
    users: Vec<RawUserConfig>,
    /// `[[server]]` entries are the upstream Jellyfin servers.
    #[serde(
        default,
        rename = "server",
        alias = "servers",
        alias = "upstream",
        alias = "upstreams"
    )]
    upstreams: Vec<UpstreamConfig>,
    #[serde(default)]
    merge: MergeConfig,
    #[serde(default)]
    cache: CacheConfig,
    #[serde(default)]
    tls: Option<TlsConfig>,
    #[serde(default)]
    upstream_tls: UpstreamTlsConfig,
    #[serde(default)]
    database: DatabaseConfig,
    #[serde(default)]
    index: IndexConfig,
    #[serde(default)]
    userdata: UserDataConfig,
    #[serde(default)]
    geo: GeoConfig,
    #[serde(default)]
    logging: LoggingConfig,
    #[serde(default)]
    metrics: MetricsConfig,
    #[serde(default)]
    debug: DebugConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
/// The proxy's own settings (`[proxy]`).
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
    /// Poll interval for config / certificate file changes (0 = only SIGHUP).
    #[serde(default = "default_reload")]
    pub reload_interval_secs: u64,
    /// How long a removed/rebuilt server may keep serving in-flight
    /// requests and streams before its upstream session is ended.
    #[serde(default = "default_drain")]
    pub drain_timeout_secs: u64,
    /// Rewrite plaintext secrets in the config file (hash user passwords,
    /// encrypt upstream passwords/API keys). Default true.
    #[serde(default = "default_true")]
    pub rewrite_secrets: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UnknownRoutes {
    Primary,
    Empty,
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

fn default_listen() -> String {
    "0.0.0.0:8096".into()
}
fn default_name() -> String {
    "Jellyfin Merge Proxy".into()
}
fn default_version() -> String {
    // Empty / omitted = highest upstream version (see system handlers).
    String::new()
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
fn default_fanout_limit() -> usize {
    5000
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
/// One upstream Jellyfin server (`[[server]]`).
pub struct UpstreamConfig {
    pub name: String,
    /// Base URL, e.g. `https://some-domain:8493` (TOML key `server`, alias `url`).
    #[serde(rename = "server", alias = "url")]
    pub url: String,
    #[serde(default)]
    pub api_key: Option<Secret>,
    /// Upstream login name (TOML key `user`, alias `username`).
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
    /// Per-upstream TLS settings; unset fields fall back to `[upstream_tls]`.
    #[serde(default)]
    pub tls: Option<UpstreamTlsConfig>,
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

fn default_dedupe_types() -> Vec<String> {
    vec!["Movie".into(), "Series".into()]
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

/// Catalog cache (`[cache]`): caches per-server library views and other
/// slow-changing catalog responses.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Entry lifetime; an expired entry is refreshed on next use.
    #[serde(default = "default_cache_ttl", alias = "ttl")]
    pub ttl_secs: u64,
    /// Proactive background refresh (0 = only refresh lazily on expiry).
    #[serde(default)]
    pub refresh_interval_secs: u64,
    /// Maximum cached entries; the oldest is evicted beyond this.
    #[serde(default = "default_cache_max")]
    pub max_entries: usize,
    /// Optional JSON file the cache is persisted to (survives restarts).
    #[serde(default)]
    pub persist_path: Option<PathBuf>,
    /// Serve an expired entry when the refresh fails.
    #[serde(default = "default_true")]
    pub serve_stale_on_error: bool,
}

fn default_cache_ttl() -> u64 {
    60
}
fn default_cache_max() -> usize {
    1000
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

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// The annotated example config shipped with the binary.
pub const EXAMPLE: &str = include_str!("../config.example.toml");

mod accounts;
mod database;
mod first_run;
mod geo;
mod index;
mod load;
mod observability;
mod secret;
mod tls;
mod userdata;

pub use accounts::*;
pub use database::*;
pub use first_run::first_run_help;
pub use geo::*;
pub use index::*;
pub use observability::*;
pub use secret::*;
pub use tls::*;
pub use userdata::*;

pub(crate) use first_run::is_permission_error;

#[cfg(test)]
mod tests;
