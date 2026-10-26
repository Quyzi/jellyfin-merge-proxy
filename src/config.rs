//! TOML configuration model.
//!
//! Layout: `[proxy]` (the proxy's own listener/identity), `[[user]]`
//! (proxy logins), `[[server]]` (upstream Jellyfin servers, each with an
//! optional `[server.tls]`), `[merge]`, `[cache]`, `[tls]`, `[upstream_tls]`,
//! `[logging]`, `[metrics]`, `[debug]`.
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

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

/// A secret string (upstream password / API key). Debug/Display redact.
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_encrypted(&self) -> bool {
        self.0.starts_with("enc:v1:")
    }
    /// Decrypt if encrypted, otherwise return plaintext.
    pub fn decrypt(
        &self,
        cipher: &dyn crate::secrets::SecretCipher,
    ) -> Result<Secret, ConfigError> {
        if self.is_encrypted() {
            Ok(Secret(cipher.decrypt(&self.0)?))
        } else {
            Ok(self.clone())
        }
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Raw password as written in the TOML file.
/// Accepted forms: `"plain"`, `{ plain = "..." }`, `{ plaintext = "..." }`,
/// `{ argon2 = "$argon2id$..." }`, or a bare `"$argon2..."` PHC string.
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum RawPassword {
    Tagged(TaggedPassword),
    Bare(String),
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaggedPassword {
    #[serde(alias = "plaintext")]
    Plain(String),
    Argon2(String),
}

impl fmt::Debug for RawPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RawPassword(***)")
    }
}

impl Drop for RawPassword {
    fn drop(&mut self) {
        match self {
            RawPassword::Bare(s)
            | RawPassword::Tagged(TaggedPassword::Plain(s))
            | RawPassword::Tagged(TaggedPassword::Argon2(s)) => s.zeroize(),
        }
    }
}

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
    pub logging: LoggingConfig,
    pub metrics: MetricsConfig,
    pub debug: DebugConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserConfig {
    pub name: String,
    pub password: HashedPassword,
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
// TLS
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
pub enum TlsVersion {
    #[default]
    #[serde(rename = "1.2")]
    V12,
    #[serde(rename = "1.3")]
    V13,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ClientAuth {
    #[default]
    None,
    Optional,
    Required,
}

/// Inbound (listener) TLS. Absent = plain HTTP.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM certificate chain (leaf first).
    pub cert_path: PathBuf,
    /// PEM private key (PKCS#8, PKCS#1/RSA or SEC1/EC).
    pub key_path: PathBuf,
    #[serde(default)]
    pub min_version: TlsVersion,
    #[serde(default)]
    pub client_auth: ClientAuth,
    /// PEM bundle of CAs trusted for client certificates (mTLS).
    #[serde(default)]
    pub client_ca_path: Option<PathBuf>,
    /// Offer HTTP/2 via ALPN (default true). HTTP/1.1 is always offered.
    #[serde(default = "default_true")]
    pub http2: bool,
    /// Optional plain-HTTP listener that redirects to HTTPS, e.g. "0.0.0.0:8080".
    #[serde(default)]
    pub redirect_http_listen: Option<String>,
}

/// Outbound TLS towards upstreams. All fields optional.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct UpstreamTlsConfig {
    /// Extra CA bundle (PEM) trusted in addition to the system roots.
    #[serde(default)]
    pub ca_path: Option<PathBuf>,
    /// Disable certificate verification. Dangerous.
    #[serde(default)]
    pub insecure_skip_verify: Option<bool>,
    /// Client certificate chain + key (PEM) for mTLS to the upstream.
    #[serde(default)]
    pub client_cert_path: Option<PathBuf>,
    #[serde(default)]
    pub client_key_path: Option<PathBuf>,
    #[serde(default)]
    pub min_version: Option<TlsVersion>,
    /// Name the upstream certificate is verified against (instead of the URL host).
    #[serde(default)]
    pub server_name: Option<String>,
}

