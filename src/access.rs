//! Per-user access limits: concurrent sessions and optional country allowlists.
//!
//! Session caps follow Jellyfin's `MaxActiveSessions` (0 = unlimited; new
//! login refused when at the cap — same-device re-login still replaces itself).
//! Geo uses a reverse-proxy country header (`[geo] country_header`) and fails
//! closed when a restricted user has no usable country.

use crate::config::{Config, UserConfig};
use crate::telemetry::names;
use crate::users::user_id_for;
use crate::Shared;
use axum::http::HeaderMap;
use metrics::counter;

/// Why a request / login was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    SessionLimit,
    /// Restricted user, header missing or not a country code.
    GeoMissing,
    /// Country present but not on the allowlist.
    GeoBlocked,
}

impl Denial {
    pub fn reason(self) -> &'static str {
        match self {
            Self::SessionLimit => "session_limit",
            Self::GeoMissing => "geo_missing",
            Self::GeoBlocked => "geo_blocked",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::SessionLimit => "Maximum number of active sessions reached.",
            Self::GeoMissing | Self::GeoBlocked => "Access from this location is not allowed.",
        }
    }

    fn count(self) {
        counter!(names::ACCESS_DENIED, "reason" => self.reason()).increment(1);
    }
}

pub fn user_by_id<'a>(cfg: &'a Config, user_id: &str) -> Option<&'a UserConfig> {
    cfg.users.iter().find(|u| user_id_for(&u.name) == user_id)
}

/// `None` / `0` = unlimited.
pub fn max_active_sessions(cfg: &Config, user_id: &str) -> u32 {
    user_by_id(cfg, user_id)
        .map(|u| u.max_active_sessions)
        .unwrap_or(0)
}

/// After same-device eviction: refuse if the user is already at the cap.
pub fn check_session_limit(st: &Shared, user_id: &str) -> Result<(), Denial> {
    let max = max_active_sessions(&st.cfg(), user_id);
    if max == 0 {
        return Ok(());
    }
    if st.sessions.count_for_user(user_id) >= max as usize {
        let d = Denial::SessionLimit;
        d.count();
        return Err(d);
    }
    Ok(())
}

/// ISO 3166-1 alpha-2 from the configured header, uppercased.
pub fn client_country(cfg: &Config, headers: &HeaderMap) -> Option<String> {
    let name = cfg.geo.header()?;
    let raw = headers.get(name)?.to_str().ok()?.trim();
    normalize_country(raw)
}

fn normalize_country(raw: &str) -> Option<String> {
    if raw.len() != 2 || !raw.is_ascii() {
        return None;
    }
    let upper = raw.to_ascii_uppercase();
    if !upper.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    // Cloudflare uses XX for unknown and T1 for Tor; treat as missing.
    if upper == "XX" || upper == "T1" {
        return None;
    }
    Some(upper)
}

/// No-op when the user has an empty allowlist.
pub fn check_geo(st: &Shared, user_id: &str, headers: &HeaderMap) -> Result<(), Denial> {
    let cfg = st.cfg();
    let Some(u) = user_by_id(&cfg, user_id) else {
        return Ok(());
    };
    if u.allowed_countries.is_empty() {
        return Ok(());
    }
    let Some(country) = client_country(&cfg, headers) else {
        let d = Denial::GeoMissing;
        d.count();
        return Err(d);
    };
    if u.allowed_countries.iter().any(|c| c == &country) {
        Ok(())
    } else {
        let d = Denial::GeoBlocked;
        d.count();
        Err(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GeoConfig;
    use crate::password::PasswordCache;
    use axum::http::HeaderValue;

    fn cfg_with_geo(header: &str) -> Config {
        let mut cfg = Config::parse(
            r#"
[[user]]
name = "a"
password = "pw"
[[server]]
name = "s"
server = "http://s:8096"
api_key = "k"
"#,
            &PasswordCache::default(),
        )
        .unwrap();
        cfg.geo = GeoConfig {
            country_header: Some(header.into()),
        };
        cfg
    }

    #[test]
    fn country_normalization() {
        assert_eq!(normalize_country("us"), Some("US".into()));
        assert_eq!(normalize_country("CA"), Some("CA".into()));
        assert_eq!(normalize_country("XX"), None);
        assert_eq!(normalize_country("T1"), None);
        assert_eq!(normalize_country("USA"), None);
        assert_eq!(normalize_country(""), None);
    }

    #[test]
    fn header_lookup() {
        let cfg = cfg_with_geo("CF-IPCountry");
        let mut h = HeaderMap::new();
        h.insert("cf-ipcountry", HeaderValue::from_static("de"));
        assert_eq!(client_country(&cfg, &h).as_deref(), Some("DE"));
    }
}
