//! [`CanonicalIdRewriter`]: show one id per grouped item.
//!
//! Runs after the JSON id rewriter: every item/genre/person/studio id in a
//! response that belongs to a group (copies on several servers, names
//! merged across servers) is replaced by the group's canonical id, so a
//! client sees one movie, one series, one season and one "Drama" whichever
//! server answered. Media source ids are left alone: they name one
//! concrete copy (and its server) for playback.

use super::groups::GroupLookup;
use crate::rewrite::{OutCtx, Payload, PayloadBody, ResponseRewriter};
use crate::types::ProxyItemId;
use serde_json::Value;
use std::sync::Arc;

pub struct CanonicalIdRewriter {
    groups: Arc<dyn GroupLookup>,
}

impl CanonicalIdRewriter {
    pub fn new(groups: Arc<dyn GroupLookup>) -> Self {
        CanonicalIdRewriter { groups }
    }

    fn map(&self, s: &mut String) {
        if let Some(c) = ProxyItemId::parse(s)
            .and_then(|p| self.groups.group(&p))
            .map(|g| g.canonical.clone())
        {
            if c.as_str() != s {
                *s = c.into_string();
            }
        }
    }

    /// Rewrite ids in `v` (public for list builders that bypass the chain).
    pub fn apply(&self, v: &mut Value) {
        match v {
            Value::Object(map) => {
                for (k, val) in map.iter_mut() {
                    let kl = k.to_ascii_lowercase();
                    if kl == "mediasources" || kl == "mediasourceid" || kl == "playsessionid" {
                        continue;
                    }
                    if kl.ends_with("id") {
                        if let Value::String(s) = val {
                            self.map(s);
                            continue;
                        }
                    }
                    if kl.ends_with("ids") {
                        if let Value::Array(a) = val {
                            for e in a.iter_mut() {
                                if let Value::String(s) = e {
                                    self.map(s);
                                }
                            }
                            continue;
                        }
                    }
                    self.apply(val);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|e| self.apply(e)),
            _ => {}
        }
    }
}

impl ResponseRewriter for CanonicalIdRewriter {
    fn name(&self) -> &'static str {
        "canonical_ids"
    }
    fn applies(&self, p: &Payload<'_>) -> bool {
        matches!(p.body, Some(PayloadBody::Json(_)))
    }
    fn rewrite(&self, p: &mut Payload<'_>, _cx: &OutCtx<'_>) {
        if let Some(PayloadBody::Json(v)) = &mut p.body {
            self.apply(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::groups::{CanonicalGroups, GroupMap};
    use serde_json::json;

    fn id(n: u8) -> ProxyItemId {
        ProxyItemId::parse(&format!("{n:032x}")).unwrap()
    }

    #[test]
    fn replaces_member_ids_but_not_media_sources() {
        let g = Arc::new(CanonicalGroups::new());
        let mut m = GroupMap::new();
        m.insert(id(1), vec![id(1), id(2)]);
        g.publish(m);
        let rw = CanonicalIdRewriter::new(g);
        let mut v = json!({
            "Id": id(2).as_str(), "SeriesId": id(2).as_str(), "Other": id(3).as_str(),
            "GenreItems": [{"Id": id(2).as_str()}],
            "MediaSources": [{"Id": id(2).as_str()}], "MediaSourceId": id(2).as_str(),
            "Ids": [id(2).as_str()]
        });
        rw.apply(&mut v);
        assert_eq!(v["Id"], id(1).as_str());
        assert_eq!(v["SeriesId"], id(1).as_str());
        assert_eq!(v["Other"], id(3).as_str());
        assert_eq!(v["GenreItems"][0]["Id"], id(1).as_str());
        assert_eq!(v["MediaSources"][0]["Id"], id(2).as_str());
        assert_eq!(v["MediaSourceId"], id(2).as_str());
        assert_eq!(v["Ids"][0], id(1).as_str());
    }
}
