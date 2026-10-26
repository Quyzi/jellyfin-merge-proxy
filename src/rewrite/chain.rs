//! Response rewriter chain and implementations.

use super::outbound::{json_out, playlist_out, url_out, OutCtx};
use super::{Payload, PayloadBody};
use axum::http::{header, HeaderValue};

pub trait ResponseRewriter: Send + Sync {
    fn name(&self) -> &'static str;
    fn applies(&self, p: &Payload<'_>) -> bool;
    fn rewrite(&self, p: &mut Payload<'_>, cx: &OutCtx<'_>);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct JsonIdRewriter;

impl ResponseRewriter for JsonIdRewriter {
    fn name(&self) -> &'static str {
        "json_ids"
    }
    fn applies(&self, p: &Payload<'_>) -> bool {
        matches!(p.body, Some(PayloadBody::Json(_)))
    }
    fn rewrite(&self, p: &mut Payload<'_>, cx: &OutCtx<'_>) {
        if let Some(PayloadBody::Json(v)) = &mut p.body {
            json_out(v, cx);
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct HlsPlaylistRewriter;

impl ResponseRewriter for HlsPlaylistRewriter {
    fn name(&self) -> &'static str {
        "hls"
    }
    fn applies(&self, p: &Payload<'_>) -> bool {
        matches!(p.body, Some(PayloadBody::Text(_))) && p.is_playlist()
    }
    fn rewrite(&self, p: &mut Payload<'_>, cx: &OutCtx<'_>) {
        if let Some(PayloadBody::Text(t)) = &mut p.body {
            *t = playlist_out(t, cx);
        }
        let ct = if p.content_type.is_empty() {
            "application/vnd.apple.mpegurl"
        } else {
            p.content_type.as_str()
        };
        p.headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(ct)
                .unwrap_or(HeaderValue::from_static("application/vnd.apple.mpegurl")),
        );
        p.headers
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct LocationHeaderRewriter;

impl ResponseRewriter for LocationHeaderRewriter {
    fn name(&self) -> &'static str {
        "location"
    }
    fn applies(&self, p: &Payload<'_>) -> bool {
        p.headers.contains_key(header::LOCATION)
    }
    fn rewrite(&self, p: &mut Payload<'_>, cx: &OutCtx<'_>) {
        let Some(loc) = p
            .headers
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
        else {
            return;
        };
        match HeaderValue::from_str(&url_out(&loc, cx, false)) {
            Ok(v) => {
                p.headers.insert(header::LOCATION, v);
            }
            Err(_) => {
                p.headers.remove(header::LOCATION);
            }
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MeteredRewriter<R>(pub R);

impl<R: ResponseRewriter> ResponseRewriter for MeteredRewriter<R> {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn applies(&self, p: &Payload<'_>) -> bool {
        self.0.applies(p)
    }
    fn rewrite(&self, p: &mut Payload<'_>, cx: &OutCtx<'_>) {
        self.0.rewrite(p, cx);
        metrics::counter!(crate::telemetry::names::REWRITES, "rewriter" => self.0.name())
            .increment(1);
    }
}

pub struct RewriteChain {
    stages: Vec<Box<dyn ResponseRewriter>>,
}

impl std::fmt::Debug for RewriteChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.names()).finish()
    }
}

impl Default for RewriteChain {
    fn default() -> Self {
        Self::default_chain()
    }
}

impl RewriteChain {
    pub fn empty() -> Self {
        RewriteChain { stages: Vec::new() }
    }
    pub fn default_chain() -> Self {
        Self::empty()
            .with(MeteredRewriter(JsonIdRewriter))
            .with(MeteredRewriter(HlsPlaylistRewriter))
            .with(MeteredRewriter(LocationHeaderRewriter))
    }
    pub fn with(mut self, r: impl ResponseRewriter + 'static) -> Self {
        self.stages.push(Box::new(r));
        self
    }
    pub fn push(&mut self, r: Box<dyn ResponseRewriter>) {
        self.stages.push(r);
    }
    pub fn names(&self) -> Vec<&'static str> {
        self.stages.iter().map(|s| s.name()).collect()
    }
    pub fn apply(&self, p: &mut Payload<'_>, cx: &OutCtx<'_>) -> Vec<&'static str> {
        let mut ran = vec![];
        for s in &self.stages {
            if s.applies(p) {
                s.rewrite(p, cx);
                ran.push(s.name());
            }
        }
        ran
    }
    pub fn rewrite_json(&self, v: &mut serde_json::Value, cx: &OutCtx<'_>) {
        let mut h = axum::http::HeaderMap::new();
        let mut p = Payload {
            content_type: "application/json".into(),
            path: String::new(),
            headers: &mut h,
            body: Some(PayloadBody::Json(std::mem::take(v))),
        };
        self.apply(&mut p, cx);
        if let Some(PayloadBody::Json(out)) = p.body {
            *v = out;
        }
    }
}
