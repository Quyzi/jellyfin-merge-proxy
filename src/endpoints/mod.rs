//! Route/endpoint registry.
//!
//! Every client-facing Jellyfin route is an [`Endpoint`] in an ordered
//! [`Registry`] (dyn: assembled at runtime, overridable through
//! `StateOptions::endpoints`). The default registry is built from small
//! modules ([`system`], [`users`], [`library`], [`sessions`], [`compat`])
//! plus a [`fallback`] that forwards anything else to the owning backend.
//!
//! Routes match on *classified* path segments: a 32-hex segment becomes
//! `{u}` (a proxy user id), `{root}` (the virtual root folder), `{view}`
//! (a merged library) or `{id}` (anything else); other segments are
//! lower-cased literals. Patterns use those classes plus `*` (any one
//! segment) and `**` (any rest, including nothing), e.g.
//! `users/{u}/items/{view}` or `web/**`.
//!
//! A handler may decline with [`Flow::Next`], handing the request on to the
//! next matching route.

pub mod compat;
pub mod fallback;
pub mod library;
pub mod sessions;
pub mod system;
pub mod users;

use crate::proxy::auth;
use crate::proxy::forward;
use crate::proxy::response::err_resp;
use crate::proxy::{Client, Req};
use crate::Shared;
use axum::http::Method;
use axum::response::Response;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Who may call a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// No token needed (handler gets an anonymous [`Client`]).
    Public,
    /// Needs a valid proxy token (GET image routes also accept anonymous).
    Authenticated,
}

/// What a handler did.
pub enum Flow {
    Done(Response),
    /// Not mine after all: try the next matching route.
    Next(Req),
}

pub type HandlerResult = anyhow::Result<Flow>;
pub type BoxFut<'a> = Pin<Box<dyn Future<Output = HandlerResult> + Send + 'a>>;
/// A plain handler function (see [`handler!`](crate::handler)).
pub type Handler = for<'a> fn(&'a Shared, Req, &'a Client) -> BoxFut<'a>;

/// Turn `async fn f(st: &Shared, r: Req, c: &Client) -> HandlerResult` into a [`Handler`].
#[macro_export]
macro_rules! handler {
    ($f:path) => {{
        fn __handler<'a>(
            st: &'a $crate::Shared,
            r: $crate::proxy::Req,
            c: &'a $crate::proxy::Client,
        ) -> $crate::endpoints::BoxFut<'a> {
            Box::pin($f(st, r, c))
        }
        __handler as $crate::endpoints::Handler
    }};
}

pub fn done(r: Response) -> HandlerResult {
    Ok(Flow::Done(r))
}

/// One route (or family of routes).
pub trait Endpoint: Send + Sync {
    fn name(&self) -> &str;
    fn access(&self) -> Access;
    /// `segs` are classified segments (see module docs).
    fn matches(&self, method: &Method, segs: &[&str]) -> bool;
    fn call<'a>(&'a self, st: &'a Shared, r: Req, c: &'a Client) -> BoxFut<'a>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Lit(String),
    Any,
    Rest,
}

/// A parsed route pattern such as `users/{u}/items/**`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern(Vec<Seg>);

impl Pattern {
    pub fn parse(s: &str) -> Self {
        Pattern(
            s.split('/')
                .filter(|p| !p.is_empty())
                .map(|p| match p {
                    "*" => Seg::Any,
                    "**" => Seg::Rest,
                    lit => Seg::Lit(lit.to_ascii_lowercase()),
                })
                .collect(),
        )
    }

    pub fn matches(&self, segs: &[&str]) -> bool {
        fn go(p: &[Seg], s: &[&str]) -> bool {
            match (p.first(), s.first()) {
                (None, None) => true,
                (Some(Seg::Rest), _) => true,
                (None, Some(_)) | (Some(_), None) => false,
                (Some(Seg::Any), Some(_)) => go(&p[1..], &s[1..]),
                (Some(Seg::Lit(l)), Some(x)) => l == x && go(&p[1..], &s[1..]),
            }
        }
        go(&self.0, segs)
    }
}

/// The standard [`Endpoint`]: patterns + method filter + handler fn.
pub struct Route {
    name: String,
    access: Access,
    patterns: Vec<Pattern>,
    methods: Option<Vec<Method>>,
    handler: Handler,
}

impl Route {
    pub fn new(name: impl Into<String>, access: Access, handler: Handler) -> Self {
        Route {
            name: name.into(),
            access,
            patterns: vec![],
            methods: None,
            handler,
        }
    }
    pub fn public(name: impl Into<String>, handler: Handler) -> Self {
        Self::new(name, Access::Public, handler)
    }
    pub fn auth(name: impl Into<String>, handler: Handler) -> Self {
        Self::new(name, Access::Authenticated, handler)
    }
    /// Add a pattern.
    pub fn at(mut self, pattern: &str) -> Self {
        self.patterns.push(Pattern::parse(pattern));
        self
    }
    /// GET and HEAD only.
    pub fn get(self) -> Self {
        self.methods(&[Method::GET, Method::HEAD])
    }
    pub fn post(self) -> Self {
        self.methods(&[Method::POST])
    }
    pub fn methods(mut self, m: &[Method]) -> Self {
        self.methods = Some(m.to_vec());
        self
    }
}

