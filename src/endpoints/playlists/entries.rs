//! A playlist's entries: list, add, remove and move.

use super::*;

pub(super) async fn entries(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let rec = match find(st, &r.segs[1], c) {
        Ok(Some(rec)) => rec,
        Ok(None) => return Ok(Flow::Next(r)),
        Err(e) => return reply(e),
    };
    if r.method != Method::POST && r.method != Method::DELETE {
        return done(proxy::playlist_items(st, &r, c, &rec).await);
    }
    if !caller_only(&r, c) {
        return done(err_resp(StatusCode::FORBIDDEN, FAILED));
    }
    if !rec.can_edit(&c.user_id) {
        return done(forbidden());
    }
    if r.method == Method::DELETE {
        let gone: Vec<String> = q_get(&r.query, "entryIds")
            .map(csv)
            .unwrap_or_default()
            .iter()
            .filter_map(|e| normalize_hex_id(e))
            .collect();
        st.local
            .playlists
            .update(&rec.id, |p| p.items.retain(|i| !gone.contains(i)));
        return done(no_content());
    }
    let ids = q_get(&r.query, "ids").map(csv).unwrap_or_default();
    let Ok(add) = proxy::expand(st, c, &ids).await else {
        return done(err_resp(StatusCode::BAD_REQUEST, FAILED));
    };
    st.local.playlists.update(&rec.id, |p| p.add(&add));
    done(no_content())
}

pub(super) async fn move_entry(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let rec = match find(st, &r.segs[1], c) {
        Ok(Some(rec)) => rec,
        Ok(None) => return Ok(Flow::Next(r)),
        Err(e) => return reply(e),
    };
    if !rec.can_edit(&c.user_id) {
        return done(forbidden());
    }
    let (Some(entry), Ok(n)) = (normalize_hex_id(&r.segs[3]), r.segs[5].parse::<usize>()) else {
        return done(err_resp(StatusCode::BAD_REQUEST, FAILED));
    };
    let user = c.user_id.clone();
    st.local.playlists.update(&rec.id, |p| {
        p.move_item(&entry, n, |i| {
            crate::types::ProxyItemId::parse(i).is_some_and(|p| st.parental.blocks(&user, &p))
        })
    });
    done(no_content())
}
