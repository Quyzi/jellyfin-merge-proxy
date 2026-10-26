//! rustls configuration for the listener and for upstream connections.
//!
//! [`TlsProvider`] is the seam used by the listener, the reload hub and the
//! backend factory; [`RustlsProvider`] (aws-lc-rs, no OpenSSL) is the
//! implementation. The free functions are kept for direct use.

use crate::config::{ClientAuth, TlsConfig, TlsVersion, UpstreamTlsConfig};
use crate::error::TlsError;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use std::path::Path;
use std::sync::Arc;

type Result<T, E = TlsError> = std::result::Result<T, E>;

/// Builds rustls configs for both directions.
pub trait TlsProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Build (and fully validate) the listener config.
    fn listener(&self, t: &TlsConfig) -> Result<Arc<rustls::ServerConfig>>;
    /// Client config for an upstream; `None` = platform defaults.
    fn upstream(&self, t: &UpstreamTlsConfig, server: &str)
        -> Result<Option<rustls::ClientConfig>>;
    /// notAfter (unix seconds) of the first certificate in a PEM file.
    fn cert_expiry(&self, path: &Path) -> Option<i64>;
}

/// rustls + aws-lc-rs implementation of [`TlsProvider`].
#[derive(Debug, Default, Clone, Copy)]
pub struct RustlsProvider;

impl TlsProvider for RustlsProvider {
    fn name(&self) -> &'static str {
        "rustls-aws-lc-rs"
    }
    fn listener(&self, t: &TlsConfig) -> Result<Arc<rustls::ServerConfig>> {
        server_config(t)
    }
    fn upstream(
        &self,
        t: &UpstreamTlsConfig,
        server: &str,
    ) -> Result<Option<rustls::ClientConfig>> {
        client_config(t, server)
    }
    fn cert_expiry(&self, path: &Path) -> Option<i64> {
        cert_expiry(path)
    }
}

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// Install aws-lc-rs as the process default provider (idempotent).
pub fn install_default_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

pub fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let read = |msg: String| TlsError::Read {
        what: "certificates",
        path: path.to_path_buf(),
        msg,
    };
    let certs: Vec<_> = CertificateDer::pem_file_iter(path)
        .map_err(|e| read(e.to_string()))?
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| read(e.to_string()))?;
    if certs.is_empty() {
        return Err(TlsError::NoCertificates(path.to_path_buf()));
    }
    Ok(certs)
}

pub fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_file(path).map_err(|e| TlsError::Read {
        what: "private key (PKCS#8/PKCS#1/SEC1 PEM)",
        path: path.to_path_buf(),
        msg: e.to_string(),
    })
}

fn versions(min: TlsVersion) -> &'static [&'static rustls::SupportedProtocolVersion] {
    static BOTH: &[&rustls::SupportedProtocolVersion] =
        &[&rustls::version::TLS13, &rustls::version::TLS12];
    static ONLY13: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];
    match min {
        TlsVersion::V12 => BOTH,
        TlsVersion::V13 => ONLY13,
    }
}

