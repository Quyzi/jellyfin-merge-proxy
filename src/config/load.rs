//! Loading and validating a [`Config`] from TOML.

use super::*;
use std::path::Path;

impl Config {
    pub fn load(path: &Path, cache: &PasswordCache) -> Result<Config, ConfigError> {
        let mut text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        // Parse to get the data_dir (needed for key loading)
        let raw: RawConfig = match toml::from_str(&text) {
            Ok(r) => r,
            Err(_) => {
                // parse() reports the error (with the friendlier message for the
                // pre-[[server]] layout); wipe the text only afterwards.
                let r = Self::parse(&text, cache);
                text.zeroize();
                return r;
            }
        };

        let data_dir = raw.proxy.data_dir.clone();

        // Load encryption key
        let key_store = crate::secrets::DefaultKeyStore;
        let key = key_store.load(&data_dir)?;
        let cipher = crate::secrets::XChaCha20Cipher::new(key);

        // Rewrite secrets if enabled and writable
        if raw.proxy.rewrite_secrets {
            match crate::config_rewrite::rewrite_secrets(path, cache, &cipher) {
                Ok(true) => {
                    tracing::info!(path = %path.display(), "rewrote plaintext secrets in config file");
                }
                Ok(false) => {}
                Err(e) if is_permission_error(&e) => {
                    counter!(crate::telemetry::names::SECRET_REWRITE_FAILURES).increment(1);
                    tracing::warn!(
                        path = %path.display(),
                        "config file not writable; secrets remain plaintext on disk: {e}"
                    );
                }
                Err(e) => {
                    counter!(crate::telemetry::names::SECRET_REWRITE_FAILURES).increment(1);
                    text.zeroize();
                    return Err(e);
                }
            }
        }

        let r = Self::parse_with_cipher(&text, cache, Some(&cipher));
        text.zeroize();
        r
    }

    pub fn parse(text: &str, cache: &PasswordCache) -> Result<Config, ConfigError> {
        Self::parse_with_cipher(text, cache, None)
    }

