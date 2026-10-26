//! Rewriting of ids and URLs in JSON bodies, query strings and HLS playlists.
//!
//! The free functions ([`json_out`], [`url_out`], [`playlist_out`],
//! [`json_in`]) do the work; [`ResponseRewriter`] wraps them as composable
//! stages ([`JsonIdRewriter`], [`HlsPlaylistRewriter`],
//! [`LocationHeaderRewriter`]) run by a [`RewriteChain`]. Add a stage by
//! implementing the trait and pushing it onto the chain; wrap any stage in
//! [`MeteredRewriter`] to count it.

use axum::http::HeaderMap;
use serde_json::Value;

pub mod chain;
pub mod inbound;
pub mod outbound;

pub use chain::{
    HlsPlaylistRewriter, JsonIdRewriter, LocationHeaderRewriter, MeteredRewriter, ResponseRewriter,
    RewriteChain,
};
pub use inbound::{json_in, InCtx};
pub use outbound::{encode_pairs, json_out, playlist_out, url_out, OutCtx};

#[derive(Debug, Clone, PartialEq)]
pub enum PayloadBody {
    Json(Value),
    Text(String),
}

#[derive(Debug)]
pub struct Payload<'p> {
    pub content_type: String,
    pub path: String,
    pub headers: &'p mut HeaderMap,
    pub body: Option<PayloadBody>,
}

impl Payload<'_> {
    pub fn is_playlist(&self) -> bool {
        is_playlist(&self.content_type, &self.path)
    }
}

pub fn is_playlist(content_type: &str, path: &str) -> bool {
    content_type.contains("mpegurl") || (path.ends_with(".m3u8") && !content_type.contains("json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{IdMap, IdMapper};
    use crate::types::ServerId;
    use axum::http::{header, HeaderValue};
    use serde_json::json;

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
