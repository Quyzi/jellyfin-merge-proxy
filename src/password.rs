//! Proxy-user password hashing.
//!
//! [`PasswordHasher`] / [`PasswordVerifier`] are the seams; [`Argon2id`] is
//! the implementation and [`MeteredHasher`] adds `jmp_argon2_duration_seconds`
//! to any hasher. [`HashedPassword`] is the only type login verification
//! accepts.

use crate::error::PasswordError;
use std::fmt;
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
        use argon2::password_hash::{PasswordHasher as _, SaltString};
        use rand::RngCore;
        let mut salt = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        let salt = SaltString::encode_b64(&salt).map_err(|e| PasswordError::Hash(e.to_string()))?;
        let h = argon2::Argon2::default()
            .hash_password(plain.expose().as_bytes(), &salt)
            .map_err(|e| PasswordError::Hash(e.to_string()))?
            .to_string();
        Ok(HashedPassword(h))
    }

    fn parse(&self, s: &str) -> Result<HashedPassword, PasswordError> {
        argon2::password_hash::PasswordHash::new(s)
            .map_err(|e| PasswordError::InvalidHash(e.to_string()))?;
        if !s.starts_with("$argon2") {
            return Err(PasswordError::NotArgon2);
        }
        Ok(HashedPassword(s.to_string()))
    }
}

impl PasswordVerifier for Argon2id {
    fn verify(&self, hash: &HashedPassword, candidate: &Password) -> bool {
        use argon2::password_hash::{PasswordHash, PasswordVerifier as _};
        match PasswordHash::new(&hash.0) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2_round_trip() {
        let a = MeteredHasher(Argon2id);
        let h = a.hash(&Password::new("pw")).unwrap();
        assert!(a.verify(&h, &Password::new("pw")));
        assert!(!a.verify(&h, &Password::new("nope")));
        assert!(a.parse("plain").is_err());
    }
}
