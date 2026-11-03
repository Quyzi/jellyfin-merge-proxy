//! Who else can see or edit a playlist (`Playlists/{id}/Users`).

use super::*;

pub(super) async fn users(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let rec = match find(st, &r.segs[1], c) {
        Ok(Some(rec)) => rec,
        Ok(None) => return Ok(Flow::Next(r)),
        Err(e) => return reply(e),
    };
    if rec.owner != c.user_id {
        return done(forbidden());
    }
    let v: Vec<Value> = rec.shares.iter().map(share_json).collect();
    done(ok_json(Value::Array(v)))
}

pub(super) async fn share(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let rec = match find(st, &r.segs[1], c) {
        Ok(Some(rec)) => rec,
        Ok(None) => return Ok(Flow::Next(r)),
        Err(e) => return reply(e),
    };
    let target = normalize_hex_id(&r.segs[3]).unwrap_or_else(|| r.segs[3].clone());
    let no_perm = || err_resp(StatusCode::NOT_FOUND, "User permissions not found");
    let existing = rec.shares.iter().find(|s| s.user_id == target).cloned();
    match r.method {
        Method::POST => {
            if rec.owner != c.user_id {
                return done(forbidden());
            }
            let can_edit = body(&r)
                .and_then(|b| field(&b, "CanEdit").and_then(Value::as_bool))
                .unwrap_or(false);
            st.local.playlists.update(&rec.id, |p| {
                p.shares.retain(|s| s.user_id != target);
                p.shares.push(Share {
                    user_id: target,
                    can_edit,
                });
            });
            done(no_content())
        }
        Method::DELETE => {
            if !rec.can_edit(&c.user_id) {
                return done(forbidden());
            }
            if existing.is_none() {
                return done(no_perm());
            }
            st.local
                .playlists
                .update(&rec.id, |p| p.shares.retain(|s| s.user_id != target));
            done(no_content())
        }
        _ => {
            if rec.owner == c.user_id {
                return done(ok_json(json!({"UserId": c.user_id, "CanEdit": true})));
            }
            let permitted = rec
                .shares
                .iter()
                .any(|s| s.can_edit && s.user_id == c.user_id)
                || target == c.user_id;
            if !permitted {
                return done(forbidden());
            }
            done(match existing {
                Some(s) => ok_json(share_json(&s)),
                None => no_perm(),
            })
        }
    }
}
