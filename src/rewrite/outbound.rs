//! Outbound rewriting (upstream → client).

use crate::ids::{normalize_hex_id, IdMapper};
use crate::types::ServerId;
use serde_json::Value;

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

pub struct OutCtx<'a> {
    pub ids: &'a dyn IdMapper,
    pub upstream: ServerId,
    pub server_id: &'a str,
    pub proxy_user_id: &'a str,
    pub upstream_user_id: Option<&'a str>,
    pub client_token: &'a str,
    pub upstream_origin: &'a str,
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
