//! Live TV across servers: `LiveTv/Info`, `LiveTv/GuideInfo` and the
//! `POST LiveTv/Programs` query (the list endpoints are plain
//! [`fan_list`]s over the `LIVE_*` specs).

use super::*;

/// `LiveTv/Info`: services unioned by name, enabled when any server is,
/// and (when enabled) every proxy user may use it.
pub async fn livetv_info(st: &Shared, r: &Req, c: &Client) -> Result<Response> {
    let (queried, raws) = fan_json(st, r, c, "livetv_info", &["LiveTv", "Info"]).await;
    if all_failed(st, queried, raws.len()) {
        return Ok(unavailable());
    }
    let mut services: Vec<Value> = vec![];
    let mut enabled = false;
    for raw in &raws {
        let mut v = raw.value.clone();
        raw.rewrite(st, c, &mut v);
        enabled |= v["IsEnabled"].as_bool().unwrap_or(false);
        for s in v["Services"].as_array().into_iter().flatten() {
            let name = s.get("Name");
            if !services.iter().any(|x| x.get("Name") == name) {
                services.push(s.clone());
            }
        }
    }
    let users = if enabled {
        st.authenticator.ids()
    } else {
        vec![]
    };
    Ok(ok_json(
        json!({"Services": services, "IsEnabled": enabled, "EnabledUsers": users}),
    ))
}

/// Earlier of two Jellyfin timestamps (parsed; text order when unparseable).
fn earlier(a: &str, b: &str) -> bool {
    let p = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
    match (p(a), p(b)) {
        (Some(x), Some(y)) => x < y,
        _ => a < b,
    }
}

/// `LiveTv/GuideInfo`: the guide spans the earliest start to the latest end.
pub async fn livetv_guide_info(st: &Shared, r: &Req, c: &Client) -> Result<Response> {
    let (queried, raws) = fan_json(st, r, c, "livetv_guide", &["LiveTv", "GuideInfo"]).await;
    if all_failed(st, queried, raws.len()) {
        return Ok(unavailable());
    }
    let (mut start, mut end): (Option<String>, Option<String>) = (None, None);
    for raw in &raws {
        if let Some(s) = raw.value["StartDate"].as_str() {
            if start.as_deref().is_none_or(|cur| earlier(s, cur)) {
                start = Some(s.to_string());
            }
        }
        if let Some(e) = raw.value["EndDate"].as_str() {
            if end.as_deref().is_none_or(|cur| earlier(cur, e)) {
                end = Some(e.to_string());
            }
        }
    }
    Ok(ok_json(json!({"StartDate": start, "EndDate": end})))
}

/// The `POST LiveTv/Programs` body for one server, or `None` when the
/// request names channels and none of them live there. Ids are translated;
/// with a `Limit` the window is widened to `StartIndex + Limit` from 0 so
/// the merged list can be paged here.
pub(super) fn program_body(
    ic: &InCtx<'_>,
    body: &Value,
    start: usize,
    limit: Option<usize>,
) -> Option<Value> {
    let mut b = body.clone();
    rewrite::json_in(&mut b, ic);
    let wanted: Vec<&str> = body
        .get("ChannelIds")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if !wanted.is_empty() {
        let mine: Vec<Value> = wanted
            .iter()
            .filter_map(|id| ic.ids.resolve_for(id, ic.upstream))
            .map(|id| Value::String(id.into_string()))
            .collect();
        if mine.is_empty() {
            return None;
        }
        b["ChannelIds"] = Value::Array(mine);
    }
    if let (Some(l), Some(o)) = (limit, b.as_object_mut()) {
        o.insert("StartIndex".into(), 0.into());
        o.insert("Limit".into(), (start + l).into());
    }
    Some(b)
}

/// Append `fields` to the body's `Fields` array unless already there.
fn add_client_fields(body: &mut Value, fields: &[&str]) {
    let Some(o) = body.as_object_mut().filter(|_| !fields.is_empty()) else {
        return;
    };
    let list = o.entry("Fields").or_insert_with(|| Value::Array(vec![]));
    if let Value::Array(a) = list {
        for f in fields {
            if !a
                .iter()
                .any(|x| x.as_str().is_some_and(|x| x.eq_ignore_ascii_case(f)))
            {
                a.push(Value::String((*f).to_string()));
            }
        }
    }
}

/// One server's answer to a programs query.
enum Ask {
    /// None of the requested channels live there.
    Skipped,
    Failed,
    Answer(Value),
}

