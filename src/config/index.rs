//! `[index]` section: the canonical catalog index (see `crate::index`).

use super::*;

/// How copies of one item on several servers are ranked; the best copy is
/// the one played and whose metadata is shown.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PreferCopy {
    /// Lowest `[[server]] priority`, then config order.
    #[default]
    Priority,
    /// Highest video resolution, then priority.
    Resolution,
}

impl PreferCopy {
    pub fn as_str(self) -> &'static str {
        match self {
            PreferCopy::Priority => "priority",
            PreferCopy::Resolution => "resolution",
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IndexConfig {
    /// Answer library queries from the index (falls back to live fan-out
    /// while it is cold or for queries it cannot express).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Full re-crawl of every server this often.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Items per upstream request while crawling.
    #[serde(default = "default_page")]
    pub page_size: usize,
    #[serde(default)]
    pub prefer: PreferCopy,
}

fn default_refresh() -> u64 {
    600
}

fn default_page() -> usize {
    500
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig {
            enabled: true,
            refresh_interval_secs: default_refresh(),
            page_size: default_page(),
            prefer: PreferCopy::default(),
        }
    }
}

impl IndexConfig {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.refresh_interval_secs < 10 {
            return Err("[index] refresh_interval_secs must be at least 10".into());
        }
        if !(10..=10_000).contains(&self.page_size) {
            return Err("[index] page_size must be between 10 and 10000".into());
        }
        Ok(())
    }
}