fn check_key_matches(
    chain: &[CertificateDer<'static>],
    key: &PrivateKeyDer<'static>,
    p: &CryptoProvider,
) -> Result<()> {
    let signing = p
        .key_provider
        .load_private_key(key.clone_key())
        .map_err(|e| TlsError::UnsupportedKey(e.to_string()))?;
    let ck = rustls::sign::CertifiedKey::new(chain.to_vec(), signing);
    ck.keys_match()
        .map_err(|e| TlsError::KeyMismatch(e.to_string()))
}

fn root_store(path: &Path) -> Result<rustls::RootCertStore> {
    let mut roots = rustls::RootCertStore::empty();
    for c in load_certs(path)? {
        roots.add(c).map_err(|e| TlsError::BadCa {
            path: path.to_path_buf(),
            msg: e.to_string(),
        })?;
    }
    Ok(roots)
}

/// Build (and fully validate) the listener's rustls config.
pub fn server_config(t: &TlsConfig) -> Result<Arc<rustls::ServerConfig>> {
    let p = provider();
    let chain = load_certs(&t.cert_path)?;
    let key = load_key(&t.key_path)?;
    check_key_matches(&chain, &key, &p)?;
    let builder = rustls::ServerConfig::builder_with_provider(p.clone())
        .with_protocol_versions(versions(t.min_version))?;
    let builder = match t.client_auth {
        ClientAuth::None => builder.with_no_client_auth(),
        mode => {
            let ca = t.client_ca_path.as_ref().ok_or(TlsError::MissingClientCa)?;
            let roots = Arc::new(root_store(ca)?);
            let vb = rustls::server::WebPkiClientVerifier::builder_with_provider(roots, p.clone());
            let vb = if mode == ClientAuth::Optional {
                vb.allow_unauthenticated()
            } else {
                vb
            };
            builder.with_client_cert_verifier(
                vb.build().map_err(|e| TlsError::Verifier(e.to_string()))?,
            )
        }
    };
    let mut cfg = builder.with_single_cert(chain, key)?;
    cfg.alpn_protocols = if t.http2 {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    } else {
        vec![b"http/1.1".to_vec()]
    };
    Ok(Arc::new(cfg))
}

/// notAfter of the first certificate in a PEM file, as unix seconds.
pub fn cert_expiry(path: &Path) -> Option<i64> {
    let certs = load_certs(path).ok()?;
    let (_, x) = x509_parser::parse_x509_certificate(certs[0].as_ref()).ok()?;
    Some(x.validity().not_after.timestamp())
}

/// Verifier that accepts anything (insecure_skip_verify).
#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Verifies the certificate against a fixed name instead of the URL host.
#[derive(Debug)]
struct NameOverride {
    inner: Arc<dyn ServerCertVerifier>,
    name: ServerName<'static>,
}

impl ServerCertVerifier for NameOverride {
    fn verify_server_cert(
        &self,
        e: &CertificateDer<'_>,
        i: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        o: &[u8],
        n: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.inner.verify_server_cert(e, i, &self.name, o, n)
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(m, c, d)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

impl UpstreamTlsConfig {
    pub fn is_default(&self) -> bool {
        self == &UpstreamTlsConfig::default()
    }
}

/// Build a rustls client config for an upstream, or None to use reqwest's
/// default (platform verifier) when nothing is customised.
pub fn client_config(t: &UpstreamTlsConfig, server: &str) -> Result<Option<rustls::ClientConfig>> {
    if t.is_default() {
        return Ok(None);
    }
    let p = provider();
    let builder = rustls::ClientConfig::builder_with_provider(p.clone())
        .with_protocol_versions(versions(t.min_version.unwrap_or_default()))?;
    let verifier: Arc<dyn ServerCertVerifier> = if t.insecure_skip_verify == Some(true) {
        tracing::warn!(upstream = %server, "!!! insecure_skip_verify = true: upstream TLS certificates are NOT verified. Anyone on the network path can impersonate this server. !!!");
        Arc::new(NoVerify(p.clone()))
    } else {
        let mut roots = rustls::RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for c in native.certs {
            let _ = roots.add(c);
        }
        if let Some(ca) = &t.ca_path {
            for c in load_certs(ca)? {
                roots.add(c).map_err(|e| TlsError::BadCa {
                    path: ca.clone(),
                    msg: e.to_string(),
                })?;
            }
        }
        if roots.is_empty() {
            return Err(TlsError::NoRoots);
        }
        let v =
            rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), p.clone())
                .build()
                .map_err(|e| TlsError::Verifier(e.to_string()))?;
        match &t.server_name {
            Some(n) => Arc::new(NameOverride {
                inner: v,
                name: ServerName::try_from(n.clone()).map_err(|e| TlsError::ServerName {
                    name: n.clone(),
                    msg: e.to_string(),
                })?,
            }),
            None => v,
        }
    };
    let builder = builder
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let mut cfg = match (&t.client_cert_path, &t.client_key_path) {
        (Some(c), Some(k)) => {
            let chain = load_certs(c)?;
            let key = load_key(k)?;
            check_key_matches(&chain, &key, &p)?;
            builder.with_client_auth_cert(chain, key)?
        }
        (None, None) => builder.with_no_client_auth(),
        _ => return Err(TlsError::ClientCertPair),
    };
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Some(cfg))
}
