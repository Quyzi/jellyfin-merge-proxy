//! Listener setup (plain HTTP or rustls), optional HTTP->HTTPS redirect and
//! metrics listeners.

use crate::backend::BackendFactory;
use crate::config::{Config, PasswordCache};
use crate::telemetry;
use crate::tls::{RustlsProvider, TlsProvider};
use crate::{AppState, Shared, StateOptions};
use anyhow::{Context, Result};
use axum::response::{IntoResponse, Redirect};
use axum_server::tls_rustls::RustlsConfig;
use axum_server::Handle;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Default)]
pub struct Options {
    pub password_cache: Option<PasswordCache>,
    /// Defaults to Jellyfin for every `[[server]]`.
    pub backend_factory: Option<Arc<dyn BackendFactory>>,
    /// Defaults to rustls.
    pub tls_provider: Option<Arc<dyn TlsProvider>>,
}

pub struct Running {
    pub addr: SocketAddr,
    pub tls: bool,
    pub metrics_addr: Option<SocketAddr>,
    pub redirect_addr: Option<SocketAddr>,
    pub state: Shared,
    handles: Vec<Handle<SocketAddr>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    main: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Running {
    /// Graceful shutdown of all listeners (10s grace for open streams).
    pub fn shutdown(&self) {
        for h in &self.handles {
            h.graceful_shutdown(Some(Duration::from_secs(10)));
        }
        for t in &self.tasks {
            t.abort();
        }
    }

    pub async fn wait(self) -> Result<()> {
        self.main.await??;
        Ok(())
    }
}

fn bind(addr: &str) -> Result<std::net::TcpListener> {
    let l = std::net::TcpListener::bind(addr).with_context(|| format!("binding {addr}"))?;
    l.set_nonblocking(true)?;
    Ok(l)
}

pub async fn start(cfg: Config, opts: Options) -> Result<Running> {
    crate::tls::install_default_provider();
    if cfg.metrics.enabled {
        telemetry::init_metrics();
        if cfg.metrics.process_metrics {
            telemetry::enable_process_metrics();
        }
    }
    let tls_provider: Arc<dyn TlsProvider> = opts
        .tls_provider
        .unwrap_or_else(|| Arc::new(RustlsProvider));
    // Validate TLS before anything is bound.
    let server_tls = match &cfg.tls {
        Some(t) => Some(tls_provider.listener(t).context("[tls]")?),
        None => None,
    };
    let listen = cfg.proxy.listen.clone();
    let metrics_listen = cfg.metrics.listen.clone().filter(|_| cfg.metrics.enabled);
    let redirect_listen = cfg
        .tls
        .as_ref()
        .and_then(|t| t.redirect_http_listen.clone());
    let http2 = cfg.tls.as_ref().map(|t| t.http2).unwrap_or(true);

    let state = AppState::with_options(
        cfg,
        StateOptions {
            password_cache: opts.password_cache,
            backend_factory: opts.backend_factory,
            tls_provider: Some(tls_provider),
            ..Default::default()
        },
    )?;
    state.spawn_initial_logins();
    let app = crate::build_app(state.clone()).into_make_service_with_connect_info::<SocketAddr>();

    let listener = bind(&listen)?;
    let addr = listener.local_addr()?;
    let handle: Handle<SocketAddr> = Handle::new();
    let mut handles = vec![handle.clone()];
    let mut tasks = vec![];
    let tls = server_tls.is_some();
    let main = match server_tls {
        Some(sc) => {
            let rc = RustlsConfig::from_config(sc);
            let mut server =
                axum_server::tls_rustls::from_tcp_rustls(listener, rc)?.handle(handle.clone());
            if !http2 {
                server = server.http1_only();
            }
            tokio::spawn(server.serve(app))
        }
        None => {
            let server = axum_server::from_tcp(listener)?.handle(handle.clone());
            tokio::spawn(server.serve(app))
        }
    };

    let mut metrics_addr = None;
    if let Some(ml) = metrics_listen {
        let l = bind(&ml)?;
        metrics_addr = Some(l.local_addr()?);
        let h = Handle::new();
        handles.push(h.clone());
        let r = crate::proxy::metrics_router(state.clone());
        let srv = axum_server::from_tcp(l)?.handle(h);
        tasks.push(tokio::spawn(async move {
            if let Err(e) = srv.serve(r.into_make_service()).await {
                tracing::error!("metrics listener: {e}");
            }
        }));
    }

    let mut redirect_addr = None;
    if let Some(rl) = redirect_listen {
        let l = bind(&rl)?;
        redirect_addr = Some(l.local_addr()?);
        let h = Handle::new();
        handles.push(h.clone());
        let port = addr.port();
        let r = axum::Router::new().fallback(move |req: axum::extract::Request| async move {
            let host = req
                .headers()
                .get(axum::http::header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("localhost");
            let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
            let host = if host.starts_with('[') || !host.contains(':') {
                host.to_string()
            } else {
                format!("[{host}]")
            };
            let pq = req
                .uri()
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or("/");
            let target = if port == 443 {
                format!("https://{host}{pq}")
            } else {
                format!("https://{host}:{port}{pq}")
            };
            Redirect::permanent(&target).into_response()
        });
        let srv = axum_server::from_tcp(l)?.handle(h);
        tasks.push(tokio::spawn(async move {
            if let Err(e) = srv.serve(r.into_make_service()).await {
                tracing::error!("redirect listener: {e}");
            }
        }));
    }

    // Periodic gauges.
    {
        let st = state.clone();
        tasks.push(tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(15));
            loop {
                iv.tick().await;
                for u in st.backends().iter() {
                    u.report_gauges().await;
                }
                st.ids.publish_gauges();
            }
        }));
    }

    let c = state.cfg();
    tracing::info!(
        "{} listening on {}://{} ({} servers, server id {}){}{}",
        c.proxy.name,
        if tls { "https" } else { "http" },
        addr,
        state.backends().len(),
        state.server_id,
        metrics_addr
            .map(|a| format!(", metrics on http://{a}{}", c.metrics.path))
            .unwrap_or_default(),
        redirect_addr
            .map(|a| format!(", http->https redirect on {a}"))
            .unwrap_or_default(),
    );
    Ok(Running {
        addr,
        tls,
        metrics_addr,
        redirect_addr,
        state,
        handles,
        tasks,
        main,
    })
}