impl UpstreamTlsConfig {
    /// Per-upstream settings layered over the global defaults.
    pub fn merged(
        global: &UpstreamTlsConfig,
        local: Option<&UpstreamTlsConfig>,
    ) -> UpstreamTlsConfig {
        let Some(l) = local else {
            return global.clone();
        };
        UpstreamTlsConfig {
            ca_path: l.ca_path.clone().or_else(|| global.ca_path.clone()),
            insecure_skip_verify: l.insecure_skip_verify.or(global.insecure_skip_verify),
            client_cert_path: l
                .client_cert_path
                .clone()
                .or_else(|| global.client_cert_path.clone()),
            client_key_path: l
                .client_key_path
                .clone()
                .or_else(|| global.client_key_path.clone()),
            min_version: l.min_version.or(global.min_version),
            server_name: l.server_name.clone().or_else(|| global.server_name.clone()),
        }
    }
}

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
             2. Delete the directory: rm -rf {}\n\
             3. Create the config file on the host: touch {} && chmod 644 {}\n\
             4. Edit the file with your [[user]] and [[server]] entries (see config.example.toml)\n\
             5. Start the container again\n\
             \n\
             Alternatively, set JMP_CONFIG to point to a file inside a mounted directory volume instead of a single-file bind mount.",
            path.display(), path.display(), path.display(), path.display()
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

