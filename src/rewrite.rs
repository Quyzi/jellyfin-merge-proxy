//! Rewriting of ids and URLs in JSON bodies, query strings and HLS playlists.
//!
//! The free functions ([`json_out`], [`url_out`], [`playlist_out`],
//! [`json_in`]) do the work; [`ResponseRewriter`] wraps them as composable
//! stages ([`JsonIdRewriter`], [`HlsPlaylistRewriter`],
//! [`LocationHeaderRewriter`]) run by a [`RewriteChain`]. Add a stage by
//! implementing the trait and pushing it onto the chain; wrap any stage in
//! [`MeteredRewriter`] to count it.

use crate::ids::{normalize_hex_id, IdMapper};
use crate::types::ServerId;
use axum::http::{header, HeaderMap, HeaderValue};
use serde_json::Value;

/// Keys ending in `Id` that are *not* item ids and must be left alone.
const SKIP_ID_KEYS: &[&str] = &[
    "playsessionid",
    "deviceid",
    "sessionid",
    "livestreamid",
    "transcodingid",
    "castreceiverid",
    "authenticationproviderid",
    "passwordresetproviderid",
    "messageid",
    "appid",
    "providerids",
    "externalid",
    "tvdbid",
    "imdbid",
    "tmdbid",
    "etag",
];

fn is_skipped(key_lc: &str) -> bool {
    SKIP_ID_KEYS.contains(&key_lc)
}

/// Context for rewriting upstream -> client.
pub struct OutCtx<'a> {
    pub ids: &'a dyn IdMapper,
    pub upstream: ServerId,
    pub server_id: &'a str,
    pub proxy_user_id: &'a str,
    pub upstream_user_id: Option<&'a str>,
    /// Token of the requesting client (substituted for upstream tokens in URLs).
    pub client_token: &'a str,
    /// Upstream origin, e.g. "http://10.0.0.2:8096" (no trailing slash).
    pub upstream_origin: &'a str,
    /// Upstream base path, e.g. "" or "/jellyfin" (no trailing slash).
    pub upstream_base_path: &'a str,
}

impl OutCtx<'_> {
    fn map_id(&self, s: &str) -> String {
        if let (Some(uu), Some(n)) = (self.upstream_user_id, normalize_hex_id(s)) {
            if normalize_hex_id(uu).as_deref() == Some(n.as_str()) {
                return self.proxy_user_id.to_string();
            }
        }
        self.ids.to_proxy(self.upstream, s)
    }
}

/// Rewrite an upstream JSON document for the client, recursively.
pub fn json_out(v: &mut Value, ctx: &OutCtx) {
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                let kl = k.to_ascii_lowercase();
                if kl == "serverid" {
                    if val.is_string() {
                        *val = Value::String(ctx.server_id.to_string());
                    }
                    continue;
                }
                if kl == "userid" {
                    if val.as_str().map(|s| normalize_hex_id(s).is_some()) == Some(true) {
                        *val = Value::String(ctx.proxy_user_id.to_string());
                    }
                    continue;
                }
                if is_skipped(&kl) {
                    continue;
                }
                if kl.ends_with("id") {
                    if let Value::String(s) = val {
                        if normalize_hex_id(s).is_some() {
                            *s = ctx.map_id(s);
                        }
                        continue;
                    }
                }
                if kl.ends_with("ids") {
                    if let Value::Array(arr) = val {
                        for e in arr.iter_mut() {
                            if let Value::String(s) = e {
                                if normalize_hex_id(s).is_some() {
                                    *s = ctx.map_id(s);
                                }
                            }
                        }
                        continue;
                    }
                }
                if kl.ends_with("url") {
                    if let Value::String(s) = val {
                        *s = url_out(s, ctx, false);
                        continue;
                    }
                }
                json_out(val, ctx);
            }
        }
        Value::Array(arr) => {
            for e in arr {
                json_out(e, ctx);
            }
        }
        _ => {}
    }
}

fn is_token_key(k: &str) -> bool {
    matches!(
        k.to_ascii_lowercase().as_str(),
        "api_key" | "apikey" | "x-emby-token" | "x-mediabrowser-token"
    )
}