    fn parse_with_cipher(
        text: &str,
        cache: &PasswordCache,
        cipher: Option<&dyn crate::secrets::SecretCipher>,
    ) -> Result<Config, ConfigError> {
        let raw: RawConfig = match toml::from_str(text) {
            Ok(r) => r,
            Err(e) => {
                // Friendlier message for the pre-[[server]] layout.
                if let Ok(v) = text.parse::<toml::Table>() {
                    if v.get("server").map(|s| s.is_table()) == Some(true) {
                        return Err(ConfigError::invalid(
                            "`[server]` is now `[proxy]`; `[[server]]` (double brackets) lists the upstream Jellyfin servers",
                        ));
                    }
                }
                return Err(e.into());
            }
        };
        let mut users = Vec::with_capacity(raw.users.len());
        for u in &raw.users {
            let password =
                cache
                    .normalize(&u.name, &u.password)
                    .map_err(|e| ConfigError::Password {
                        user: u.name.clone(),
                        source: e,
                    })?;
            let mut accounts = u.accounts.clone();
            if let Some(c) = cipher {
                for a in &mut accounts {
                    a.decrypt(&u.name, c)?;
                }
            }
            users.push(UserConfig {
                name: u.name.clone(),
                password,
                accounts,
                max_parental_rating: u.max_parental_rating.clone(),
                block_unrated: u.block_unrated,
                max_active_sessions: u.max_active_sessions,
                allowed_countries: normalize_countries(&u.name, &u.allowed_countries)?,
            });
        }

        // Decrypt upstream secrets
        let mut upstreams = raw.upstreams.clone();
        if let Some(c) = cipher {
            for upstream in &mut upstreams {
                if let Some(password) = &upstream.password {
                    upstream.password = Some(password.decrypt(c).map_err(|e| {
                        counter!(crate::telemetry::names::SECRET_DECRYPT_FAILURES, "kind" => "upstream_password").increment(1);
                        tracing::error!(upstream = %upstream.name, "failed to decrypt upstream password: {e}");
                        e
                    })?);
                }
                if let Some(api_key) = &upstream.api_key {
                    upstream.api_key = Some(api_key.decrypt(c).map_err(|e| {
                        counter!(crate::telemetry::names::SECRET_DECRYPT_FAILURES, "kind" => "upstream_api_key").increment(1);
                        tracing::error!(upstream = %upstream.name, "failed to decrypt upstream api_key: {e}");
                        e
                    })?);
                }
            }
        }

        // `raw` (and with it every plaintext) is dropped + zeroized here.
        let cfg = Config {
            proxy: raw.proxy.clone(),
            users,
            upstreams,
            merge: raw.merge.clone(),
            cache: raw.cache.clone(),
            tls: raw.tls.clone(),
            upstream_tls: raw.upstream_tls.clone(),
            database: raw.database.clone(),
            index: raw.index.clone(),
            userdata: raw.userdata.clone(),
            geo: raw.geo.clone(),
            logging: raw.logging.clone(),
            metrics: raw.metrics.clone(),
            debug: raw.debug.clone(),
        };
        drop(raw);
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        self.index.validate().map_err(ConfigError::Invalid)?;
        self.userdata.validate().map_err(ConfigError::Invalid)?;
        if self.users.is_empty() {
            return bad("at least one [[user]] is required".into());
        }
        let mut seen = std::collections::HashSet::new();
        for u in &self.users {
            if !seen.insert(u.name.to_lowercase()) {
                return bad(format!("duplicate user '{}'", u.name));
            }
        }
        if self.upstreams.iter().filter(|u| u.enabled).count() == 0 {
            return bad("at least one enabled [[server]] is required".into());
        }
        let mut names = std::collections::HashSet::new();
        for u in &self.upstreams {
            if !names.insert(u.name.to_lowercase()) {
                return bad(format!("duplicate [[server]] name '{}'", u.name));
            }
            if u.api_key.is_none() && u.username.is_none() {
                return bad(format!(
                    "[[server]] '{}' needs api_key or user/password",
                    u.name
                ));
            }
            if u.api_key.is_none() && u.password.is_none() {
                return bad(format!(
                    "[[server]] '{}' has a user but no password",
                    u.name
                ));
            }
            url::Url::parse(&u.url).map_err(|e| {
                ConfigError::Invalid(format!("[[server]] '{}' server url: {e}", u.name))
            })?;
        }
        accounts::validate_accounts(self).map_err(ConfigError::Invalid)?;
        self.geo
            .validate(self.users.iter().any(|u| !u.allowed_countries.is_empty()))
            .map_err(ConfigError::Invalid)?;
        if let Some(id) = &self.proxy.id {
            if crate::ids::normalize_hex_id(id).is_none() {
                return bad("proxy.id must be 32 hex characters".into());
            }
        }
        if let Some(t) = &self.tls {
            if t.client_auth != ClientAuth::None && t.client_ca_path.is_none() {
                return bad("tls.client_auth requires tls.client_ca_path".into());
            }
        }
        tracing_subscriber::EnvFilter::try_new(&self.logging.filter)
            .map_err(|e| ConfigError::Invalid(format!("logging.filter: {e}")))?;
        if !self.metrics.path.starts_with('/') || !self.debug.path.starts_with('/') {
            return bad("metrics.path / debug.path must start with '/'".into());
        }
        if self.cache.enabled && self.cache.max_entries == 0 {
            return bad("cache.max_entries must be > 0 (or set cache.enabled = false)".into());
        }
        Ok(())
    }

    /// Every file the running config depends on (for change detection).
    pub fn watched_files(&self) -> Vec<PathBuf> {
        let mut v = vec![];
        if let Some(t) = &self.tls {
            v.push(t.cert_path.clone());
            v.push(t.key_path.clone());
            v.extend(t.client_ca_path.clone());
        }
        let mut add = |t: &UpstreamTlsConfig| {
            v.extend(t.ca_path.clone());
            v.extend(t.client_cert_path.clone());
            v.extend(t.client_key_path.clone());
        };
        add(&self.upstream_tls);
        for u in &self.upstreams {
            if let Some(t) = &u.tls {
                add(t);
            }
        }
        v.sort();
        v.dedup();
        v
    }
}
