//! How the proxy authenticates *to* an upstream.
//!
//! [`UpstreamCredentials`] is the seam, with [`ApiKeyCredentials`] and
//! [`PasswordCredentials`]. A credential talks to the server only through
//! an [`AuthChannel`], so the same credential types work for any backend
//! that can carry Jellyfin-style auth calls.

use crate::config::{Secret, UpstreamAccount, UpstreamConfig};
use crate::error::BackendError;
use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::sync::Arc;

/// Minimal request channel used during login.
#[async_trait]
pub trait AuthChannel: Send + Sync {
    fn server_name(&self) -> &str;
    /// JSON call; `token` authenticates when given. Transport failures are
    /// `BackendError::Unreachable`.
    async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value), BackendError>;
}

/// The result of a login.
#[derive(Debug, Clone)]
pub struct Grant {
    pub token: Secret,
    pub user_id: Option<String>,
}

#[async_trait]
pub trait UpstreamCredentials: Send + Sync + std::fmt::Debug {
    /// "api_key" / "password" (debug endpoint, logs).
    fn kind(&self) -> &'static str;
    async fn login(&self, ch: &dyn AuthChannel) -> Result<Grant, BackendError>;
}

/// `api_key = "..."`: the key is the token; the user comes from `user_id`,
/// `user` (by name) or the first administrator.
#[derive(Debug, Clone)]
pub struct ApiKeyCredentials {
    pub key: Secret,
    pub user: Option<String>,
    pub user_id: Option<String>,
}

#[async_trait]
impl UpstreamCredentials for ApiKeyCredentials {
    fn kind(&self) -> &'static str {
        "api_key"
    }
    async fn login(&self, ch: &dyn AuthChannel) -> Result<Grant, BackendError> {
        let user_id = match &self.user_id {
            Some(u) => Some(u.clone()),
            None => match ch
                .call(Method::GET, "Users", Some(self.key.expose()), None)
                .await
            {
                Ok((s, users)) if s.is_success() => pick_user(&users, self.user.as_deref()),
                Ok(_) => None,
                Err(e @ BackendError::Unreachable { .. }) => return Err(e),
                Err(_) => None,
            },
        };
        if user_id.is_none() {
            tracing::warn!(upstream = %ch.server_name(), "api_key auth but no user found; set user_id or user");
        }
        Ok(Grant {
            token: self.key.clone(),
            user_id,
        })
    }
}

fn pick_user(users: &Value, name: Option<&str>) -> Option<String> {
    let arr = users.as_array()?;
    let id_of = |u: &Value| u.get("Id").and_then(|i| i.as_str()).map(String::from);
    if let Some(name) = name {
        if let Some(u) = arr.iter().find(|u| {
            u.get("Name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        }) {
            return id_of(u);
        }
    }
    if let Some(u) = arr
        .iter()
        .find(|u| u.pointer("/Policy/IsAdministrator") == Some(&Value::Bool(true)))
    {
        return id_of(u);
    }
    arr.first().and_then(id_of)
}

/// `user` + `password`: `POST Users/AuthenticateByName`.
#[derive(Debug, Clone)]
pub struct PasswordCredentials {
    pub user: String,
    pub password: Secret,
    pub user_id: Option<String>,
}

#[async_trait]
impl UpstreamCredentials for PasswordCredentials {
    fn kind(&self) -> &'static str {
        "password"
    }
    async fn login(&self, ch: &dyn AuthChannel) -> Result<Grant, BackendError> {
        let server = ch.server_name().to_string();
        let (status, v) = ch
            .call(
                Method::POST,
                "Users/AuthenticateByName",
                None,
                Some(json!({"Username": self.user, "Pw": self.password.expose()})),
            )
            .await?;
        if !status.is_success() {
            return Err(BackendError::Auth {
                server,
                msg: format!("HTTP {status}"),
            });
        }
        let token = v
            .get("AccessToken")
            .and_then(|t| t.as_str())
            .map(Secret::new)
            .ok_or_else(|| BackendError::Auth {
                server: server.clone(),
                msg: "no AccessToken in response".into(),
            })?;
        let user_id = self.user_id.clone().or_else(|| {
            v.pointer("/User/Id")
                .and_then(|t| t.as_str())
                .map(String::from)
        });
        Ok(Grant { token, user_id })
    }
}

/// Pick the credential type for a `[[server]]` entry (api_key wins).
pub fn from_config(cfg: &UpstreamConfig) -> Arc<dyn UpstreamCredentials> {
    pick(&cfg.api_key, &cfg.username, &cfg.password, &cfg.user_id)
}

/// The same for a per-user `[[user.account]]`.
pub fn from_account(cfg: &UpstreamAccount) -> Arc<dyn UpstreamCredentials> {
    pick(&cfg.api_key, &cfg.username, &cfg.password, &cfg.user_id)
}

fn pick(
    api_key: &Option<Secret>,
    user: &Option<String>,
    password: &Option<Secret>,
    user_id: &Option<String>,
) -> Arc<dyn UpstreamCredentials> {
    match api_key {
        Some(k) => Arc::new(ApiKeyCredentials {
            key: k.clone(),
            user: user.clone(),
            user_id: user_id.clone(),
        }),
        None => Arc::new(PasswordCredentials {
            user: user.clone().unwrap_or_default(),
            password: password.clone().unwrap_or_else(|| Secret::new("")),
            user_id: user_id.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts() {
        let c = PasswordCredentials {
            user: "u".into(),
            password: Secret::new("meow"),
            user_id: None,
        };
        assert!(!format!("{c:?}").contains("meow"));
        let k = ApiKeyCredentials {
            key: Secret::new("k123"),
            user: None,
            user_id: None,
        };
        assert!(!format!("{k:?}").contains("k123"));
    }

    #[test]
    fn picks_named_then_admin_then_first() {
        let users = json!([
            {"Id": "1", "Name": "a"},
            {"Id": "2", "Name": "b", "Policy": {"IsAdministrator": true}}
        ]);
        assert_eq!(pick_user(&users, Some("A")).as_deref(), Some("1"));
        assert_eq!(pick_user(&users, None).as_deref(), Some("2"));
        assert_eq!(pick_user(&json!([{"Id": "9"}]), None).as_deref(), Some("9"));
    }
}
