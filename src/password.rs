//! Proxy-user password hashing.
//!
//! [`PasswordHasher`] / [`PasswordVerifier`] are the seams; [`Argon2id`] is
//! the implementation and [`MeteredHasher`] adds `jmp_argon2_duration_seconds`
//! to any hasher. [`HashedPassword`] is the only type login verification
//! accepts; plaintexts from the TOML are hashed once at load (cached across
//! reloads by [`PasswordCache`]) and zeroized.

use crate::error::PasswordError;
use argon2::password_hash::phc::PasswordHash as PhcHash;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use std::time::Instant;
use zeroize::Zeroize;

/// A plaintext password held only transiently. Debug redacts; zeroized on drop.
pub struct Password(String);

impl Password {
    pub fn new(s: impl Into<String>) -> Self {
        Password(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(***)")
    }
}
impl Drop for Password {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A PHC-format password hash (e.g. `$argon2id$v=19$...`).
#[derive(Clone, PartialEq, Eq)]
pub struct HashedPassword(String);

impl fmt::Debug for HashedPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HashedPassword($argon2…)")
    }
}

/// Produces password hashes.
pub trait PasswordHasher: Send + Sync {
    fn hash(&self, plain: &Password) -> Result<HashedPassword, PasswordError>;
    /// Accept an existing hash string from config (validated).
    fn parse(&self, phc: &str) -> Result<HashedPassword, PasswordError>;
}

/// Checks a candidate against a stored hash.
pub trait PasswordVerifier: Send + Sync {
    fn verify(&self, hash: &HashedPassword, candidate: &Password) -> bool;
}

/// argon2id with the crate's default (OWASP-recommended) parameters.
#[derive(Debug, Default, Clone, Copy)]
pub struct Argon2id;

impl PasswordHasher for Argon2id {
    fn hash(&self, plain: &Password) -> Result<HashedPassword, PasswordError> {
        use argon2::password_hash::PasswordHasher as _;
        // argon2 0.6 draws a 16-byte salt from the OS RNG (getrandom) itself;
        // the PHC string layout is the same as with 0.5's `SaltString`.
        let h = argon2::Argon2::default()
            .hash_password(plain.expose().as_bytes())
            .map_err(|e| PasswordError::Hash(e.to_string()))?
            .to_string();
        Ok(HashedPassword(h))
    }

    fn parse(&self, s: &str) -> Result<HashedPassword, PasswordError> {
        PhcHash::new(s).map_err(|e| PasswordError::InvalidHash(e.to_string()))?;
        if !s.starts_with("$argon2") {
            return Err(PasswordError::NotArgon2);
        }
        Ok(HashedPassword(s.to_string()))
    }
}

impl PasswordVerifier for Argon2id {
    fn verify(&self, hash: &HashedPassword, candidate: &Password) -> bool {
        use argon2::password_hash::PasswordVerifier as _;
        match PhcHash::new(&hash.0) {
            Ok(h) => argon2::Argon2::default()
                .verify_password(candidate.expose().as_bytes(), &h)
                .is_ok(),
            Err(_) => false,
        }
    }
}

/// Decorator: times every hash/verify into `jmp_argon2_duration_seconds`.
#[derive(Debug, Default, Clone, Copy)]
pub struct MeteredHasher<H>(pub H);

impl<H: PasswordHasher> PasswordHasher for MeteredHasher<H> {
    fn hash(&self, plain: &Password) -> Result<HashedPassword, PasswordError> {
        let t = Instant::now();
        let r = self.0.hash(plain);
        crate::telemetry::argon2_duration("hash", t.elapsed());
        r
    }
    fn parse(&self, phc: &str) -> Result<HashedPassword, PasswordError> {
        self.0.parse(phc)
    }
}

impl<H: PasswordVerifier> PasswordVerifier for MeteredHasher<H> {
    fn verify(&self, hash: &HashedPassword, candidate: &Password) -> bool {
        let t = Instant::now();
        let ok = self.0.verify(hash, candidate);
        crate::telemetry::argon2_duration("verify", t.elapsed());
        ok
    }
}