impl Endpoint for Route {
    fn name(&self) -> &str {
        &self.name
    }
    fn access(&self) -> Access {
        self.access
    }
    fn matches(&self, method: &Method, segs: &[&str]) -> bool {
        self.methods.as_ref().is_none_or(|m| m.contains(method))
            && self.patterns.iter().any(|p| p.matches(segs))
    }
    fn call<'a>(&'a self, st: &'a Shared, r: Req, c: &'a Client) -> BoxFut<'a> {
        (self.handler)(st, r, c)
    }
}

/// Ordered list of endpoints; the first match that doesn't decline wins.
#[derive(Default, Clone)]
pub struct Registry {
    list: Vec<Arc<dyn Endpoint>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.names()).finish()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The Jellyfin surface served by the proxy.
    pub fn default_jellyfin() -> Self {
        let mut r = Registry::new();
        r.extend(system::routes());
        r.extend(users::routes());
        r.extend(library::routes());
        r.extend(sessions::routes());
        r.extend(compat::routes());
        r.extend(fallback::routes());
        r
    }

    pub fn add(&mut self, e: impl Endpoint + 'static) -> &mut Self {
        self.list.push(Arc::new(e));
        self
    }
    /// Insert before everything else (overrides built-ins).
    pub fn add_first(&mut self, e: impl Endpoint + 'static) -> &mut Self {
        self.list.insert(0, Arc::new(e));
        self
    }
    pub fn extend(&mut self, it: impl IntoIterator<Item = Route>) -> &mut Self {
        for e in it {
            self.add(e);
        }
        self
    }
    pub fn names(&self) -> Vec<String> {
        self.list.iter().map(|e| e.name().to_string()).collect()
    }
    pub fn len(&self) -> usize {
        self.list.len()
    }
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
    /// Endpoints matching a classified path, in order.
    pub fn route<'a>(
        &'a self,
        method: &'a Method,
        segs: &'a [&'a str],
    ) -> impl Iterator<Item = &'a Arc<dyn Endpoint>> + 'a {
        self.list.iter().filter(move |e| e.matches(method, segs))
    }

    /// Classify, authenticate (lazily, only when a matching route needs it)
    /// and run the first route that handles the request.
    pub async fn dispatch(&self, st: &Shared, r: Req) -> anyhow::Result<Response> {
        let id_stage = crate::telemetry::Stage::start("id_decode");
        let pat: Vec<String> = r.segs.iter().map(|s| forward::classify(st, s)).collect();
        drop(id_stage);
        let p: Vec<&str> = pat.iter().map(|s| s.as_str()).collect();
        let method = r.method.clone();
        let mut client: Option<Client> = None;
        let anon = auth::anon_client(st);
        let mut r = r;
        for ep in self.route(&method, &p) {
            let c = match ep.access() {
                Access::Public => &anon,
                Access::Authenticated => {
                    if client.is_none() {
                        match auth::authenticate(st, &r, &p) {
                            Some(c) => client = Some(c),
                            None => {
                                return Ok(err_resp(
                                    axum::http::StatusCode::UNAUTHORIZED,
                                    "Unauthorized",
                                ))
                            }
                        }
                    }
                    client.as_ref().unwrap()
                }
            };
            match ep.call(st, r, c).await? {
                Flow::Done(resp) => return Ok(resp),
                Flow::Next(back) => r = back,
            }
        }
        Ok(err_resp(axum::http::StatusCode::NOT_FOUND, "Not found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        let p = Pattern::parse("users/{u}/items/**");
        assert!(p.matches(&["users", "{u}", "items"]));
        assert!(p.matches(&["users", "{u}", "items", "{id}", "x"]));
        assert!(!p.matches(&["users", "{id}", "items"]));
        let p = Pattern::parse("displaypreferences/*");
        assert!(p.matches(&["displaypreferences", "{id}"]));
        assert!(!p.matches(&["displaypreferences"]));
        assert!(!p.matches(&["displaypreferences", "a", "b"]));
        assert!(Pattern::parse("").matches(&[]));
        assert!(!Pattern::parse("").matches(&["x"]));
        assert!(Pattern::parse("**").matches(&[]));
    }

    #[test]
    fn default_registry_order_and_methods() {
        let r = Registry::default_jellyfin();
        let names = r.names();
        assert_eq!(names.first().map(String::as_str), Some("root-redirect"));
        assert_eq!(names.last().map(String::as_str), Some("forward"));
        let first =
            |m: Method, segs: &[&str]| r.route(&m, segs).next().map(|e| e.name().to_string());
        assert_eq!(
            first(Method::GET, &["users", "{u}", "views"]).as_deref(),
            Some("views")
        );
        assert_eq!(
            first(Method::POST, &["users", "authenticatebyname"]).as_deref(),
            Some("login")
        );
        // GET-only route doesn't take POST: falls to the generic forward.
        assert_eq!(first(Method::POST, &["items"]).as_deref(), Some("forward"));
        assert_eq!(
            first(Method::GET, &["items", "{id}", "download"]).as_deref(),
            Some("forward")
        );
    }
}
