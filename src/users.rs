//! Proxy users: who may log in to the proxy.
//!
//! [`ProxyAuthenticator`] is the seam. [`TomlAuthenticator`] serves the
//! `[[user]]` entries (argon2 hashes, swapped atomically on reload);
//! [`MeteredAuthenticator`] adds `jmp_logins_total{outcome}`. Future
//! backends (LDAP, OIDC, a database) implement the same trait.

use crate::config::Config;
use crate::error::{AuthError, ReloadError};
use crate::password::{DefaultHasher, HashedPassword, Password, PasswordVerifier};
use crate::reload::{ReloadContext, Reloadable};
use crate::telemetry::names;
use async_trait::async_trait;
use std::sync::{Arc, RwLock};

/// Public facts about a proxy user (no credentials).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProxyUserInfo {
    pub name: String,
    /// Stable 32-hex id handed to clients.
    pub id: String,
}

/// Stable id for a proxy user name.
pub fn user_id_for(name: &str) -> String {
    crate::ids::hash_id(&["user", &name.to_lowercase()])
}

pub trait ProxyAuthenticator: Send + Sync {
    fn name(&self) -> &'static str;
    /// Check credentials. Blocking (hashing): call from `spawn_blocking`.
    fn authenticate(&self, username: &str, password: &Password)
        -> Result<ProxyUserInfo, AuthError>;
    fn list(&self) -> Vec<ProxyUserInfo>;
    fn by_id(&self, id: &str) -> Option<ProxyUserInfo> {
        self.list().into_iter().find(|u| u.id == id)
    }
    fn by_name(&self, name: &str) -> Option<ProxyUserInfo> {
        self.list()
            .into_iter()
            .find(|u| u.name.eq_ignore_ascii_case(name))
    }
    fn ids(&self) -> Vec<String> {
        self.list().into_iter().map(|u| u.id).collect()
    }
    fn is_user_id(&self, normalized_id: &str) -> bool {
        self.list().iter().any(|u| u.id == normalized_id)
    }
}

pub struct ProxyUser {
    pub info: ProxyUserInfo,
    pub password: HashedPassword,
}

impl std::fmt::Debug for ProxyUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyUser")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// Users from the TOML `[[user]]` entries.
#[derive(Debug, Default)]
pub struct UserTable {
    pub list: Vec<ProxyUser>,
}

impl UserTable {
    pub fn from_config(cfg: &Config) -> Self {
        UserTable {
            list: cfg
                .users
                .iter()
                .map(|u| ProxyUser {
                    info: ProxyUserInfo {
                        name: u.name.clone(),
                        id: user_id_for(&u.name),
                    },
                    password: u.password.clone(),
                })
                .collect(),
        }
    }
}

/// [`ProxyAuthenticator`] over a [`UserTable`], generic over the verifier.
pub struct TomlAuthenticator<V: PasswordVerifier = DefaultHasher> {
    table: RwLock<Arc<UserTable>>,
    verifier: V,
}

impl TomlAuthenticator<DefaultHasher> {
    pub fn from_config(cfg: &Config) -> Self {
        Self::with_verifier(cfg, DefaultHasher::default())
    }
}

impl<V: PasswordVerifier> TomlAuthenticator<V> {
    pub fn with_verifier(cfg: &Config, verifier: V) -> Self {
        TomlAuthenticator {
            table: RwLock::new(Arc::new(UserTable::from_config(cfg))),
            verifier,
        }
    }
    fn table(&self) -> Arc<UserTable> {
        self.table.read().unwrap().clone()
    }
}

impl<V: PasswordVerifier> ProxyAuthenticator for TomlAuthenticator<V> {
    fn name(&self) -> &'static str {
        "toml"
    }
    fn authenticate(
        &self,
        username: &str,
        password: &Password,
    ) -> Result<ProxyUserInfo, AuthError> {
        let t = self.table();
        let u = t
            .list
            .iter()
            .find(|u| u.info.name.eq_ignore_ascii_case(username));
        match u {
            Some(u) if self.verifier.verify(&u.password, password) => Ok(u.info.clone()),
            Some(_) => Err(AuthError::BadPassword),
            None => Err(AuthError::UnknownUser),
        }
    }
    fn list(&self) -> Vec<ProxyUserInfo> {
        self.table().list.iter().map(|u| u.info.clone()).collect()
    }
    fn is_user_id(&self, id: &str) -> bool {
        self.table().list.iter().any(|u| u.info.id == id)
    }
}

#[async_trait]
impl<V: PasswordVerifier> Reloadable for TomlAuthenticator<V> {
    fn reload_name(&self) -> String {
        "users".into()
    }
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        if cx.old.users == cx.new.users {
            return Ok(false);
        }
        *self.table.write().unwrap() = Arc::new(UserTable::from_config(cx.new));
        Ok(true)
    }
}

/// Decorator: counts every login attempt by outcome.
pub struct MeteredAuthenticator<A>(pub A);

impl<A: ProxyAuthenticator> ProxyAuthenticator for MeteredAuthenticator<A> {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn authenticate(
        &self,
        username: &str,
        password: &Password,
    ) -> Result<ProxyUserInfo, AuthError> {
        let r = self.0.authenticate(username, password);
        let outcome = match &r {
            Ok(_) => "success",
            Err(e) => e.reason(),
        };
        metrics::counter!(names::LOGINS, "outcome" => outcome).increment(1);
        r
    }
    fn list(&self) -> Vec<ProxyUserInfo> {
        self.0.list()
    }
    fn by_id(&self, id: &str) -> Option<ProxyUserInfo> {
        self.0.by_id(id)
    }
    fn is_user_id(&self, id: &str) -> bool {
        self.0.is_user_id(id)
    }
}

#[async_trait]
impl<A: ProxyAuthenticator + Reloadable> Reloadable for MeteredAuthenticator<A> {
    fn reload_name(&self) -> String {
        self.0.reload_name()
    }
    fn validate(&self, cx: &ReloadContext<'_>) -> Result<(), ReloadError> {
        self.0.validate(cx)
    }
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        self.0.apply(cx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_authenticator() {
        let cache = crate::config::PasswordCache::default();
        let cfg = Config::parse(
            "[[user]]\nname = \"Fam\"\npassword = \"pw\"\n[[server]]\nname = \"a\"\nserver = \"http://a\"\napi_key = \"k\"\n",
            &cache,
        )
        .unwrap();
        let a = MeteredAuthenticator(TomlAuthenticator::from_config(&cfg));
        let u = a.authenticate("fam", &Password::new("pw")).unwrap();
        assert_eq!(u.name, "Fam");
        assert_eq!(u.id, user_id_for("fam"));
        assert_eq!(
            a.authenticate("fam", &Password::new("x")),
            Err(AuthError::BadPassword)
        );
        assert_eq!(
            a.authenticate("who", &Password::new("pw")),
            Err(AuthError::UnknownUser)
        );
        assert!(a.is_user_id(&u.id));
        assert_eq!(a.by_id(&u.id).unwrap().name, "Fam");
    }
}
