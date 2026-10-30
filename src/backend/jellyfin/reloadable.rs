//! Hot reload of one Jellyfin backend (credentials, TLS, timeouts).

use super::*;

#[async_trait]
impl Reloadable for JellyfinBackend {
    fn reload_name(&self) -> String {
        format!("backend:{}", self.name)
    }

    fn validate(&self, cx: &ReloadContext<'_>) -> Result<(), ReloadError> {
        let Some(nc) = own_config(cx.new, &self.name) else {
            return Ok(());
        };
        let tls = UpstreamTlsConfig::merged(&cx.new.upstream_tls, nc.tls.as_ref());
        cx.tls
            .upstream(&tls, &self.name)
            .map_err(|e| ReloadError::Tls {
                what: format!("[[server]] '{}' TLS", self.name),
                source: e,
            })?;
        Ok(())
    }

    /// Rebuilds the HTTP clients if TLS/timeout changed (or TLS files
    /// changed on disk); drops the cached token if the credentials changed.
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        let Some(nc) = own_config(cx.new, &self.name) else {
            return Ok(false);
        };
        let dropped = self.accounts.reload(&self.name, cx.new);
        let accounts_changed = !dropped.is_empty() || cx.old.users != cx.new.users;
        self.logout_accounts(dropped).await;
        let oc = own_config(cx.old, &self.name);
        let tls = UpstreamTlsConfig::merged(&cx.new.upstream_tls, nc.tls.as_ref());
        let timeout = timeout_of(cx.new);
        let old = self.cfg.read().unwrap().clone();
        let tls_changed =
            tls != *self.tls.read().unwrap() || timeout != *self.timeout.read().unwrap();
        if oc == Some(nc) && !tls_changed && !cx.files_changed {
            return Ok(accounts_changed);
        }
        if oc != Some(nc) {
            counter!(names::CONFIG_UPSTREAM_CHANGES, "change" => "changed").increment(1);
        }
        let priority = nc.priority.unwrap_or(self.id.0 as i64);
        if self
            .priority
            .swap(priority, std::sync::atomic::Ordering::Relaxed)
            != priority
        {
            tracing::info!(upstream = %self.name, priority, "priority changed");
        }
        let t = match build_transport(&self.name, &tls, timeout, &self.location.base_path, cx.tls) {
            Ok(t) => {
                counter!(names::TLS_RELOADS, "target" => "upstream", "outcome" => "success")
                    .increment(1);
                t
            }
            Err(e) => {
                counter!(names::TLS_RELOADS, "target" => "upstream", "outcome" => "error")
                    .increment(1);
                return Err(e.into());
            }
        };
        self.set_transport(t);
        record_client_cert_expiry(&self.name, &tls, cx.tls);
        *self.tls.write().unwrap() = tls;
        *self.timeout.write().unwrap() = timeout;
        let creds_changed = old.api_key != nc.api_key
            || old.username != nc.username
            || old.password != nc.password
            || old.user_id != nc.user_id;
        *self.cfg.write().unwrap() = nc.clone();
        if creds_changed {
            tracing::info!(upstream = %self.name, "credentials changed; will re-login");
            *self.credentials.write().unwrap() = credentials::from_config(nc);
            *self.state.write().await = AuthState::default();
            self.mark_up();
        }
        Ok(true)
    }
}

fn own_config<'a>(cfg: &'a Config, name: &str) -> Option<&'a UpstreamConfig> {
    cfg.upstreams
        .iter()
        .find(|u| u.enabled && u.name.eq_ignore_ascii_case(name))
}
