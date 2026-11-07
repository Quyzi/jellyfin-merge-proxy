//! Per-proxy-user settings inside `[[user]]`: upstream accounts
//! (`[[user.account]]`), parental rating limit, session cap, geo allowlist.

use super::*;

/// One upstream account a proxy user acts as on one `[[server]]`.
/// Same credential keys as `[[server]]`: `user` + `password`, or
/// `api_key` (+ optional `user` / `user_id`). Secrets are encrypted in
/// place (`enc:v1:`) on start.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamAccount {
    /// The `[[server]]` name this account belongs to.
    pub server: String,
    #[serde(default, rename = "user", alias = "username")]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<Secret>,
    #[serde(default)]
    pub api_key: Option<Secret>,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// `max_parental_rating`: a Jellyfin rating score (`13`) or a rating name
/// (`"PG-13"`) resolved against the servers' rating tables.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum RatingLimit {
    Score(i64),
    Name(String),
}

impl UpstreamAccount {
    pub(super) fn decrypt(
        &mut self,
        user: &str,
        c: &dyn crate::secrets::SecretCipher,
    ) -> Result<(), ConfigError> {
        let fail = |kind: &'static str, e: ConfigError| {
            counter!(crate::telemetry::names::SECRET_DECRYPT_FAILURES, "kind" => kind).increment(1);
            tracing::error!(user, server = %self.server, "failed to decrypt {kind}: {e}");
            e
        };
        if let Some(p) = &self.password {
            let p = p.decrypt(c).map_err(|e| fail("user_account_password", e))?;
            self.password = Some(p);
        }
        if let Some(k) = &self.api_key {
            let k = k.decrypt(c).map_err(|e| fail("user_account_api_key", e))?;
            self.api_key = Some(k);
        }
        Ok(())
    }
}

/// Check every user's accounts against the configured servers.
pub(super) fn validate_accounts(cfg: &Config) -> Result<(), String> {
    let servers: std::collections::HashSet<String> = cfg
        .upstreams
        .iter()
        .map(|u| u.name.to_lowercase())
        .collect();
    for u in &cfg.users {
        let mut seen = std::collections::HashSet::new();
        for a in &u.accounts {
            let s = a.server.to_lowercase();
            if !servers.contains(&s) {
                return Err(format!(
                    "user '{}': account for unknown [[server]] '{}'",
                    u.name, a.server
                ));
            }
            if !seen.insert(s) {
                return Err(format!(
                    "user '{}': more than one account for server '{}'",
                    u.name, a.server
                ));
            }
            if a.api_key.is_none() && (a.username.is_none() || a.password.is_none()) {
                return Err(format!(
                    "user '{}': account for '{}' needs api_key or user + password",
                    u.name, a.server
                ));
            }
        }
        if let Some(RatingLimit::Name(n)) = &u.max_parental_rating {
            if n.trim().is_empty() {
                return Err(format!("user '{}': empty max_parental_rating", u.name));
            }
        }
        // Limits are judged against the catalog index; without it every
        // item of a restricted user would stay blocked.
        if (u.max_parental_rating.is_some() || u.block_unrated) && !cfg.index.enabled {
            return Err(format!(
                "user '{}': max_parental_rating / block_unrated need [index] enabled = true",
                u.name
            ));
        }
    }
    Ok(())
}

/// Uppercase ISO 3166-1 alpha-2 codes; rejects empties and duplicates.
pub(super) fn normalize_countries(user: &str, raw: &[String]) -> Result<Vec<String>, ConfigError> {
    let mut out = Vec::with_capacity(raw.len());
    let mut seen = std::collections::HashSet::new();
    for c in raw {
        let t = c.trim();
        if t.len() != 2 || !t.is_ascii() || !t.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(ConfigError::Invalid(format!(
                "user '{user}': allowed_countries entry '{c}' must be an ISO 3166-1 alpha-2 code"
            )));
        }
        let u = t.to_ascii_uppercase();
        if !seen.insert(u.clone()) {
            return Err(ConfigError::Invalid(format!(
                "user '{user}': duplicate allowed_countries entry '{u}'"
            )));
        }
        out.push(u);
    }
    Ok(out)
}
