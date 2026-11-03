//! Proxy-owned playlists: expanding what a client adds (series, albums, ...)
//! into playable items, building playlist DTOs and listing entries.
//! The records live in [`crate::local::playlists`].

use super::*;
use crate::index::snapshot::Snapshot;
use crate::local::playlists::PlaylistRec;
use crate::types::ProxyItemId;

/// Kinds whose children come from the upstream that owns them.
const UPSTREAM_FOLDERS: &[&str] = &["MusicAlbum", "BoxSet", "Folder", "CollectionFolder"];

/// DTO keys only sent when the client's `Fields` ask for them.
const OPTIONAL: &[&str] = &["SortName", "DateCreated", "CanDelete", "Etag"];

/// Id of the synthetic "Playlists" library shown when no upstream has one.
pub fn playlists_view_id() -> String {
    synthetic_id(&["view", "playlists", "playlists"]).into_string()
}

/// The non-virtual episodes under a series or season, in watch order.
pub(crate) fn episodes_in_order(snap: &Snapshot, idx: usize) -> Vec<String> {
    let mut eps: Vec<usize> = snap
        .episodes_of(idx)
        .iter()
        .copied()
        .filter(|&i| !snap.is_virtual(i))
        .collect();
    eps.sort_by_key(|&i| {
        let e = snap.primary(&snap.items[i]);
        (e.parent_index.unwrap_or(0), e.index.unwrap_or(0))
    });
    eps.into_iter()
        .map(|i| snap.items[i].id.as_str().to_string())
        .collect()
}