/// The hasher used when nothing else is configured.
pub type DefaultHasher = MeteredHasher<Argon2id>;

impl HashedPassword {
    /// Validate an existing PHC string with the default hasher.
    pub fn from_phc(s: &str) -> Result<Self, PasswordError> {
        DefaultHasher::default().parse(s)
    }
    /// Hash a plaintext with the default hasher.
    pub fn hash(plain: &str) -> Result<Self, PasswordError> {
        DefaultHasher::default().hash(&Password::new(plain))
    }
    /// Verify with the default verifier.
    pub fn verify(&self, candidate: &str) -> bool {
        DefaultHasher::default().verify(self, &Password::new(candidate))
    }
    pub fn phc(&self) -> &str {
        &self.0
    }
}

/// Caches plaintext -> hash conversions across reloads so unchanged users
/// are not re-hashed. Keys are `sha256(process_key || user || plaintext)`,
/// with a random per-process key, so the cache holds no plaintext.
/// Generic over the hasher (static dispatch).
pub struct PasswordCache<H: PasswordHasher = DefaultHasher> {
    key: [u8; 32],
    hasher: H,
    map: Mutex<HashMap<[u8; 32], HashedPassword>>,
}

impl Default for PasswordCache<DefaultHasher> {
    fn default() -> Self {
        PasswordCache::with_hasher(DefaultHasher::default())
    }
}

impl<H: PasswordHasher> PasswordCache<H> {
    pub fn with_hasher(hasher: H) -> Self {
        use rand::RngCore;
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        PasswordCache {
            key,
            hasher,
            map: Mutex::new(HashMap::new()),
        }
    }

    fn digest(&self, user: &str, plain: &str) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(self.key);
        h.update(user.to_lowercase().as_bytes());
        h.update([0u8]);
        h.update(plain.as_bytes());
        h.finalize().into()
    }

    /// Convert a raw config password into a hash, reusing a cached hash if
    /// the same plaintext was seen before for this user.
    pub fn normalize(
        &self,
        user: &str,
        raw: &crate::config::RawPassword,
    ) -> Result<HashedPassword, PasswordError> {
        use crate::config::{RawPassword, TaggedPassword};
        let plain = match raw {
            RawPassword::Tagged(TaggedPassword::Argon2(h)) => return self.hasher.parse(h),
            RawPassword::Bare(s) if s.starts_with("$argon2") => return self.hasher.parse(s),
            RawPassword::Bare(p) | RawPassword::Tagged(TaggedPassword::Plain(p)) => p,
        };
        let d = self.digest(user, plain);
        if let Some(h) = self.map.lock().unwrap().get(&d) {
            return Ok(h.clone());
        }
        let h = self.hasher.hash(&Password::new(plain.as_str()))?;
        self.map.lock().unwrap().insert(d, h.clone());
        Ok(h)
    }

    pub fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake hasher proves the cache is generic over the trait.
    #[derive(Default)]
    struct Fake;
    impl PasswordHasher for Fake {
        fn hash(&self, p: &Password) -> Result<HashedPassword, PasswordError> {
            Ok(HashedPassword(format!("$fake${}", p.expose().len())))
        }
        fn parse(&self, phc: &str) -> Result<HashedPassword, PasswordError> {
            Ok(HashedPassword(phc.into()))
        }
    }

    #[test]
    fn cache_is_generic_and_debug_redacts() {
        let c = PasswordCache::with_hasher(Fake);
        let raw = crate::config::RawPassword::Bare("hunter2".into());
        let h = c.normalize("u", &raw).unwrap();
        assert_eq!(h.phc(), "$fake$7");
        assert_eq!(c.len(), 1);
        assert!(!format!("{:?}", Password::new("hunter2")).contains("hunter2"));
        assert!(!format!("{h:?}").contains("fake$7"));
    }

    #[test]
    fn argon2_round_trip() {
        let a = MeteredHasher(Argon2id);
        let h = a.hash(&Password::new("pw")).unwrap();
        assert!(a.verify(&h, &Password::new("pw")));
        assert!(!a.verify(&h, &Password::new("nope")));
        assert!(a.parse("plain").is_err());
    }
}
