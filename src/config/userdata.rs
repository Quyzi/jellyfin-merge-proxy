//! `[userdata]` section: per-proxy-user watch state (see `crate::userdata`).

use super::*;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UserDataConfig {
    /// Keep watch state, favourites, ratings and resume points per proxy
    /// user in the proxy's database. When off, every proxy user sees the
    /// upstream accounts' state (shared).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Also send every change to the upstream copies (with the account the
    /// proxy user is mapped to), so the servers' own clients and features
    /// see it.
    #[serde(default = "default_true")]
    pub write_through: bool,
    /// Import each proxy user's state once per server from the upstream
    /// account it uses (shared or per-user), so upgrading keeps history.
    #[serde(default = "default_true")]
    pub seed_from_upstream: bool,
    /// Jellyfin's resume rules (Dashboard > Playback > Resume): stopping
    /// before `min_resume_pct` keeps no position, after `max_resume_pct`
    /// marks played, and items shorter than `min_resume_duration_secs`
    /// are never resumable.
    #[serde(default = "default_min_pct")]
    pub min_resume_pct: f64,
    #[serde(default = "default_max_pct")]
    pub max_resume_pct: f64,
    #[serde(default = "default_min_secs")]
    pub min_resume_duration_secs: u64,
}

fn default_min_pct() -> f64 {
    5.0
}

fn default_max_pct() -> f64 {
    90.0
}

fn default_min_secs() -> u64 {
    300
}

impl Default for UserDataConfig {
    fn default() -> Self {
        UserDataConfig {
            enabled: true,
            write_through: true,
            seed_from_upstream: true,
            min_resume_pct: default_min_pct(),
            max_resume_pct: default_max_pct(),
            min_resume_duration_secs: default_min_secs(),
        }
    }
}

impl UserDataConfig {
    pub(super) fn validate(&self) -> Result<(), String> {
        let ok = |p: f64| (0.0..=100.0).contains(&p);
        if !ok(self.min_resume_pct) || !ok(self.max_resume_pct) {
            return Err("[userdata] resume percentages must be between 0 and 100".into());
        }
        if self.min_resume_pct >= self.max_resume_pct {
            return Err("[userdata] min_resume_pct must be below max_resume_pct".into());
        }
        Ok(())
    }
}