/// Items under a folder-like item, asked of its server as the user.
async fn upstream_children(
    st: &Shared,
    c: &Client,
    server: ServerId,
    uid: &str,
    kind: &str,
) -> Vec<String> {
    let Some(up) = st.backend(server) else {
        return vec![];
    };
    let user = c.user_id.as_str();
    let Ok(uc) = up.login_as(Some(user)).await else {
        return vec![];
    };
    let pairs: &[(&str, &str)] = match kind {
        "MusicArtist" => &[
            ("IncludeItemTypes", "Audio"),
            ("Recursive", "true"),
            ("SortBy", "AlbumArtist,Album,SortName"),
        ],
        "MusicGenre" => &[
            ("IncludeItemTypes", "Audio"),
            ("Recursive", "true"),
            ("SortBy", "AlbumArtist,Album,SortName"),
        ],
        _ => &[
            ("Recursive", "true"),
            ("IsFolder", "false"),
            ("MediaTypes", "Audio,Video"),
        ],
    };
    let key = match kind {
        "MusicArtist" => "ArtistIds",
        "MusicGenre" => "GenreIds",
        _ => "ParentId",
    };
    let mut q: Vec<(String, String)> = vec![(key.to_string(), uid.to_string())];
    q.extend(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let segs = crate::paths::adapt(
        vec!["Items".into()],
        &mut q,
        uc.modern,
        uc.user_id.as_deref(),
    );
    match up
        .get_json_as(Some(user), &path_with_query(&segs, &q))
        .await
    {
        Ok((s, mut v)) if s.is_success() => {
            st.rewriters
                .rewrite_json(&mut v, &out_ctx(st, up.as_ref(), &uc, c));
            v.get("Items")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|i| i.get("Id").and_then(Value::as_str).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        }
        _ => vec![],
    }
}

/// An id that is neither an item nor a playlist the proxy knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotAnItem;

/// What adding `ids` to a playlist adds (`Playlist.GetPlaylistItems`):
/// series and seasons become their episodes, albums, collections, artists
/// and genres their tracks. `Err` when an id is not an item (or playlist)
/// the proxy knows. Without an index every id stands for itself.
pub async fn expand(st: &Shared, c: &Client, ids: &[String]) -> Result<Vec<String>, NotAnItem> {
    let snap = st.index.latest();
    let mut out: Vec<String> = vec![];
    for raw in ids {
        let pid = ProxyItemId::parse(raw).ok_or(NotAnItem)?;
        if let Some(rec) = st.local.playlists.get(pid.as_str()) {
            if !rec.visible(&c.user_id) {
                return Err(NotAnItem);
            }
            out.extend(rec.items);
            continue;
        }
        let Some(Target::Item { upstream, id }) = st.ids.decode(&pid) else {
            return Err(NotAnItem);
        };
        let found = snap
            .as_deref()
            .and_then(|s| s.item_index(&pid).map(|i| (s, i)));
        let Some((snap, idx)) = found else {
            out.push(pid.into_string());
            continue;
        };
        let kind = snap.primary(&snap.items[idx]).kind.as_str();
        match kind {
            "Series" | "Season" => out.extend(episodes_in_order(snap, idx)),
            k if UPSTREAM_FOLDERS.contains(&k) || k == "MusicArtist" || k == "MusicGenre" => {
                out.extend(upstream_children(st, c, upstream, id.as_str(), k).await)
            }
            _ => out.push(pid.into_string()),
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|i| seen.insert(i.clone()));
    Ok(out)
}

/// `PlaylistManager.CreatePlaylist`'s media type guess: the first item with
/// a known media type (a folder's first expanded child counts), else Audio.
pub async fn infer_media_type(st: &Shared, c: &Client, ids: &[String]) -> String {
    let Some(snap) = st.index.latest() else {
        return "Audio".into();
    };
    let media = |id: &str| {
        let e = snap.primary(snap.item(&ProxyItemId::parse(id)?)?);
        e.media_type.clone().filter(|m| m != "Unknown")
    };
    for id in ids {
        if let Some(m) = media(id) {
            return m;
        }
        let Some(item) = ProxyItemId::parse(id).and_then(|p| snap.item(&p).cloned()) else {
            continue;
        };
        match snap.primary(&item).kind.as_str() {
            "Genre" => return "Video".into(),
            "MusicArtist" | "MusicGenre" | "MusicAlbum" => return "Audio".into(),
            _ => {}
        }
        let kids = expand(st, c, std::slice::from_ref(id))
            .await
            .unwrap_or_default();
        if let Some(m) = kids.iter().find_map(|k| media(k)) {
            return m;
        }
    }
    "Audio".into()
}

/// The id of the user's "playlists" library: an upstream's, else the
/// synthetic one.
pub async fn playlists_view_for(st: &Shared, c: &Client) -> String {
    let views = views(st, c, false).await;
    let hidden = [("includeHidden".to_string(), "true".to_string())];
    present_views(st, c, &hidden, views)
        .iter()
        .find(|v| {
            v.get("CollectionType")
                .and_then(Value::as_str)
                .is_some_and(|t| t.eq_ignore_ascii_case("playlists"))
        })
        .and_then(|v| v.get("Id").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(playlists_view_id)
}

/// The synthetic "Playlists" library DTO (`UserViewManager.GetUserViews`).
pub fn playlists_view_dto(st: &Shared) -> Value {
    let id = playlists_view_id();
    json!({"Name": "Playlists", "ServerId": st.server_id, "Id": id, "Etag": id,
           "DisplayPreferencesId": id, "ParentId": st.root_id, "Type": "UserView",
           "CollectionType": "playlists", "IsFolder": true, "SortName": "playlists",
           "ImageTags": {}, "BackdropImageTags": [], "LocationType": "FileSystem",
           "PlayAccess": "Full"})
}

/// A playlist as an item. `parent` is the user's playlists library.
pub fn playlist_dto(st: &Shared, user: &str, rec: &PlaylistRec, parent: &str) -> Value {
    let id = ProxyItemId::parse(&rec.id);
    let user_data = id
        .as_ref()
        .map(|id| crate::userdata::view::user_dto(&st.userdata, user, id, None, None))
        .unwrap_or(Value::Null);
    json!({"Name": rec.name, "ServerId": st.server_id, "Id": rec.id, "Etag": rec.id,
           "DateCreated": rec.created, "CanDelete": rec.owner == user,
           "SortName": rec.name.to_lowercase(), "IsFolder": true, "Type": "Playlist",
           "MediaType": rec.media_type, "ChildCount": rec.items.len(),
           "UserData": user_data, "ImageTags": {}, "BackdropImageTags": [],
           "LocationType": "FileSystem", "PlayAccess": "Full", "ParentId": parent,
           "PrimaryImageAspectRatio": 1})
}

/// Drop the optional keys the client did not ask for in `Fields`.
pub fn strip_playlist_fields(dto: &mut Value, fields: &str) {
    let want = |k: &str| fields.split(',').any(|f| f.trim().eq_ignore_ascii_case(k));
    if let Value::Object(o) = dto {
        for k in OPTIONAL.iter().chain(&["PrimaryImageAspectRatio"]) {
            if !want(k) {
                o.remove(*k);
            }
        }
    }
}

fn page_of(q: &[(String, String)]) -> (usize, Option<usize>) {
    let n = |k| q_get(q, k).and_then(|s| s.parse::<usize>().ok());
    (n("StartIndex").unwrap_or(0), n("Limit"))
}

/// `GET Playlists/{id}/Items`: the entries `c` may see, as DTOs.
pub async fn playlist_items(st: &Shared, r: &Req, c: &Client, rec: &PlaylistRec) -> Response {
    let visible: Vec<&String> = rec
        .items
        .iter()
        .filter(|i| ProxyItemId::parse(i).is_some_and(|p| !st.parental.blocks(&c.user_id, &p)))
        .collect();
    let total = visible.len();
    let (start, limit) = page_of(&r.query);
    let page: Vec<&String> = visible
        .into_iter()
        .skip(start)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    let picks: Vec<Option<(ServerId, String)>> = page
        .iter()
        .map(
            |i| match ProxyItemId::parse(i).and_then(|p| st.ids.decode(&p)) {
                Some(Target::Item { upstream, id }) => Some((upstream, id.into_string())),
                _ => None,
            },
        )
        .collect();
    let mut dtos = fetch_dtos(st, c, &picks, &r.query).await;
    let items: Vec<Value> = page
        .into_iter()
        .zip(picks)
        .filter_map(|(entry, pick)| {
            let mut v = dtos.remove(&pick?)?;
            v["Id"] = json!(entry);
            v["PlaylistItemId"] = json!(entry);
            Some(v)
        })
        .collect();
    ok_json(dto::query_result(items, total, start))
}

/// `GET Items` for the playlists library or `IncludeItemTypes=Playlist`:
/// the upstreams' playlists (the usual query) plus the user's own.
/// `None` when the request is about something else.
pub async fn items_hook(st: &Shared, r: &Req, c: &Client) -> Option<Result<Response>> {
    let parent = q_get(&r.query, "ParentId").and_then(crate::types::normalize_hex_id);
    if let Some(rec) = parent.as_deref().and_then(|p| st.local.playlists.get(p)) {
        if !rec.visible(&c.user_id) {
            return Some(Ok(err_resp(StatusCode::NOT_FOUND, "Playlist not found")));
        }
        return Some(Ok(playlist_items(st, r, c, &rec).await));
    }
    let only_playlists =
        q_get(&r.query, "IncludeItemTypes").is_some_and(|t| t.eq_ignore_ascii_case("Playlist"));
    let own_view = playlists_view_id();
    let in_view = match parent.as_deref() {
        Some(p) => p == own_view || p == playlists_view_for(st, c).await,
        None => false,
    };
    if !in_view && !only_playlists {
        return None;
    }
    let (start, limit) = page_of(&r.query);
    let (mut items, upstream_total) = if parent.as_deref() == Some(own_view.as_str()) {
        (vec![], 0)
    } else {
        let mut q: Vec<(String, String)> = r
            .query
            .iter()
            .filter(|(k, _)| {
                !k.eq_ignore_ascii_case("StartIndex") && !k.eq_ignore_ascii_case("Limit")
            })
            .cloned()
            .collect();
        q.push(("StartIndex".into(), "0".into()));
        if let Some(l) = limit {
            q.push(("Limit".into(), (start + l).to_string()));
        }
        let mut inner = copy_req(r);
        inner.query = q.clone();
        let resp = match indexed_items(st, &inner, c, "items", &q).await {
            Some(res) => res,
            None => fan_list(st, &inner, c, &ITEMS).await,
        };
        let resp = match resp {
            Ok(r) => r,
            Err(e) => return Some(Err(e)),
        };
        if !resp.status().is_success() {
            return Some(Ok(resp));
        }
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .ok()?;
        let mut v: Value = serde_json::from_slice(&bytes).ok()?;
        let total = v["TotalRecordCount"].as_u64().unwrap_or(0) as usize;
        let items = match v["Items"].take() {
            Value::Array(a) => a,
            _ => vec![],
        };
        (items, total)
    };
    let view = playlists_view_for(st, c).await;
    let term = q_get(&r.query, "SearchTerm").map(str::to_lowercase);
    let fields = q_get(&r.query, "Fields").unwrap_or("");
    let own: Vec<Value> = st
        .local
        .playlists
        .for_user(&c.user_id)
        .iter()
        .filter(|p| {
            term.as_ref()
                .is_none_or(|t| p.name.to_lowercase().contains(t))
        })
        .map(|p| playlist_dto(st, &c.user_id, p, &view))
        .collect();
    let total = upstream_total + own.len();
    items.extend(own);
    let mut sort = merge::parse_sort(q_get(&r.query, "SortBy"), q_get(&r.query, "SortOrder"));
    if sort.is_empty() {
        sort = vec![("SortName".to_string(), false)];
    }
    merge::sort_items(&mut items, &sort);
    let mut items = merge::page(items, start, limit);
    for i in items.iter_mut().filter(|i| {
        i["Type"] == "Playlist"
            && st
                .local
                .playlists
                .is_playlist(i["Id"].as_str().unwrap_or(""))
    }) {
        strip_playlist_fields(i, fields);
    }
    Some(Ok(ok_json(dto::query_result(items, total, start))))
}