async fn ask_programs(
    st: &Shared,
    c: &Client,
    up: Arc<dyn MediaBackend>,
    body: &Value,
    user_ids: &[String],
    start: usize,
    limit: Option<usize>,
) -> Ask {
    let user = c.user_id.as_str();
    let uc = match up.login_as(Some(user)).await {
        Ok(uc) => uc,
        Err(e) => {
            tracing::warn!(upstream = %up.name(), "livetv programs: {e}");
            return Ask::Failed;
        }
    };
    let ic = in_ctx(st, up.as_ref(), &uc, user_ids);
    let Some(mut b) = program_body(&ic, body, start, limit) else {
        return Ask::Skipped;
    };
    if let Some(s) = &c.session {
        add_client_fields(&mut b, client_fields::fields(&s.client));
    }
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    let req = BackendRequest::new(Method::POST, "LiveTv/Programs")
        .headers(h)
        .body(Some(Bytes::from(b.to_string())))
        .as_user(Some(user));
    let resp = match up.send(req).await {
        Ok(r) if r.status.is_success() => r,
        Ok(r) => {
            tracing::warn!(upstream = %up.name(), "livetv programs: HTTP {}", r.status);
            return Ask::Failed;
        }
        Err(e) => {
            tracing::warn!(upstream = %up.name(), "livetv programs: {e}");
            return Ask::Failed;
        }
    };
    let parsed = match resp.bytes().await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
        Err(_) => None,
    };
    let Some(mut v) = parsed else {
        return Ask::Failed;
    };
    st.rewriters
        .rewrite_json(&mut v, &out_ctx(st, up.as_ref(), &uc, c));
    Ask::Answer(v)
}

/// `POST LiveTv/Programs`: ask every server that has the requested
/// channels, merge by start date and page.
pub async fn livetv_programs(st: &Shared, r: &Req, c: &Client) -> Result<Response> {
    let body: Value = if r.body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&r.body) {
            Ok(v @ Value::Object(_)) => v,
            _ => return Ok(bad_request()),
        }
    };
    let n = |k: &str| body.get(k).and_then(|v| v.as_u64()).map(|v| v as usize);
    let (start, limit) = (n("StartIndex").unwrap_or(0), n("Limit"));
    let user_ids = st.authenticator.ids();
    let set = st.backends();
    let futs = set
        .iter()
        .filter(|b| !b.is_down())
        .cloned()
        .map(|up| ask_programs(st, c, up, &body, &user_ids, start, limit));
    let answers = join_all(futs).await;
    let asked = answers
        .iter()
        .filter(|a| !matches!(a, Ask::Skipped))
        .count();
    let ok: Vec<Value> = answers
        .into_iter()
        .filter_map(|a| match a {
            Ask::Answer(v) => Some(v),
            _ => None,
        })
        .collect();
    if all_failed(st, asked, ok.len()) {
        return Ok(unavailable());
    }
    if ok.len() < asked {
        counter!(names::FANOUT_PARTIAL, "endpoint" => "livetv_programs").increment(1);
    }
    let mut items: Vec<Value> = vec![];
    let mut total = 0usize;
    for mut v in ok {
        let page = match v.get_mut("Items").map(Value::take) {
            Some(Value::Array(a)) => a,
            _ => vec![],
        };
        total += v["TotalRecordCount"]
            .as_u64()
            .map_or(page.len(), |t| t as usize);
        items.extend(page);
    }
    merge::sort_items(&mut items, &vec![("StartDate".to_string(), false)]);
    let items = merge::page(items, start, limit);
    Ok(ok_json(dto::query_result(items, total, start)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{HashedIdTable, IdMapper};
    use crate::servers::ServerRegistry;
    use crate::types::UpstreamItemId;

    fn setup() -> (HashedIdTable, ServerId, ServerId, String, String) {
        let reg = ServerRegistry::with_names(["a", "b"]);
        let (a, b) = (reg.id_for("a"), reg.id_for("b"));
        let t = HashedIdTable::new(reg);
        let ch_a = t
            .encode(a, &UpstreamItemId::parse(&"1".repeat(32)).unwrap())
            .id
            .into_string();
        let ch_b = t
            .encode(b, &UpstreamItemId::parse(&"2".repeat(32)).unwrap())
            .id
            .into_string();
        (t, a, b, ch_a, ch_b)
    }

    /// Each server must only be asked about its own channels (a foreign
    /// channel id would 404 or match nothing), and with a `Limit` the
    /// window must start at 0 and cover `StartIndex + Limit` so the merged
    /// list can be paged here.
    #[test]
    fn program_body_keeps_own_channels_and_widens_the_window() {
        let (t, a, b, ch_a, ch_b) = setup();
        let users: Vec<String> = vec![];
        let ic = |s| InCtx {
            ids: &t,
            upstream: s,
            upstream_user_id: None,
            proxy_user_ids: &users,
        };
        let body =
            json!({"ChannelIds": [ch_a, ch_b], "StartIndex": 10, "Limit": 5, "HasAired": false});
        let for_a = program_body(&ic(a), &body, 10, Some(5)).unwrap();
        assert_eq!(for_a["ChannelIds"], json!(["1".repeat(32)]));
        assert_eq!(for_a["StartIndex"], 0);
        assert_eq!(for_a["Limit"], 15);
        assert_eq!(for_a["HasAired"], false);
        let for_b = program_body(&ic(b), &json!({"ChannelIds": [ch_a]}), 0, None);
        assert!(for_b.is_none(), "no requested channel lives on b");
        let any = program_body(&ic(b), &json!({"HasAired": true}), 0, None).unwrap();
        assert!(any.get("ChannelIds").is_none() && any.get("Limit").is_none());
    }

    /// The guide window is the earliest start and latest end, compared as
    /// instants even when the fraction lengths differ.
    #[test]
    fn earlier_compares_instants() {
        assert!(earlier(
            "2026-01-01T00:00:00.0000000Z",
            "2026-01-01T00:00:01Z"
        ));
        assert!(!earlier("2026-01-02T00:00:00Z", "2026-01-01T23:59:59.9Z"));
    }
}
