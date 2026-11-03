//! Rewriting contexts and upstream path building.

use super::*;

// ---------------------------------------------------------------------------
// Rewriting contexts
// ---------------------------------------------------------------------------

pub(crate) fn out_ctx<'a>(
    st: &'a Shared,
    up: &'a dyn MediaBackend,
    uc: &'a SessionInfo,
    c: &'a Client,
) -> OutCtx<'a> {
    let loc = up.location();
    OutCtx {
        ids: st.ids.as_ref(),
        upstream: up.id(),
        server_id: &st.server_id,
        proxy_user_id: &c.user_id,
        upstream_user_id: uc.user_id.as_deref(),
        client_token: &c.token,
        upstream_origin: &loc.origin,
        upstream_base_path: &loc.base_path,
    }
}

pub(crate) fn in_ctx<'a>(
    st: &'a Shared,
    up: &'a dyn MediaBackend,
    uc: &'a SessionInfo,
    user_ids: &'a [String],
) -> InCtx<'a> {
    InCtx {
        ids: st.ids.as_ref(),
        upstream: up.id(),
        upstream_user_id: uc.user_id.as_deref(),
        proxy_user_ids: user_ids,
    }
}

pub(crate) fn path_with_query(segs: &[String], q: &[(String, String)]) -> String {
    path_with_query_slash(segs, q, false)
}

pub(super) fn path_with_query_slash(
    segs: &[String],
    q: &[(String, String)],
    slash: bool,
) -> String {
    let mut s = segs.join("/");
    if slash && !s.is_empty() {
        s.push('/');
    }
    if !q.is_empty() {
        s.push('?');
        s.push_str(&rewrite::encode_pairs(q));
    }
    s
}