/// Rewrite a URL produced by an upstream so that it points at the proxy.
/// Absolute URLs to the upstream become absolute paths on the proxy;
/// foreign absolute URLs are left alone. When `ensure_token` is set an
/// `api_key` with the client's token is always present.
pub fn url_out(u: &str, ctx: &OutCtx, ensure_token: bool) -> String {
    let rest: &str;
    let mut prefix_slash = false;
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let origin = ctx.upstream_origin.to_ascii_lowercase();
        if !origin.is_empty() && lower.starts_with(&origin) {
            rest = &u[origin.len()..];
            prefix_slash = true;
        } else {
            return u.to_string();
        }
    } else {
        rest = u;
    }
    let mut rest = rest.to_string();
    if rest.starts_with('/') || prefix_slash {
        prefix_slash = true;
        let bp = ctx.upstream_base_path;
        if !bp.is_empty()
            && rest
                .to_ascii_lowercase()
                .starts_with(&bp.to_ascii_lowercase())
        {
            rest = rest[bp.len()..].to_string();
        }
        rest = rest.trim_start_matches('/').to_string();
    }
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (rest.clone(), None),
    };
    let new_path: Vec<String> = path
        .split('/')
        .map(|seg| {
            if normalize_hex_id(seg).is_some() {
                ctx.map_id(seg)
            } else {
                seg.to_string()
            }
        })
        .collect();
    let mut out = String::new();
    if prefix_slash {
        out.push('/');
    }
    out.push_str(&new_path.join("/"));
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut had_token = false;
    if let Some(q) = &query {
        for (k, v) in form_urlencoded::parse(q.as_bytes()) {
            let k = k.into_owned();
            let kl = k.to_ascii_lowercase();
            if is_token_key(&k) {
                had_token = true;
                pairs.push(("api_key".into(), ctx.client_token.to_string()));
                continue;
            }
            let v = if kl == "userid" {
                if normalize_hex_id(&v).is_some() {
                    ctx.proxy_user_id.to_string()
                } else {
                    v.into_owned()
                }
            } else if (kl.ends_with("id") || kl.ends_with("ids")) && !is_skipped(&kl) {
                map_list(&v, |s| ctx.map_id(s))
            } else {
                v.into_owned()
            };
            pairs.push((k, v));
        }
    }
    if ensure_token && !had_token {
        pairs.push(("api_key".into(), ctx.client_token.to_string()));
    }
    if !pairs.is_empty() || query.is_some() {
        out.push('?');
        out.push_str(&encode_pairs(&pairs));
    }
    out
}

fn map_list(v: &str, f: impl Fn(&str) -> String) -> String {
    v.split(',')
        .map(|s| {
            if normalize_hex_id(s).is_some() {
                f(s)
            } else {
                s.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub fn encode_pairs(pairs: &[(String, String)]) -> String {
    let mut s = form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish()
}

/// Rewrite every URI in an HLS playlist (both plain lines and URI="..." attributes).
pub fn playlist_out(text: &str, ctx: &OutCtx) -> String {
    let mut out = String::with_capacity(text.len() + 256);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            out.push_str(line);
        } else if trimmed.starts_with('#') {
            out.push_str(&rewrite_uri_attrs(line, ctx));
        } else {
            out.push_str(&url_out(trimmed, ctx, true));
        }
        out.push('\n');
    }
    out
}

fn rewrite_uri_attrs(line: &str, ctx: &OutCtx) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(pos) = rest.find("URI=\"") {
        let start = pos + 5;
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        if let Some(end) = after.find('"') {
            out.push_str(&url_out(&after[..end], ctx, true));
            rest = &after[end..];
        } else {
            rest = after;
            break;
        }
    }
    out.push_str(rest);
    out
}

/// Context for rewriting client -> upstream.
pub struct InCtx<'a> {
    pub ids: &'a dyn IdMapper,
    pub upstream: ServerId,
    pub upstream_user_id: Option<&'a str>,
    /// All proxy user ids (any of them maps to the upstream user).
    pub proxy_user_ids: &'a [String],
}

impl InCtx<'_> {
    pub fn map(&self, s: &str) -> String {
        if let Some(n) = normalize_hex_id(s) {
            if self.proxy_user_ids.contains(&n) {
                if let Some(u) = self.upstream_user_id {
                    return u.to_string();
                }
            }
            if let Some(id) = self.ids.resolve_for(&n, self.upstream) {
                return id.into_string();
            }
        }
        s.to_string()
    }
    pub fn map_list(&self, v: &str) -> String {
        map_list(v, |s| self.map(s))
    }
}

