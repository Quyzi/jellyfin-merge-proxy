//! `[tls]`, `[upstream_tls]` and per-server TLS settings.

use super::*;

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
