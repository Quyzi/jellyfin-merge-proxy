//! Items, merged libraries and fan-out lists.

use super::{done, Flow, HandlerResult, Route};
use crate::dto;
use crate::handler;
use crate::ids::Target;
use crate::proxy::{self, ok_json, q_get, Client, ListSpec, Req};
use crate::Shared;

pub fn routes() -> Vec<Route> {
    vec![
        Route::auth("root-folder", handler!(root))
            .at("users/{u}/items/{root}")
            .at("items/{root}")
            .at("items/root")
            .at("users/{u}/items/root")
            .get(),
        Route::auth("merged-view", handler!(view))
            .at("users/{u}/items/{view}")
            .at("items/{view}")
            .get(),
        Route::auth("merged-view-image", handler!(view_image)).at("items/{view}/images/**"),
        Route::auth("items", handler!(items))
            .at("users/{u}/items")
            .at("items")
            .get(),
        Route::auth("latest", handler!(latest))
            .at("users/{u}/items/latest")
            .at("items/latest")
            .get(),
        Route::auth("resume", handler!(resume))
            .at("users/{u}/items/resume")
            .at("useritems/resume")
            .get(),
        Route::auth("show-seasons", handler!(seasons))
            .at("shows/{id}/seasons")
            .get(),
        Route::auth("show-episodes", handler!(episodes))
            .at("shows/{id}/episodes")
            .get(),
        Route::auth("nextup", handler!(nextup))
            .at("shows/nextup")
            .get(),
        Route::auth("search", handler!(search))
            .at("search/hints")
            .get(),
        Route::auth("named-lists", handler!(named))
            .at("genres")
            .at("musicgenres")
            .at("studios")
            .at("persons")
            .at("artists")
            .at("artists/albumartists")
            .get(),
        Route::auth("filters", handler!(filters))
            .at("items/filters")
            .at("items/filters2")
            .get(),
        Route::auth("counts", handler!(counts))
            .at("items/counts")
            .get(),
    ]
}

async fn root(st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(ok_json(dto::root_folder(&st.root_id, &st.server_id)))
}

async fn view(st: &Shared, r: Req, _c: &Client) -> HandlerResult {
    if let Some(Target::View(v)) = r.segs.last().and_then(|s| st.ids.resolve(s)) {
        return done(ok_json(v.template.clone()));
    }
    Ok(Flow::Next(r))
}

async fn view_image(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    if let Some(Target::View(v)) = st.ids.resolve(&r.segs[1]) {
        let (u, mid) = v.members[0].clone();
        let mut segs = vec!["Items".to_string(), mid.into_string()];
        segs.extend(r.segs[2..].iter().cloned());
        return done(proxy::forward(st, u, r, c, Some(segs)).await?);
    }
    Ok(Flow::Next(r))
}

async fn items(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    if q_get(&r.query, "ParentId")
        .and_then(crate::ids::normalize_hex_id)
        .as_deref()
        == Some(st.root_id.as_str())
    {
        let v = proxy::views(st, c, false).await;
        let n = v.len();
        return done(ok_json(dto::query_result(v, n, 0)));
    }
    if let Some(res) = proxy::indexed_items(st, &r, c, "items", &r.query).await {
        return done(res?);
    }
    done(proxy::fan_list(st, &r, c, &proxy::ITEMS).await?)
}

async fn latest(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::fan_list(st, &r, c, &proxy::LATEST).await?)
}

async fn resume(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    if let Some(res) = proxy::indexed_resume(st, &r, c).await {
        return done(res?);
    }
    done(proxy::fan_list(st, &r, c, &proxy::RESUME).await?)
}

async fn nextup(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    if let Some(res) = proxy::indexed_nextup(st, &r, c).await {
        return done(res?);
    }
    done(proxy::fan_list(st, &r, c, &proxy::NEXTUP).await?)
}

async fn seasons(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    if let Some(res) = proxy::indexed_show_children(st, &r, c, false).await {
        return done(res?);
    }
    match proxy::show_children(st, &r, c, &proxy::SEASONS).await {
        Some(res) => done(res?),
        None => Ok(Flow::Next(r)),
    }
}

async fn episodes(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    if let Some(res) = proxy::indexed_show_children(st, &r, c, true).await {
        return done(res?);
    }
    match proxy::show_children(st, &r, c, &proxy::EPISODES).await {
        Some(res) => done(res?),
        None => Ok(Flow::Next(r)),
    }
}

async fn search(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::fan_list(st, &r, c, &proxy::SEARCH).await?)
}

async fn named(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    if let Some(kind) = r.segs.first().and_then(|s| proxy::name_kind(s)) {
        if let Some(res) = proxy::indexed_names(st, &r, c, kind).await {
            return done(res?);
        }
    }
    let spec = ListSpec {
        path: r.segs.clone(),
        ..proxy::NAMED
    };
    done(proxy::fan_list(st, &r, c, &spec).await?)
}

async fn filters(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let v2 = r
        .segs
        .get(1)
        .is_some_and(|s| s.eq_ignore_ascii_case("filters2"));
    if let Some(mut v) = proxy::indexed_filters(st, &r, c, v2).await {
        if v2 {
            // Values the index does not keep (audio/subtitle languages,
            // newer servers' fields) come from the live servers.
            let live = proxy::fan_merge_value(st, &r, c, false).await;
            if let (serde_json::Value::Object(m), serde_json::Value::Object(l)) = (&mut v, live) {
                for (k, mut val) in l {
                    if let serde_json::Value::Array(a) = &mut val {
                        a.sort_by(|x, y| x["Name"].as_str().cmp(&y["Name"].as_str()));
                    }
                    m.entry(k).or_insert(val);
                }
            }
        }
        return done(ok_json(v));
    }
    done(proxy::fan_merge_objects(st, &r, c, false).await?)
}

async fn counts(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::fan_merge_objects(st, &r, c, true).await?)
}
