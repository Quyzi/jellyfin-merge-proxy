//! Typed errors (thiserror) for every subsystem. Request handlers still use
//! `anyhow` at the very top to attach context, but every trait seam returns
//! one of these.

use std::path::PathBuf;
use thiserror::Error;

/// Problems loading or validating the TOML config.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("config: {0}")]
    Invalid(String),
    #[error("user '{user}': {source}")]
    Password {
        user: String,
        #[source]
        source: PasswordError,
    },
}

impl ConfigError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        ConfigError::Invalid(msg.into())
    }
}

/// Password hashing / PHC parsing failures.
#[derive(Debug, Error)]
pub enum PasswordError {
    #[error("invalid argon2 hash: {0}")]
    InvalidHash(String),
    #[error("not an argon2 hash")]
    NotArgon2,
    #[error("hashing failed: {0}")]
    Hash(String),
}

/// Proxy-user authentication failures. Deliberately coarse: the client
/// only ever sees "invalid username or password".
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    #[error("unknown user")]
    UnknownUser,
    #[error("bad password")]
    BadPassword,
}

impl AuthError {
    /// Bounded label for metrics.
    pub fn reason(&self) -> &'static str {
        match self {
            AuthError::UnknownUser => "unknown_user",
            AuthError::BadPassword => "bad_password",
        }
    }
}

/// TLS material / rustls configuration problems.
#[derive(Debug, Error)]
pub enum TlsError {
    #[error("reading {what} from {path}: {msg}")]
    Read {
        what: &'static str,
        path: PathBuf,
        msg: String,
    },
    #[error("no certificates found in {0}")]
    NoCertificates(PathBuf),
    #[error("unsupported private key: {0}")]
    UnsupportedKey(String),
    #[error("private key does not match certificate: {0}")]
    KeyMismatch(String),
    #[error("bad CA certificate in {path}: {msg}")]
    BadCa { path: PathBuf, msg: String },
    #[error("no trusted roots (system store empty and no ca_path)")]
    NoRoots,
    #[error("client_auth needs client_ca_path")]
    MissingClientCa,
    #[error("client_cert_path and client_key_path must be set together")]
    ClientCertPair,
    #[error("server_name '{name}': {msg}")]
    ServerName { name: String, msg: String },
    #[error("certificate verifier: {0}")]
    Verifier(String),
    #[error(transparent)]
    Rustls(#[from] rustls::Error),
}

/// Coarse transport failure class (also the `kind` metric label).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Timeout,
    Tls,
    Dns,
    Connect,
    Body,
    Other,
}

impl TransportKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransportKind::Timeout => "timeout",
            TransportKind::Tls => "tls",
            TransportKind::Dns => "dns",
            TransportKind::Connect => "connect",
            TransportKind::Body => "body",
            TransportKind::Other => "other",
        }
    }
}

/// Errors talking to a media backend (upstream server).
#[derive(Debug, Error)]
pub enum BackendError {
    #[error("upstream {server} is marked down")]
    Down { server: String },
    #[error("upstream {server} unreachable: {msg}")]
    Unreachable { server: String, msg: String },
    #[error("upstream {server} {kind}: {msg}", kind = .kind.as_str())]
    Transport {
        server: String,
        kind: TransportKind,
        msg: String,
    },
    #[error("upstream {server} login failed: {msg}")]
    Auth { server: String, msg: String },
    #[error("upstream {server}: invalid request: {msg}")]
    Request { server: String, msg: String },
    #[error("upstream {server}: bad response: {msg}")]
    Response { server: String, msg: String },
    #[error("upstream {server}: {source}")]
    Tls {
        server: String,
        #[source]
        source: TlsError,
    },
}

impl BackendError {
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            BackendError::Transport {
                kind: TransportKind::Timeout,
                ..
            }
        )
    }
    /// Bounded label for metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            BackendError::Down { .. } => "down",
            BackendError::Unreachable { .. } => "unreachable",
            BackendError::Transport { kind, .. } => kind.as_str(),
            BackendError::Auth { .. } => "auth",
            BackendError::Request { .. } => "request",
            BackendError::Response { .. } => "response",
            BackendError::Tls { .. } => "tls",
        }
    }
}

/// Errors raised by a reload subscriber.
#[derive(Debug, Error)]
pub enum ReloadError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("{what}: {source}")]
    Tls {
        what: String,
        #[source]
        source: TlsError,
    },
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("{0}")]
    Other(String),
}
