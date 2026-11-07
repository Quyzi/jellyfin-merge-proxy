//! TOML configuration model.
//!
//! Layout: `[proxy]` (the proxy's own listener/identity), `[[user]]`
//! (proxy logins), `[[server]]` (upstream Jellyfin servers, each with an
//! optional `[server.tls]`), `[merge]`, `[cache]`, `[tls]`, `[upstream_tls]`,
//! `[database]`, `[index]`, `[userdata]`, `[logging]`, `[metrics]`, `[debug]`.
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
    logging: LoggingConfig,
    #[serde(default)]
    metrics: MetricsConfig,
    #[serde(default)]
    debug: DebugConfig,
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

/// First run (no config file yet): try to write `config.example.toml` next to
/// the configured `path` if its directory is writable. If not writable, return
/// instructions telling the user to create the file manually. `None` when the
/// config exists.
pub fn first_run_help(path: &Path) -> Option<String> {
    // Check if path is a directory (Docker creates a dir when bind-mounting a missing file)
    if path.is_dir() {
        return Some(format!(
            "ERROR: config path {} is a directory, not a file.\n\
             This happens when Docker bind-mounts a non-existent file - Docker creates a directory instead.\n\
             Fix:\n\
             1. Stop the container\n\
             2. On the HOST, delete the directory that was created at the host bind-mount path\n\
             3. On the HOST, create the config file: touch <host-path>/jmp.toml\n\
             4. Make it writable by the container user (uid 65532, or 99:100 on Unraid with --user): chown 65532:65532 <host-path>/jmp.toml\n\
             5. Edit the file with your [[user]] and [[server]] entries (see config.example.toml)\n\
             6. Start the container again\n\
             \n\
             Alternatively, set JMP_CONFIG to point to a file inside a mounted directory volume instead of a single-file bind mount.",
            path.display()
        ));
    }

    if path.exists() {
        return None;
    }
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    let example_name = if path.file_name() == Some(std::ffi::OsStr::new("jmp.toml")) {
        "jmp.example.toml"
    } else {
        "config.example.toml"
    };
    let example = dir.join(example_name);

    let wrote = if example.exists() {
        Ok(false)
    } else {
        std::fs::create_dir_all(dir)
            .and_then(|_| std::fs::write(&example, EXAMPLE))
            .map(|_| true)
    };
    let mut msg = format!("no config file at {}.\n", path.display());
    match wrote {
        Ok(true) => msg += &format!("Wrote an annotated example to {}.\n", example.display()),
        Ok(false) => msg += &format!("An annotated example is at {}.\n", example.display()),
        Err(e) => {
            let (uid, gid) = (proc_status_id("Uid:"), proc_status_id("Gid:"));
            msg += &format!(
                "Could not write an example to {} ({e}). The config directory must be writable \
                 by this process (uid {uid}, gid {gid}); on the host run e.g. \
                 `chown -R {uid}:{gid} <the folder containing {}>`, or create the file from \
                 config.example.toml in the project repository.\n",
                example.display(),
                path.display()
            )
        }
    }
    msg += &format!(
        "Copy it to {} and edit it: [proxy], one [[user]] per person/TV and one [[server]] \
         per Jellyfin server. Plain HTTP is the default; TLS is optional. Then restart.",
        path.display()
    );
    Some(msg)
}

/// Effective uid/gid from /proc/self/status ("Uid:\t<real>\t<effective>...").
fn proc_status_id(key: &str) -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix(key))
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_string))
        })
        .unwrap_or_else(|| "?".into())
}

fn is_permission_error(e: &ConfigError) -> bool {
    matches!(e, ConfigError::Io { source, .. } if matches!(
        source.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
    ))
}

mod accounts;
mod database;
mod index;
mod load;
mod observability;
mod secret;
mod tls;
mod userdata;

pub use accounts::*;
pub use database::*;
pub use index::*;
pub use observability::*;
pub use secret::*;
pub use tls::*;
pub use userdata::*;

#[cfg(test)]
mod tests;
