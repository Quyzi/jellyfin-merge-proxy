//! Per-proxy-user upstream accounts (`[[user.account]]`).
//!
//! An [`AccountPool`] belongs to one backend and holds, per proxy user id,
//! the [`UpstreamCredentials`] of that user's own account on the server
//! and its cached login. Logins go through the backend's [`AuthChannel`],
//! so they work for any backend that can log in. Tokens never leave the
//! pool except as request headers; nothing here logs them.

use super::credentials::{self, AuthChannel, UpstreamCredentials};
use crate::config::{Config, Secret, UpstreamAccount};
use crate::error::BackendError;
use crate::telemetry::names;
use metrics::{counter, gauge};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tokio::sync::Mutex;

/// A cached login of a per-user account.
#[derive(Clone)]
pub struct AccountLogin {
    pub token: Secret,
    pub user_id: Option<String>,
    pub obtained: Instant,
}

impl std::fmt::Debug for AccountLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountLogin")
            .field("token", &"***")
            .field("user_id", &self.user_id)
            .finish()
    }
}

#[derive(Debug)]
pub struct Account {
    /// Proxy user name (logs only).
    pub user: String,
    cfg: UpstreamAccount,
    creds: Arc<dyn UpstreamCredentials>,
    login: Mutex<Option<AccountLogin>>,
}

impl Account {
    fn new(user: &str, cfg: &UpstreamAccount) -> Self {
        Account {
            user: user.to_string(),
            creds: credentials::from_account(cfg),
            cfg: cfg.clone(),
            login: Mutex::new(None),
        }
    }

    pub fn kind(&self) -> &'static str {
        self.creds.kind()
    }

    /// The cached login, logging in first if there is none or if the
    /// cached token is `stale` (it just got a 401).
    pub async fn login(
        &self,
        ch: &dyn AuthChannel,
        stale: Option<&str>,
    ) -> Result<AccountLogin, BackendError> {
        let mut g = self.login.lock().await;
        if let Some(l) = g.as_ref() {
            if Some(l.token.expose()) != stale {
                return Ok(l.clone());
            }
        }
        let r = self.creds.login(ch).await;
        let outcome = match &r {
            Ok(_) => "success",
            Err(BackendError::Unreachable { .. }) => "unreachable",
            Err(_) => "failure",
        };
        let server = ch.server_name().to_string();
        counter!(names::USER_ACCOUNT_AUTH, "server" => server.clone(), "outcome" => outcome)
            .increment(1);
        let grant = r.inspect_err(|e| {
            tracing::warn!(upstream = %server, user = %self.user, "per-user account login failed: {e}");
        })?;
        tracing::info!(upstream = %server, user = %self.user, upstream_user_id = ?grant.user_id, credentials = self.kind(), "logged in per-user account");
        let l = AccountLogin {
            token: grant.token,
            user_id: grant.user_id,
            obtained: Instant::now(),
        };
        *g = Some(l.clone());
        Ok(l)
    }

    /// Take the cached login (for logout on retire / removal).
    pub async fn take(&self) -> Option<AccountLogin> {
        self.login.lock().await.take()
    }
}

/// The per-user accounts of one server, keyed by proxy user id.
#[derive(Debug, Default)]
pub struct AccountPool {
    map: RwLock<HashMap<String, Arc<Account>>>,
}

fn wanted(server: &str, cfg: &Config) -> HashMap<String, (String, UpstreamAccount)> {
    let mut out = HashMap::new();
    for u in &cfg.users {
        if let Some(a) = u
            .accounts
            .iter()
            .find(|a| a.server.eq_ignore_ascii_case(server))
        {
            out.insert(
                crate::users::user_id_for(&u.name),
                (u.name.clone(), a.clone()),
            );
        }
    }
    out
}

impl AccountPool {
    pub fn from_config(server: &str, cfg: &Config) -> Self {
        let p = AccountPool::default();
        p.reload(server, cfg);
        p
    }

    /// Apply a new config: unchanged accounts keep their login; changed or
    /// removed ones are returned so the caller can end their sessions.
    pub fn reload(&self, server: &str, cfg: &Config) -> Vec<Arc<Account>> {
        let want = wanted(server, cfg);
        let mut map = self.map.write().unwrap();
        let mut dropped = vec![];
        map.retain(|id, a| {
            let keep = want
                .get(id)
                .is_some_and(|(n, c)| *c == a.cfg && *n == a.user);
            if !keep {
                dropped.push(a.clone());
            }
            keep
        });
        for (id, (name, c)) in want {
            map.entry(id)
                .or_insert_with(|| Arc::new(Account::new(&name, &c)));
        }
        gauge!(names::USER_ACCOUNTS, "server" => server.to_string()).set(map.len() as f64);
        if !dropped.is_empty() {
            tracing::info!(upstream = %server, changed = dropped.len(), "per-user accounts changed");
        }
        dropped
    }

    pub fn get(&self, user_id: &str) -> Option<Arc<Account>> {
        self.map.read().unwrap().get(user_id).cloned()
    }

    pub fn len(&self) -> usize {
        self.map.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn all(&self) -> Vec<Arc<Account>> {
        self.map.read().unwrap().values().cloned().collect()
    }
}
