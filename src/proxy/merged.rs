//! Endpoints that used to reach only one (the primary) server and now
//! answer for all of them: movie recommendations, Live TV, the name detail
//! routes (`Genres/{name}`, `Persons/{name}`, ...) and play-session routing.
//!
//! The list endpoints reuse [`fan_list`] with their own specs; the object
//! endpoints share [`fan_json`], which asks every target, rewrites nothing
//! itself and reports how many servers were asked.

use super::*;

mod livetv;
mod name_detail;
mod recs;
mod sessions;

pub use livetv::*;
pub use name_detail::*;
pub use recs::*;
pub use sessions::*;

/// One upstream's successful answer to a fanned-out GET (not yet rewritten).
pub(super) struct Raw {
    pub up: Arc<dyn MediaBackend>,
    pub uc: SessionInfo,
    pub value: Value,
}

impl Raw {
    /// Rewrite ids/urls in `v` (a part of this answer) for the client.
    pub(super) fn rewrite(&self, st: &Shared, c: &Client, v: &mut Value) {
        st.rewriters
            .rewrite_json(v, &out_ctx(st, self.up.as_ref(), &self.uc, c));
    }
}

pub(super) fn unavailable() -> Response {
    err_resp(StatusCode::SERVICE_UNAVAILABLE, "upstream unavailable")
}

pub(super) fn bad_request() -> Response {
    err_resp(
        StatusCode::BAD_REQUEST,
        "One or more validation errors occurred.",
    )
}

/// Nothing answered although something should have: every queried server
/// failed, or no server is up at all.
pub(super) fn all_failed(st: &Shared, queried: usize, answered: usize) -> bool {
    answered == 0 && (queried > 0 || st.backends().iter().all(|b| b.is_down()))
}

/// `Req` is not `Clone`; this is the field-by-field copy.
pub(super) fn copy_req(r: &Req) -> Req {
    Req {
        method: r.method.clone(),
        segs: r.segs.clone(),
        query: r.query.clone(),
        headers: r.headers.clone(),
        body: r.body.clone(),
        raw_path: r.raw_path.clone(),
    }
}

/// GET `path` with the client's query (ids translated per server) on every
/// target. Returns how many servers were asked and the ones that answered
/// 2xx, in server order.
pub(super) async fn fan_json(
    st: &Shared,
    r: &Req,
    c: &Client,
    endpoint: &'static str,
    path: &[&str],
) -> (usize, Vec<Raw>) {
    let (targets, _) = list_targets(st, &r.query).await;
    let user = c.user_id.as_str();
    let mut queried = 0usize;
    let mut futs = vec![];
    for (u, parent) in targets {
        let Some(up) = st.backend(u) else { continue };
        let Some(q) = query_for(st, &r.query, u, parent.as_ref().map(|p| p.as_str()), None) else {
            continue;
        };
        queried += 1;
        let path: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        futs.push(async move {
            let outcome = |o: &'static str| {
                counter!(names::FANOUT_RESULTS, "endpoint" => endpoint, "server" => up.name().to_string(), "outcome" => o).increment(1);
            };
            let uc = match up.login_as(Some(user)).await {
                Ok(uc) => uc,
                Err(e) => {
                    tracing::warn!(upstream = %up.name(), "skipping: {e}");
                    outcome("skipped");
                    return None;
                }
            };
            let mut q = q;
            if let Some(uid) = &uc.user_id {
                // Servers before 10.9 need it; newer ones accept their own id.
                q.retain(|(k, _)| !k.eq_ignore_ascii_case("userid"));
                q.push(("userId".into(), uid.clone()));
            }
            client_fields::apply(c, &path, &mut q);
            let segs = crate::paths::adapt(path, &mut q, uc.modern, uc.user_id.as_deref());
            match up.get_json_as(Some(user), &path_with_query(&segs, &q)).await {
                Ok((s, value)) if s.is_success() => {
                    outcome("success");
                    Some(Raw { up, uc, value })
                }
                Ok((s, _)) => {
                    tracing::warn!(upstream = %up.name(), endpoint, "HTTP {s}");
                    outcome("http_error");
                    None
                }
                Err(e) => {
                    tracing::warn!(upstream = %up.name(), endpoint, "{e}");
                    outcome(fan_outcome(&e));
                    None
                }
            }
        });
    }
    histogram!(names::FANOUT_UPSTREAMS, "endpoint" => endpoint).record(queried as f64);
    let raws: Vec<Raw> = join_all(futs).await.into_iter().flatten().collect();
    if !raws.is_empty() && raws.len() < queried {
        counter!(names::FANOUT_PARTIAL, "endpoint" => endpoint).increment(1);
    }
    (queried, raws)
}
