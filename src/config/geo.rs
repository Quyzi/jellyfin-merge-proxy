//! Client geolocation for per-user country allowlists.
//!
//! JMP does not look up IP addresses itself. A reverse proxy (Cloudflare,
//! nginx `geo`, …) sets a country header; `[geo] country_header` names it.
//! Required when any `[[user]]` sets `allowed_countries`.

use serde::Deserialize;

/// How the proxy learns the client's country (`[geo]`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct GeoConfig {
    /// HTTP header carrying an ISO 3166-1 alpha-2 country code
    /// (`CF-IPCountry`, `X-Country-Code`, …). Empty / omitted = geo rules
    /// cannot be used (validated when any user has `allowed_countries`).
    #[serde(default)]
    pub country_header: Option<String>,
}

impl GeoConfig {
    pub(super) fn validate(&self, users_need_geo: bool) -> Result<(), String> {
        let header = self.header();
        if self
            .country_header
            .as_ref()
            .is_some_and(|h| h.trim().is_empty())
        {
            return Err("[geo] country_header is empty".into());
        }
        if users_need_geo && header.is_none() {
            return Err(
                "allowed_countries requires [geo] country_header (e.g. \"CF-IPCountry\")".into(),
            );
        }
        Ok(())
    }

    /// Trimmed header name, if configured.
    pub fn header(&self) -> Option<&str> {
        self.country_header
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
    }
}
