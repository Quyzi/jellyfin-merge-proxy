//! Inbound rewriting (client → upstream).

use crate::ids::{normalize_hex_id, IdMapper};
use crate::types::ServerId;
use serde_json::Value;

pub struct InCtx<'a> {
    pub ids: &'a dyn IdMapper,
    pub upstream: ServerId,
    pub upstream_user_id: Option<&'a str>,
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
