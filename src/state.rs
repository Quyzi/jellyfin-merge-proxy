//! Config-gauge publishing, background startup tasks and hot reload for
//! [`AppState`](crate::AppState). Split out of `lib.rs` to keep it small.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use metrics::{counter, gauge};

use crate::reload::ReloadContext;
use crate::telemetry::{self, names};
use crate::{dto, proxy, Shared};

impl crate::AppState {
    pub(crate) fn publish_config_gauges(&self) {
        let cfg = self.cfg();
        gauge!(names::CONFIG_GENERATION).set(self.generation.load(Ordering::Relaxed) as f64);
        gauge!(names::CONFIG_LAST_RELOAD).set(telemetry::unix_now());
        gauge!(names::CONFIG_UPSTREAMS)
            .set(cfg.upstreams.iter().filter(|u| u.enabled).count() as f64);
        gauge!(names::SERVERS_ACTIVE).set(self.backends.load().len() as f64);
        gauge!(names::CONFIG_USERS).set(cfg.users.len() as f64);
        if let Some(t) = &cfg.tls {
            if let Some(exp) = self.tls_provider.cert_expiry(&t.cert_path) {
                gauge!(names::TLS_CERT_EXPIRY, "cert" => "listener").set(exp as f64);
            }
        }
    }

    /// Log into all backends in the background (failures are only logged),
    /// and start the catalog refresher if `cache.refresh_interval_secs > 0`
    /// and the catalog index crawler.
    pub fn spawn_initial_logins(self: &Shared) {
        for u in self.backends().iter() {
            let u = u.clone();
            tokio::spawn(async move {
                if let Err(e) = u.ensure_login().await {
                    tracing::warn!(upstream = %u.name(), "initial login failed: {e}");
                }
            });
        }
        let weak = Arc::downgrade(self);
        self.index.spawn(weak.clone());
        crate::live::notify::spawn_userdata(weak.clone());
        crate::live::notify::spawn_library(weak.clone());
        let w = weak.clone();
        tokio::spawn(async move {
            loop {
                let Some(st) = w.upgrade() else { return };
                st.seeder.pass(&st).await;
                drop(st);
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        });
        tokio::spawn(async move {
            loop {
                let secs = match weak.upgrade() {
                    Some(st) => st.cfg().cache.refresh_interval_secs,
                    None => return,
                };
                tokio::time::sleep(std::time::Duration::from_secs(secs.max(5))).await;
                if secs == 0 {
                    continue;
                }
                let Some(st) = weak.upgrade() else { return };
                if st.cfg().cache.enabled {
                    proxy::refresh_catalog(&st).await;
                }
            }
        });
    }

    /// Re-load the config from its source and apply it through the reload
    /// hub. On any error the running config is kept unchanged.
    /// `files_changed` = referenced files (TLS) changed on disk.
    #[tracing::instrument(skip(self))]
    pub async fn reload(self: &Shared, files_changed: bool) -> anyhow::Result<bool> {
        let _g = self.reload_lock.lock().await;
        let r = self.reload_inner(files_changed).await;
        let outcome = match &r {
            Ok(true) => "success",
            Ok(false) => "unchanged",
            Err(_) => "error",
        };
        counter!(names::CONFIG_RELOADS, "outcome" => outcome).increment(1);
        *self.last_reload.lock().unwrap() =
            Some((dto::now_iso(), r.as_ref().err().map(|e| format!("{e:#}"))));
        if let Err(e) = &r {
            tracing::error!("config reload failed, keeping previous config: {e:#}");
        }
        r
    }

    async fn reload_inner(self: &Shared, files_changed: bool) -> anyhow::Result<bool> {
        let Some(src) = self.source.clone() else {
            anyhow::bail!("no config source to reload from");
        };
        // Hashing new passwords is CPU heavy; keep it off the async workers.
        let st = self.clone();
        let new = tokio::task::spawn_blocking(move || src.load(&st.password_cache)).await??;
        let old = self.cfg();
        if *old == new && !files_changed {
            return Ok(false);
        }
        let cx = ReloadContext {
            old: &old,
            new: &new,
            files_changed,
            tls: self.tls_provider.as_ref(),
        };
        self.hub.run(&cx).await?;
        *self.cfg.write().unwrap() = Arc::new(new);
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.publish_config_gauges();
        tracing::info!(
            generation = self.generation.load(Ordering::Relaxed),
            "config reloaded"
        );
        Ok(true)
    }
}
