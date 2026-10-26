//! HTTP transport for HTTP-based backends.
//!
//! [`Transport`] is the seam; [`HttpTransport`] is reqwest (rustls), and
//! [`Instrumented`] is a decorator implementing the same trait that adds the
//! `jmp_upstream_*` request metrics. Further decorators (rate limiting,
//! recording for tests) wrap the same way.

use crate::error::TransportKind;
use crate::telemetry::{self, names};
use futures_util::future::BoxFuture;
use metrics::{counter, gauge, histogram};
use std::time::{Duration, Instant};

pub trait Transport: Send + Sync {
    /// `streaming` = no total timeout and no redirects (bodies may be huge).
    fn send(
        &self,
        req: reqwest::Request,
        streaming: bool,
    ) -> BoxFuture<'_, Result<reqwest::Response, reqwest::Error>>;
}

/// Plain reqwest transport: one client for API calls (total timeout) and one
/// for streams (connect timeout only).
pub struct HttpTransport {
    api: reqwest::Client,
    stream: reqwest::Client,
}

impl HttpTransport {
    pub fn build(
        timeout: Duration,
        rustls_cfg: Option<rustls::ClientConfig>,
    ) -> Result<Self, reqwest::Error> {
        let mk = |streaming: bool| -> Result<reqwest::Client, reqwest::Error> {
            let mut b = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .user_agent(concat!("jellyfin-merge-proxy/", env!("CARGO_PKG_VERSION")));
            b = if streaming {
                b.redirect(reqwest::redirect::Policy::none())
            } else {
                b.timeout(timeout)
            };
            if let Some(c) = &rustls_cfg {
                b = b.use_preconfigured_tls(c.clone());
            }
            b.build()
        };
        Ok(HttpTransport {
            api: mk(false)?,
            stream: mk(true)?,
        })
    }
}

impl Transport for HttpTransport {
    fn send(
        &self,
        req: reqwest::Request,
        streaming: bool,
    ) -> BoxFuture<'_, Result<reqwest::Response, reqwest::Error>> {
        let c = if streaming { &self.stream } else { &self.api };
        Box::pin(c.execute(req))
    }
}

/// Metrics decorator for any transport.
pub struct Instrumented<T> {
    inner: T,
    server: String,
    base_path: String,
}

impl<T> Instrumented<T> {
    pub fn new(inner: T, server: &str, base_path: &str) -> Self {
        Instrumented {
            inner,
            server: server.to_string(),
            base_path: base_path.to_string(),
        }
    }
}

/// Classify a reqwest error into a bounded kind.
pub fn error_kind(e: &reqwest::Error) -> TransportKind {
    if e.is_timeout() {
        return TransportKind::Timeout;
    }
    let mut chain = String::new();
    let mut src: Option<&dyn std::error::Error> = Some(e);
    while let Some(s) = src {
        if s.downcast_ref::<rustls::Error>().is_some() {
            return TransportKind::Tls;
        }
        chain.push_str(&s.to_string().to_ascii_lowercase());
        chain.push(' ');
        src = s.source();
    }
    if chain.contains("certificate") || chain.contains("tls") || chain.contains("handshake") {
        TransportKind::Tls
    } else if chain.contains("dns")
        || chain.contains("resolve")
        || chain.contains("lookup")
        || chain.contains("name or service")
    {
        TransportKind::Dns
    } else if e.is_connect() {
        TransportKind::Connect
    } else if e.is_body() || e.is_decode() {
        TransportKind::Body
    } else {
        TransportKind::Other
    }
}

impl<T: Transport> Transport for Instrumented<T> {
    fn send(
        &self,
        req: reqwest::Request,
        streaming: bool,
    ) -> BoxFuture<'_, Result<reqwest::Response, reqwest::Error>> {
        let path = req.url().path();
        let path = path.strip_prefix(self.base_path.as_str()).unwrap_or(path);
        let endpoint = telemetry::route_template(path);
        Box::pin(async move {
            let server = self.server.clone();
            let inflight = gauge!(names::UPSTREAM_IN_FLIGHT, "server" => server.clone());
            inflight.increment(1.0);
            let start = Instant::now();
            let r = self.inner.send(req, streaming).await;
            inflight.decrement(1.0);
            histogram!(names::UPSTREAM_DURATION, "server" => server.clone(), "endpoint" => endpoint.clone())
                .record(start.elapsed().as_secs_f64());
            let class = match &r {
                Ok(resp) => match resp.status().as_u16() {
                    100..=199 => "1xx",
                    200..=299 => "2xx",
                    300..=399 => "3xx",
                    400..=499 => "4xx",
                    _ => "5xx",
                },
                Err(e) => {
                    let kind = error_kind(e).as_str();
                    counter!(names::UPSTREAM_ERRORS, "server" => server.clone(), "kind" => kind)
                        .increment(1);
                    "error"
                }
            };
            counter!(names::UPSTREAM_REQUESTS, "server" => server, "endpoint" => endpoint, "status_class" => class).increment(1);
            r
        })
    }
}
