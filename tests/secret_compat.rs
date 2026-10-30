//! On-disk secret formats must survive dependency upgrades.
//!
//! `tests/fixtures/secrets_v1.json` holds argon2id PHC strings and `enc:v1:`
//! values produced by the master build *before* the argon2 0.6 /
//! chacha20poly1305 0.11 port (argon2 0.5.3, chacha20poly1305 0.10.1). These
//! tests prove the current build still verifies/decrypts exactly those bytes,
//! and that newly produced values keep the same format.

use base64::{engine::general_purpose::STANDARD, Engine};
use jmp::config::{Config, PasswordCache};
use jmp::password::{Argon2id, HashedPassword, Password, PasswordHasher, PasswordVerifier};
use jmp::secrets::{EncryptionKey, SecretCipher, XChaCha20Cipher};
use serde_json::Value;

const FIXTURES: &str = include_str!("fixtures/secrets_v1.json");

fn fixtures() -> Value {
    serde_json::from_str(FIXTURES).unwrap()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap()
}

fn fixture_cipher(f: &Value) -> XChaCha20Cipher {
    XChaCha20Cipher::new(EncryptionKey::from_hex(s(f, "enc_v1_key_hex")).unwrap())
}

#[test]
fn legacy_argon2id_hashes_verify() {
    let f = fixtures();
    let a = Argon2id;
    let list = f["argon2id"].as_array().unwrap();
    assert!(list.len() >= 4);
    for e in list {
        let (plain, phc) = (s(e, "plaintext"), s(e, "phc"));
        assert!(phc.starts_with("$argon2id$v=19$"), "{phc}");
        let h = a.parse(phc).expect("legacy PHC string parses");
        assert_eq!(h.phc(), phc, "stored verbatim");
        assert!(a.verify(&h, &Password::new(plain)), "verifies: {plain:?}");
        assert!(!a.verify(&h, &Password::new(format!("{plain}x"))));
        assert!(HashedPassword::from_phc(phc).unwrap().verify(plain));
    }
}

#[test]
fn legacy_enc_v1_values_decrypt() {
    let f = fixtures();
    let c = fixture_cipher(&f);
    let list = f["enc_v1"].as_array().unwrap();
    assert!(list.len() >= 4);
    for e in list {
        let v = s(e, "value");
        assert!(c.is_encrypted(v));
        assert_eq!(c.decrypt(v).unwrap(), s(e, "plaintext"), "{v}");
    }
}

#[test]
fn legacy_enc_v1_rejects_wrong_key_and_tampering() {
    let f = fixtures();
    let other = XChaCha20Cipher::new(EncryptionKey::from_bytes([7u8; 32]));
    let c = fixture_cipher(&f);
    for e in f["enc_v1"].as_array().unwrap() {
        let v = s(e, "value");
        assert!(other.decrypt(v).is_err());
        let mut raw = STANDARD.decode(&v["enc:v1:".len()..]).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        assert!(c
            .decrypt(&format!("enc:v1:{}", STANDARD.encode(raw)))
            .is_err());
    }
}

#[test]
fn new_argon2id_hashes_keep_format_and_round_trip() {
    let a = Argon2id;
    let h = a.hash(&Password::new("fresh pw ü")).unwrap();
    // Same algorithm, version and default params as before the upgrade.
    assert!(
        h.phc().starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
        "{}",
        h.phc()
    );
    let parts: Vec<&str> = h.phc().split('$').collect();
    assert_eq!(parts.len(), 6);
    assert_eq!(parts[4].len(), 22, "16-byte salt, unpadded B64");
    assert_eq!(parts[5].len(), 43, "32-byte output, unpadded B64");
    assert!(a.verify(&h, &Password::new("fresh pw ü")));
    assert!(!a.verify(&h, &Password::new("fresh pw u")));
    let h2 = a.hash(&Password::new("fresh pw ü")).unwrap();
    assert_ne!(h.phc(), h2.phc(), "random salt per hash");
    assert_eq!(a.parse(h.phc()).unwrap(), h);
}

#[test]
fn new_enc_v1_values_keep_format_and_round_trip() {
    let f = fixtures();
    let c = fixture_cipher(&f);
    for e in f["enc_v1"].as_array().unwrap() {
        let plain = s(e, "plaintext");
        let v = c.encrypt(plain).unwrap();
        assert!(v.starts_with("enc:v1:"));
        // 24-byte nonce || ciphertext || 16-byte tag, standard padded base64.
        let raw = STANDARD.decode(&v["enc:v1:".len()..]).unwrap();
        assert_eq!(raw.len(), 24 + plain.len() + 16);
        assert_eq!(c.decrypt(&v).unwrap(), plain);
        assert_ne!(c.encrypt(plain).unwrap(), v, "random nonce per value");
    }
}

/// End to end through `Config::load`: the key comes from `secret.key` in the
/// data dir (the KeyStore path), users carry legacy PHC strings and the
/// upstream carries legacy `enc:v1:` values.
#[test]
fn config_load_reads_legacy_secrets() {
    let f = fixtures();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let key_path = data.join("secret.key");
    std::fs::write(&key_path, s(&f, "enc_v1_key_hex")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let h = &f["argon2id"][0];
    let (pw, key) = (&f["enc_v1"][0], &f["enc_v1"][2]);
    let toml = format!(
        "[proxy]\nname = \"compat\"\ndata_dir = {data:?}\n\n\
         [[user]]\nname = \"u\"\npassword = '{phc}'\n\n\
         [[server]]\nname = \"s\"\nserver = \"http://127.0.0.1:1\"\nuser = \"jmp\"\n\
         password = \"{pwv}\"\napi_key = \"{keyv}\"\n",
        data = data.display().to_string(),
        phc = s(h, "phc"),
        pwv = s(pw, "value"),
        keyv = s(key, "value"),
    );
    let path = dir.path().join("jmp.toml");
    std::fs::write(&path, &toml).unwrap();
    // JMP_SECRET_KEY would take precedence over secret.key; this test must
    // exercise the file.
    if std::env::var_os("JMP_SECRET_KEY").is_some() {
        eprintln!("JMP_SECRET_KEY set; skipping config_load_reads_legacy_secrets");
        return;
    }
    let cfg = Config::load(&path, &PasswordCache::default()).unwrap();
    assert!(cfg.users[0].password.verify(s(h, "plaintext")));
    assert_eq!(cfg.users[0].password.phc(), s(h, "phc"));
    let up = &cfg.upstreams[0];
    assert_eq!(up.password.as_ref().unwrap().expose(), s(pw, "plaintext"));
    assert_eq!(up.api_key.as_ref().unwrap().expose(), s(key, "plaintext"));
    // Nothing to rewrite: the file is untouched.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), toml);
}
