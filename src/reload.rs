//! Reacting to config changes: a [`Reloadable`] component first
//! `validate`s a new config, then `apply`s it ([`ReloadContext`] carries
//! the old and new config). Validation must not change any state.

use crate::config::Config;
use crate::error::ReloadError;
use crate::tls::TlsProvider;
use async_trait::async_trait;

/// Everything a subscriber needs to react to a reload.
pub struct ReloadContext<'a> {
    pub old: &'a Config,
    pub new: &'a Config,
    /// Referenced files (certificates, keys) changed on disk.
    pub files_changed: bool,
    pub tls: &'a dyn TlsProvider,
}

/// A component that reacts to config changes.
#[async_trait]
pub trait Reloadable: Send + Sync {
    /// Name for logs (e.g. "backend:home", "listener-tls").
    fn reload_name(&self) -> String;
    /// Check the new config can be applied. Must not change state.
    fn validate(&self, _cx: &ReloadContext<'_>) -> Result<(), ReloadError> {
        Ok(())
    }
    /// Apply the new config. Returns true if anything changed.
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError>;
}
