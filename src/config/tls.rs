use serde::Deserialize;
use std::path::PathBuf;

fn default_true() -> bool {
    true
}

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

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    #[serde(default)]
    pub min_version: TlsVersion,
    #[serde(default)]
    pub client_auth: ClientAuth,
    #[serde(default)]
    pub client_ca_path: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub http2: bool,
    #[serde(default)]
    pub redirect_http_listen: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct UpstreamTlsConfig {
    #[serde(default)]
    pub ca_path: Option<PathBuf>,
    #[serde(default)]
    pub insecure_skip_verify: Option<bool>,
    #[serde(default)]
    pub client_cert_path: Option<PathBuf>,
    #[serde(default)]
    pub client_key_path: Option<PathBuf>,
    #[serde(default)]
    pub min_version: Option<TlsVersion>,
    #[serde(default)]
    pub server_name: Option<String>,
}

impl UpstreamTlsConfig {
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