impl Config {
    pub fn load(path: &Path, cache: &PasswordCache) -> Result<Config, ConfigError> {
        let mut text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        // Parse to get the data_dir (needed for key loading)
        let raw: RawConfig = match toml::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                text.zeroize();
                // Friendlier message for the pre-[[server]] layout.
                if let Ok(v) = text.parse::<toml::Table>() {
                    if v.get("server").map(|s| s.is_table()) == Some(true) {
                        return Err(ConfigError::invalid(
                            "`[server]` is now `[proxy]`; `[[server]]` (double brackets) lists the upstream Jellyfin servers",
                        ));
                    }
                }
                return Err(e.into());
            }
        };

        let data_dir = raw.proxy.data_dir.clone();

        // Load encryption key
        let key_store = crate::secrets::DefaultKeyStore;
        let key = key_store.load(&data_dir)?;
        let cipher = crate::secrets::XChaCha20Cipher::new(key);

        // Rewrite secrets if enabled and writable
        if raw.proxy.rewrite_secrets {
            match crate::config_rewrite::rewrite_secrets(path, cache, &cipher) {
                Ok(true) => {
                    tracing::info!(path = %path.display(), "rewrote plaintext secrets in config file");
                }
                Ok(false) => {}
                Err(e) if is_permission_error(&e) => {
                    counter!(crate::telemetry::names::SECRET_REWRITE_FAILURES).increment(1);
                    tracing::warn!(
                        path = %path.display(),
                        "config file not writable; secrets remain plaintext on disk: {e}"
                    );
                }
                Err(e) => {
                    counter!(crate::telemetry::names::SECRET_REWRITE_FAILURES).increment(1);
                    text.zeroize();
                    return Err(e);
                }
            }
        }

        let r = Self::parse_with_cipher(&text, cache, Some(&cipher));
        text.zeroize();
        r
    }

    pub fn parse(text: &str, cache: &PasswordCache) -> Result<Config, ConfigError> {
        Self::parse_with_cipher(text, cache, None)
    }

    fn parse_with_cipher(
        text: &str,
        cache: &PasswordCache,
        cipher: Option<&dyn crate::secrets::SecretCipher>,
    ) -> Result<Config, ConfigError> {
        let raw: RawConfig = match toml::from_str(text) {
            Ok(r) => r,
            Err(e) => {
                // Friendlier message for the pre-[[server]] layout.
                if let Ok(v) = text.parse::<toml::Table>() {
                    if v.get("server").map(|s| s.is_table()) == Some(true) {
                        return Err(ConfigError::invalid(
                            "`[server]` is now `[proxy]`; `[[server]]` (double brackets) lists the upstream Jellyfin servers",
                        ));
                    }
                }
                return Err(e.into());
            }
        };
        let mut users = Vec::with_capacity(raw.users.len());
        for u in &raw.users {
            let password =
                cache
                    .normalize(&u.name, &u.password)
                    .map_err(|e| ConfigError::Password {
                        user: u.name.clone(),
                        source: e,
                    })?;
            users.push(UserConfig {
                name: u.name.clone(),
                password,
            });
        }

        // Decrypt upstream secrets
        let mut upstreams = raw.upstreams.clone();
        if let Some(c) = cipher {
            for upstream in &mut upstreams {
                if let Some(password) = &upstream.password {
                    upstream.password = Some(password.decrypt(c).map_err(|e| {
                        counter!(crate::telemetry::names::SECRET_DECRYPT_FAILURES, "kind" => "upstream_password").increment(1);
                        tracing::error!(upstream = %upstream.name, "failed to decrypt upstream password: {e}");
                        e
                    })?);
                }
                if let Some(api_key) = &upstream.api_key {
                    upstream.api_key = Some(api_key.decrypt(c).map_err(|e| {
                        counter!(crate::telemetry::names::SECRET_DECRYPT_FAILURES, "kind" => "upstream_api_key").increment(1);
                        tracing::error!(upstream = %upstream.name, "failed to decrypt upstream api_key: {e}");
                        e
                    })?);
                }
            }
        }

        // `raw` (and with it every plaintext) is dropped + zeroized here.
        let cfg = Config {
            proxy: raw.proxy.clone(),
            users,
            upstreams,
            merge: raw.merge.clone(),
            cache: raw.cache.clone(),
            tls: raw.tls.clone(),
            upstream_tls: raw.upstream_tls.clone(),
            logging: raw.logging.clone(),
            metrics: raw.metrics.clone(),
            debug: raw.debug.clone(),
        };
        drop(raw);
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        if self.users.is_empty() {
            return bad("at least one [[user]] is required".into());
        }
        let mut seen = std::collections::HashSet::new();
        for u in &self.users {
            if !seen.insert(u.name.to_lowercase()) {
                return bad(format!("duplicate user '{}'", u.name));
            }
        }
        if self.upstreams.iter().filter(|u| u.enabled).count() == 0 {
            return bad("at least one enabled [[server]] is required".into());
        }
        let mut names = std::collections::HashSet::new();
        for u in &self.upstreams {
            if !names.insert(u.name.to_lowercase()) {
                return bad(format!("duplicate [[server]] name '{}'", u.name));
            }
            if u.api_key.is_none() && u.username.is_none() {
                return bad(format!(
                    "[[server]] '{}' needs api_key or user/password",
                    u.name
                ));
            }
            if u.api_key.is_none() && u.password.is_none() {
                return bad(format!(
                    "[[server]] '{}' has a user but no password",
                    u.name
                ));
            }
            url::Url::parse(&u.url).map_err(|e| {
                ConfigError::Invalid(format!("[[server]] '{}' server url: {e}", u.name))
            })?;
        }
        if let Some(id) = &self.proxy.id {
            if crate::ids::normalize_hex_id(id).is_none() {
                return bad("proxy.id must be 32 hex characters".into());
            }
        }
        if let Some(t) = &self.tls {
            if t.client_auth != ClientAuth::None && t.client_ca_path.is_none() {
                return bad("tls.client_auth requires tls.client_ca_path".into());
            }
        }
        tracing_subscriber::EnvFilter::try_new(&self.logging.filter)
            .map_err(|e| ConfigError::Invalid(format!("logging.filter: {e}")))?;
        if !self.metrics.path.starts_with('/') || !self.debug.path.starts_with('/') {
            return bad("metrics.path / debug.path must start with '/'".into());
        }
        if self.cache.enabled && self.cache.max_entries == 0 {
            return bad("cache.max_entries must be > 0 (or set cache.enabled = false)".into());
        }
        Ok(())
    }

    /// Every file the running config depends on (for change detection).
    pub fn watched_files(&self) -> Vec<PathBuf> {
        let mut v = vec![];
        if let Some(t) = &self.tls {
            v.push(t.cert_path.clone());
            v.push(t.key_path.clone());
            v.extend(t.client_ca_path.clone());
        }
        let mut add = |t: &UpstreamTlsConfig| {
            v.extend(t.ca_path.clone());
            v.extend(t.client_cert_path.clone());
            v.extend(t.client_key_path.clone());
        };
        add(&self.upstream_tls);
        for u in &self.upstreams {
            if let Some(t) = &u.tls {
                add(t);
            }
        }
        v.sort();
        v.dedup();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_writes_example_once_and_never_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("sub/jmp.toml");
        let msg = first_run_help(&cfg).unwrap();
        assert!(msg.contains("Wrote an annotated example"), "{msg}");
        let example = dir.path().join("sub/jmp.example.toml");
        assert_eq!(std::fs::read_to_string(&example).unwrap(), EXAMPLE);
        assert!(!cfg.exists());
        // Existing example is left alone.
        std::fs::write(&example, "# mine").unwrap();
        let msg = first_run_help(&cfg).unwrap();
        assert!(msg.contains("An annotated example is at"), "{msg}");
        assert_eq!(std::fs::read_to_string(&example).unwrap(), "# mine");
        std::fs::write(&cfg, "").unwrap();
        assert!(first_run_help(&cfg).is_none());
    }

    #[test]
    fn parses_example() {
        let cache = PasswordCache::default();
        let cfg = Config::parse(include_str!("../config.example.toml"), &cache).unwrap();
        assert_eq!(cfg.upstreams.len(), 2);
        assert_eq!(cfg.upstreams[0].name, "gummybear");
        assert_eq!(cfg.upstreams[0].url, "https://some-domain:8493");
        assert!(!cfg.users.is_empty());
        assert!(cfg.tls.is_none());
        assert!(cfg.cache.enabled);
    }

    /// The exact upstream shape from the requirements must parse as-is.
    #[test]
    fn parses_exact_server_snippet() {
        const SNIPPET: &str = r#"
[[server]]
name = "gummybear"
server = "https://some-domain:8493"
user = "gummybear"
password = "meow"
"#;
        // Raw: the snippet alone deserialises.
        let raw: RawConfig = toml::from_str(SNIPPET).unwrap();
        assert_eq!(raw.upstreams.len(), 1);
        // Full config: plus a proxy login.
        let cache = PasswordCache::default();
        let text = format!("{SNIPPET}\n[[user]]\nname = \"me\"\npassword = \"pw\"\n");
        let cfg = Config::parse(&text, &cache).unwrap();
        let s = &cfg.upstreams[0];
        assert_eq!(s.name, "gummybear");
        assert_eq!(s.url, "https://some-domain:8493");
        assert_eq!(s.username.as_deref(), Some("gummybear"));
        assert_eq!(s.password.as_ref().unwrap().expose(), "meow");
        assert!(s.api_key.is_none() && s.tls.is_none() && s.enabled);
        assert_eq!(cfg.proxy, ProxyConfig::default());
        assert_eq!(cfg.cache, CacheConfig::default());
        assert!(!format!("{cfg:?}").contains("meow"));
    }

    #[test]
    fn server_tls_subtable_aliases_and_old_layout_hint() {
        let cache = PasswordCache::default();
        let text = r#"
[proxy]
listen = "127.0.0.1:1"
[[user]]
name = "me"
password = "pw"
[[server]]
name = "a"
server = "https://a:8920"
api_key = "k"
[server.tls]
insecure_skip_verify = true
[[server]]
name = "b"
url = "http://b:8096"
username = "u"
password = "p"
[cache]
ttl_secs = 5
max_entries = 10
persist_path = "/tmp/x.json"
"#;
        let cfg = Config::parse(text, &cache).unwrap();
        assert_eq!(cfg.upstreams.len(), 2);
        assert_eq!(
            cfg.upstreams[0].tls.as_ref().unwrap().insecure_skip_verify,
            Some(true)
        );
        assert_eq!(cfg.upstreams[1].username.as_deref(), Some("u"));
        assert_eq!(cfg.cache.ttl_secs, 5);
        assert_eq!(cfg.proxy.listen, "127.0.0.1:1");
        // `[[upstream]]` is still accepted as an alias for `[[server]]`.
        let alias = "[[user]]\nname = \"me\"\npassword = \"pw\"\n[[upstream]]\nname = \"b\"\nserver = \"http://b:8096\"\nuser = \"u\"\npassword = \"p\"\n";
        let cfg = Config::parse(alias, &cache).unwrap();
        assert_eq!(cfg.upstreams[0].name, "b");
        let old = "[server]\nlisten = \"x\"\n";
        let e = Config::parse(old, &cache).unwrap_err().to_string();
        assert!(e.contains("[proxy]"), "{e}");
    }

    const MINI: &str = r#"
