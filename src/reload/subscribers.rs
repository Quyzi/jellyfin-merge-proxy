//! The built-in [`Reloadable`] subscribers.

use super::*;

/// Hook that pushes a new rustls config into the running listener.
pub type TlsReloader = Box<dyn Fn(Arc<rustls::ServerConfig>) + Send + Sync>;

/// Hot-swaps the listener certificate.
#[derive(Default)]
pub struct ListenerTls {
    pub reloader: Mutex<Option<TlsReloader>>,
}

#[async_trait]
impl Reloadable for ListenerTls {
    fn reload_name(&self) -> String {
        "listener-tls".into()
    }
    fn validate(&self, cx: &ReloadContext<'_>) -> Result<(), ReloadError> {
        if let Some(t) = &cx.new.tls {
            if let Err(e) = cx.tls.listener(t) {
                counter!(names::TLS_RELOADS, "target" => "listener", "outcome" => "error")
                    .increment(1);
                return Err(ReloadError::Tls {
                    what: "[tls]".into(),
                    source: e,
                });
            }
        }
        Ok(())
    }
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        if cx.old.proxy.listen != cx.new.proxy.listen
            || cx.old.tls.is_some() != cx.new.tls.is_some()
        {
            tracing::warn!(
                "proxy.listen / enabling or disabling [tls] needs a restart; other changes applied"
            );
        }
        let Some(t) = &cx.new.tls else {
            return Ok(false);
        };
        if cx.old.tls.as_ref() == Some(t) && !cx.files_changed {
            return Ok(false);
        }
        let sc = cx.tls.listener(t).map_err(|e| ReloadError::Tls {
            what: "[tls]".into(),
            source: e,
        })?;
        let applied = self
            .reloader
            .lock()
            .unwrap()
            .as_ref()
            .map(|f| f(sc))
            .is_some();
        if applied {
            counter!(names::TLS_RELOADS, "target" => "listener", "outcome" => "success")
                .increment(1);
            tracing::info!("listener TLS certificate reloaded");
        }
        Ok(applied)
    }
}

/// Applies `[logging] filter` to the live subscriber.
pub struct LogFilter(pub Option<crate::telemetry::LogHandle>);

#[async_trait]
impl Reloadable for LogFilter {
    fn reload_name(&self) -> String {
        "log-filter".into()
    }
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        if cx.old.logging.format != cx.new.logging.format
            || cx.old.logging.ansi != cx.new.logging.ansi
        {
            tracing::warn!("logging.format / logging.ansi changes need a restart");
        }
        if cx.old.logging.filter == cx.new.logging.filter {
            return Ok(false);
        }
        if let Some(l) = &self.0 {
            l.set_filter(&cx.new.logging.filter)
                .map_err(|e| ReloadError::Other(format!("logging.filter: {e:#}")))?;
        }
        Ok(true)
    }
}

/// Warns about settings that only take effect after a restart.
pub struct RestartNotices;

#[async_trait]
impl Reloadable for RestartNotices {
    fn reload_name(&self) -> String {
        "restart-notices".into()
    }
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        let (o, n) = (cx.old, cx.new);
        if o.metrics.listen != n.metrics.listen
            || o.metrics.path != n.metrics.path
            || o.debug.path != n.debug.path
            || o.debug.enabled != n.debug.enabled
            || o.metrics.enabled != n.metrics.enabled
        {
            tracing::warn!("metrics/debug endpoint changes need a restart");
        }
        if o.cache.enabled != n.cache.enabled || o.cache.persist_path != n.cache.persist_path {
            tracing::warn!("cache.enabled / cache.persist_path changes need a restart");
        }
        if o.database != n.database {
            tracing::warn!("[database] changes need a restart");
        }
        if o.proxy.data_dir != n.proxy.data_dir {
            tracing::warn!("proxy.data_dir changes need a restart");
        }
        if o.proxy.name != n.proxy.name || o.proxy.id != n.proxy.id {
            tracing::warn!(
                "proxy.name / proxy.id (the server id clients see) change after a restart"
            );
        }
        // Adding/removing [[server]]s and URL changes are applied live by
        // `servers::BackendManager`.
        Ok(false)
    }
}

/// Re-tunes / invalidates the catalog cache.
pub struct CatalogCacheReload(pub Arc<dyn crate::cache::CatalogCache>);

#[async_trait]
impl Reloadable for CatalogCacheReload {
    fn reload_name(&self) -> String {
        "catalog-cache".into()
    }
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        let mut changed = false;
        if cx.old.cache != cx.new.cache {
            self.0.configure((&cx.new.cache).into());
            changed = true;
        }
        // Per-server entries are purged by `servers::BackendManager`.
        if cx.old.merge != cx.new.merge {
            self.0.invalidate_all();
            changed = true;
        }
        Ok(changed)
    }
}