/// Rewrite ids in a client JSON body (e.g. playstate reports) for an upstream.
pub fn json_in(v: &mut Value, ctx: &InCtx) {
    match v {
        Value::Object(map) => {
            for (_, val) in map.iter_mut() {
                json_in(val, ctx);
            }
        }
        Value::Array(a) => {
            for e in a {
                json_in(e, ctx);
            }
        }
        Value::String(s) if normalize_hex_id(s).is_some() => {
            *s = ctx.map(s);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Response rewriter chain
// ---------------------------------------------------------------------------

/// A (possibly buffered) response body.
#[derive(Debug, Clone, PartialEq)]
pub enum PayloadBody {
    Json(Value),
    Text(String),
}

/// What a rewriter sees: the response headers, and the body when it was
/// buffered (JSON and playlists; media streams pass with `body: None`).
#[derive(Debug)]
pub struct Payload<'p> {
    /// Lower-cased content type ("" if none).
    pub content_type: String,
    /// Client request path (lower-cased).
    pub path: String,
    pub headers: &'p mut HeaderMap,
    pub body: Option<PayloadBody>,
}

impl Payload<'_> {
    pub fn is_playlist(&self) -> bool {
        is_playlist(&self.content_type, &self.path)
    }
}

/// Is this response an HLS playlist?
pub fn is_playlist(content_type: &str, path: &str) -> bool {
    content_type.contains("mpegurl") || (path.ends_with(".m3u8") && !content_type.contains("json"))
}

/// One stage of response rewriting.
pub trait ResponseRewriter: Send + Sync {
    fn name(&self) -> &'static str;
    fn applies(&self, p: &Payload<'_>) -> bool;
    fn rewrite(&self, p: &mut Payload<'_>, cx: &OutCtx<'_>);
}

/// Item/user/server ids and URLs inside JSON documents.
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

/// URIs inside HLS playlists (segments get the client's token).
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

/// `Location` headers pointing at the upstream.
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

/// Counts applications of any rewriter (`jmp_rewrites_total`).
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

/// Ordered list of rewriters (dyn: assembled at runtime).
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
    /// JSON ids, HLS playlists, Location headers; each metered.
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
    /// Run every applicable stage; returns the names that ran.
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
    /// Convenience: rewrite a bare JSON value through the chain.
    pub fn rewrite_json(&self, v: &mut Value, cx: &OutCtx<'_>) {
        let mut h = HeaderMap::new();
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::ids::IdMap;

    fn ctx(ids: &IdMap) -> OutCtx<'_> {
        OutCtx {
            ids,
            upstream: ServerId(0),
            server_id: "11111111111111111111111111111111",
            proxy_user_id: "22222222222222222222222222222222",
            upstream_user_id: Some("33333333333333333333333333333333"),
            client_token: "CLIENTTOKEN",
            upstream_origin: "http://up:8096",
            upstream_base_path: "/jf",
        }
    }

    #[test]
    fn json_ids_rewritten_recursively() {
        let ids = IdMap::new(crate::servers::ServerRegistry::with_names(["a"]));
        let c = ctx(&ids);
        let item = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let series = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let tag = "cccccccccccccccccccccccccccccccc";
        let mut v = json!({
            "Items": [{
                "Id": item, "SeriesId": series, "ServerId": "upstreamserver",
                "UserData": {"Key": "x", "ItemId": item},
                "ImageTags": {"Primary": tag},
                "PrimaryImageTag": tag,
                "ProviderIds": {"Tmdb": "123"},
                "PlaySessionId": "dddddddddddddddddddddddddddddddd",
                "MediaSources": [{"Id": item, "TranscodingUrl":
                    "/jf/videos/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/master.m3u8?MediaSourceId=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&api_key=UPTOKEN&Tag=cccccccccccccccccccccccccccccccc"}],
                "BackdropImageTags": [tag],
                "ArtistItems": [{"Name": "x", "Id": series}],
            }],
            "TotalRecordCount": 1
        });
        json_out(&mut v, &c);
        let pi = ids.to_proxy(ServerId(0), item);
        let ps = ids.to_proxy(ServerId(0), series);
        let it = &v["Items"][0];
        assert_eq!(it["Id"], pi);
        assert_eq!(it["SeriesId"], ps);
        assert_eq!(it["ServerId"], c.server_id);
        assert_eq!(it["UserData"]["ItemId"], pi);
        assert_eq!(it["ImageTags"]["Primary"], tag);
        assert_eq!(it["PrimaryImageTag"], tag);
        assert_eq!(it["BackdropImageTags"][0], tag);
        assert_eq!(it["PlaySessionId"], "dddddddddddddddddddddddddddddddd");
        assert_eq!(it["ArtistItems"][0]["Id"], ps);
        assert_eq!(it["MediaSources"][0]["Id"], pi);
        let url = it["MediaSources"][0]["TranscodingUrl"].as_str().unwrap();
        assert_eq!(
            url,
            format!("/videos/{pi}/master.m3u8?MediaSourceId={pi}&api_key=CLIENTTOKEN&Tag={tag}")
        );
        // and back again
        let users = vec![c.proxy_user_id.to_string()];
        let inc = InCtx {
            ids: &ids,
            upstream: ServerId(0),
            upstream_user_id: c.upstream_user_id,
            proxy_user_ids: &users,
        };
        let mut body = json!({"ItemId": pi, "MediaSourceId": pi, "UserId": c.proxy_user_id, "PlaySessionId": "zz"});
        json_in(&mut body, &inc);
        assert_eq!(body["ItemId"], item);
        assert_eq!(body["UserId"], "33333333333333333333333333333333");
        assert_eq!(body["PlaySessionId"], "zz");
    }

    #[test]
    fn playlist_rewrite() {
        let ids = IdMap::new(crate::servers::ServerRegistry::with_names(["a"]));
        let c = ctx(&ids);
        let item = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let pi = ids.to_proxy(ServerId(0), item);
        let pl = "#EXTM3U\n#EXT-X-MAP:URI=\"hls1/main/-1.mp4?MediaSourceId=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n#EXTINF:6.0,\nhls1/main/0.ts?MediaSourceId=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&api_key=UP\nhttp://up:8096/jf/videos/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/hls1/main/1.ts\nhttps://cdn.example/x.ts\n";
        let out = playlist_out(pl, &c);
        assert!(out.contains(&format!(
            "URI=\"hls1/main/-1.mp4?MediaSourceId={pi}&api_key=CLIENTTOKEN\""
        )));
        assert!(out.contains(&format!(
            "\nhls1/main/0.ts?MediaSourceId={pi}&api_key=CLIENTTOKEN\n"
        )));
        assert!(out.contains(&format!(
            "\n/videos/{pi}/hls1/main/1.ts?api_key=CLIENTTOKEN\n"
        )));
        assert!(out.contains("\nhttps://cdn.example/x.ts\n"));
        assert!(!out.contains("UP\n"));
    }

    #[test]
    fn chain_runs_applicable_stages() {
        let ids = IdMap::new(crate::servers::ServerRegistry::with_names(["a"]));
        let c = ctx(&ids);
        let chain = RewriteChain::default_chain();
        assert_eq!(chain.names(), vec!["json_ids", "hls", "location"]);
        let item = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut h = HeaderMap::new();
        h.insert(
            header::LOCATION,
            HeaderValue::from_static("http://up:8096/jf/Items/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let mut p = Payload {
            content_type: "application/json".into(),
            path: "/items".into(),
            headers: &mut h,
            body: Some(PayloadBody::Json(serde_json::json!({"Id": item}))),
        };
        let ran = chain.apply(&mut p, &c);
        assert_eq!(ran, vec!["json_ids", "location"]);
        let pi = ids.to_proxy(ServerId(0), item);
        assert_eq!(
            p.body,
            Some(PayloadBody::Json(serde_json::json!({"Id": pi})))
        );
        assert_eq!(h[header::LOCATION], format!("/Items/{pi}").as_str());

        let mut h = HeaderMap::new();
        let mut p = Payload {
            content_type: String::new(),
            path: "/videos/x/main.m3u8".into(),
            headers: &mut h,
            body: Some(PayloadBody::Text("seg.ts\n".into())),
        };
        assert_eq!(chain.apply(&mut p, &c), vec!["hls"]);
        assert_eq!(
            p.body,
            Some(PayloadBody::Text("seg.ts?api_key=CLIENTTOKEN\n".into()))
        );
        assert_eq!(h[header::CACHE_CONTROL], "no-cache");
    }

    struct Upper;
    impl ResponseRewriter for Upper {
        fn name(&self) -> &'static str {
            "upper"
        }
        fn applies(&self, p: &Payload<'_>) -> bool {
            matches!(p.body, Some(PayloadBody::Text(_)))
        }
        fn rewrite(&self, p: &mut Payload<'_>, _cx: &OutCtx<'_>) {
            if let Some(PayloadBody::Text(t)) = &mut p.body {
                *t = t.to_uppercase();
            }
        }
    }

    #[test]
    fn custom_stage_composes() {
        let ids = IdMap::new(crate::servers::ServerRegistry::with_names(["a"]));
        let c = ctx(&ids);
        let chain = RewriteChain::empty().with(MeteredRewriter(Upper));
        let mut h = HeaderMap::new();
        let mut p = Payload {
            content_type: "text/plain".into(),
            path: "/x".into(),
            headers: &mut h,
            body: Some(PayloadBody::Text("hi".into())),
        };
        assert_eq!(chain.apply(&mut p, &c), vec!["upper"]);
        assert_eq!(p.body, Some(PayloadBody::Text("HI".into())));
    }
}