[[user]]
name = "a"
password = "pw"
[[user]]
name = "b"
password = { plain = "pw2" }
[[server]]
name = "x"
server = "http://x:8096"
api_key = "k"
"#;

    #[test]
    fn plaintext_passwords_are_hashed_and_cached() {
        let cache = PasswordCache::default();
        let cfg = Config::parse(MINI, &cache).unwrap();
        for u in &cfg.users {
            assert!(
                u.password.phc().starts_with("$argon2id$"),
                "stored as argon2"
            );
            // (A substring check would flake: base64 salt/hash may contain "pw".)
            assert!(u
                .password
                .phc()
                .split('$')
                .all(|seg| seg != "pw" && seg != "pw2"));
        }
        assert!(cfg.users[0].password.verify("pw"));
        assert!(!cfg.users[0].password.verify("nope"));
        assert!(cfg.users[1].password.verify("pw2"));
        // Debug output never contains secrets
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("pw2") && !dbg.contains("\"k\""));
        // Reload: unchanged users reuse the cached hash (no re-hash, same salt)
        assert_eq!(cache.len(), 2);
        let cfg2 = Config::parse(MINI, &cache).unwrap();
        assert_eq!(cfg.users[0].password, cfg2.users[0].password);
        assert_eq!(cache.len(), 2);
        // Changed password -> new hash
        let cfg3 = Config::parse(&MINI.replace("\"pw\"", "\"pw-new\""), &cache).unwrap();
        assert_ne!(cfg.users[0].password, cfg3.users[0].password);
        assert!(cfg3.users[0].password.verify("pw-new"));
    }

    #[test]
    fn argon2_passthrough_and_validation() {
        let h = HashedPassword::hash("secret").unwrap();
        let cache = PasswordCache::default();
        let toml_bare = MINI.replace("\"pw\"", &format!("'{}'", h.phc()));
        let cfg = Config::parse(&toml_bare, &cache).unwrap();
        assert_eq!(
            cfg.users[0].password, h,
            "existing hash passes through unchanged"
        );
        let toml_tagged = MINI.replace("\"pw\"", &format!("{{ argon2 = '{}' }}", h.phc()));
        let cfg = Config::parse(&toml_tagged, &cache).unwrap();
        assert!(cfg.users[0].password.verify("secret"));
        let bad = MINI.replace("\"pw\"", "{ argon2 = 'not-a-hash' }");
        assert!(Config::parse(&bad, &cache).is_err());
    }
}
