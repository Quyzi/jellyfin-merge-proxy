//! `Genres/{name}`, `Studios/{name}`, `Persons/{name}`, `MusicGenres/{name}`
//! and `Artists/{name}` (and their images): which server has this name.
//!
//! Names are not ids, so nothing in the path says who owns them. The index
//! knows genres, studios and people; for the rest (and a cold index) the
//! live servers are probed in config order. The name route itself is never
//! probed for genres/studios/artists: Jellyfin creates the item on a miss.

use super::*;
use percent_encoding::percent_decode_str;

/// The decoded name in a name-detail path (`persons/<name>/...`). Path
/// segments keep a literal `+`.
pub fn detail_name(seg: &str) -> String {
    percent_decode_str(seg).decode_utf8_lossy().into_owned()
}

/// The `Items` filter that finds items carrying a name of this kind.
fn filter_key(head: &str) -> Option<&'static str> {
    match head {
        "genres" | "musicgenres" => Some("Genres"),
        "studios" => Some("Studios"),
        "artists" => Some("Artists"),
        _ => None,
    }
}

/// The server to answer `r` (a name-detail request) from, if any has the name.
pub async fn name_server(st: &Shared, r: &Req, c: &Client) -> Option<ServerId> {
    let head = r.segs.first()?.to_ascii_lowercase();
    let raw = r.segs.get(1)?;
    let name = detail_name(raw);
    if let (Some(kind), Some(snap)) = (name_kind(&head), warm_snapshot(st, "name_detail")) {
        if let Some(ni) = snap.name_by_name(kind, &name) {
            let n = &snap.names[ni];
            let server = std::iter::once(&n.id)
                .chain(n.members.iter())
                .find_map(|m| match st.ids.decode(m) {
                    Some(Target::Item { upstream, .. }) if is_live(st, upstream) => Some(upstream),
                    _ => None,
                });
            if server.is_some() {
                return server;
            }
        }
    }
    probe(st, c, &head, raw, &name).await
}

async fn probe(st: &Shared, c: &Client, head: &str, raw: &str, name: &str) -> Option<ServerId> {
    let user = c.user_id.as_str();
    let set = st.backends();
    for up in set.iter().filter(|b| !b.is_down()) {
        let Ok(uc) = up.login_as(Some(user)).await else {
            continue;
        };
        let mut q: Vec<(String, String)> = vec![];
        let path = match filter_key(head) {
            Some(key) => {
                q.push((key.into(), name.to_string()));
                q.push(("Recursive".into(), "true".into()));
                q.push(("Limit".into(), "0".into()));
                q.push(("EnableTotalRecordCount".into(), "true".into()));
                vec!["Items".to_string()]
            }
            None => vec!["Persons".to_string(), raw.to_string()],
        };
        let segs = crate::paths::adapt(path, &mut q, uc.modern, uc.user_id.as_deref());
        let found = match up
            .get_json_as(Some(user), &path_with_query(&segs, &q))
            .await
        {
            Ok((s, v)) if s.is_success() => {
                filter_key(head).is_none() || v["TotalRecordCount"].as_u64().unwrap_or(0) > 0
            }
            _ => false,
        };
        if found {
            return Some(up.id());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name with spaces and a plus (`Jane+Doe` is literal in a path) must
    /// decode to the name the index and upstream filters know.
    #[test]
    fn detail_names_decode_percent_escapes_but_not_plus() {
        assert_eq!(detail_name("Jane%20Doe"), "Jane Doe");
        assert_eq!(detail_name("A+B"), "A+B");
        assert_eq!(detail_name("Caf%C3%A9"), "Café");
    }
}
