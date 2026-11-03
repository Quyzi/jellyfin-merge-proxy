//! `GET`/`POST DisplayPreferences/{id}`
//! (`DisplayPreferencesController.cs:66-211`).

use super::*;
use crate::proxy::{err_resp, no_content, ok_json, q_get, Client, Req};
use crate::Shared;
use axum::http::{Method, StatusCode};
use axum::response::Response;
use serde_json::Map;

const HOME_DEFAULTS: [&str; 8] = [
    "SmallLibraryTiles",
    "Resume",
    "ResumeAudio",
    "ResumeBook",
    "LiveTv",
    "NextUp",
    "LatestMedia",
    "None",
];
const HOME_TYPES: [&str; 10] = [
    "None",
    "SmallLibraryTiles",
    "LibraryButtons",
    "ActiveRecordings",
    "Resume",
    "ResumeAudio",
    "LatestMedia",
    "NextUp",
    "LiveTv",
    "ResumeBook",
];
const INDEXING: [&str; 3] = ["PremiereDate", "ProductionYear", "CommunityRating"];
const VIEW_TYPES: [&str; 21] = [
    "Albums",
    "AlbumArtists",
    "Artists",
    "Channels",
    "Collections",
    "Episodes",
    "Favorites",
    "Genres",
    "Guide",
    "Movies",
    "Networks",
    "Playlists",
    "Programs",
    "Recordings",
    "Schedule",
    "Series",
    "Shows",
    "Songs",
    "Suggestions",
    "Trailers",
    "Upcoming",
];

/// The canonical spelling of `s` among `names`, ignoring case.
fn parse_name(names: &[&'static str], s: &str) -> Option<&'static str> {
    names.iter().copied().find(|n| n.eq_ignore_ascii_case(s))
}

fn status(code: StatusCode) -> Response {
    err_resp(code, "Error processing request.")
}

pub async fn handle(st: &Shared, r: Req, c: &Client) -> Response {
    let Some(client) = q_get(&r.query, "client") else {
        return status(StatusCode::BAD_REQUEST);
    };
    if let Some(u) = q_get(&r.query, "userId") {
        if normalize_hex_id(u).as_deref() != Some(c.user_id.as_str()) {
            return status(StatusCode::FORBIDDEN);
        }
    }
    let prefs = prefs_guid(&r.segs[1]);
    if r.method == Method::POST {
        post(st, &r, c, &prefs, client)
    } else {
        get(st, c, &prefs, client)
    }
}

fn get(st: &Shared, c: &Client, prefs: &str, client: &str) -> Response {
    let rec = st
        .local
        .prefs
        .get(&c.user_id, prefs, client)
        .unwrap_or_else(default_record);
    let f = |k: &str| rec.get(k).cloned().unwrap_or(Value::Null);
    let text = |k: &str| f(k).as_str().map(str::to_string);
    let mut custom = Map::new();
    for h in rec["HomeSections"].as_array().into_iter().flatten() {
        custom.insert(
            format!("homesection{}", h["Order"]),
            json!(h["Type"].as_str().unwrap_or("None").to_lowercase()),
        );
    }
    custom.insert(
        "chromecastVersion".into(),
        json!(text("ChromecastVersion").unwrap_or_default().to_lowercase()),
    );
    custom.insert(
        "skipForwardLength".into(),
        json!(f("SkipForwardLength").to_string()),
    );
    custom.insert(
        "skipBackLength".into(),
        json!(f("SkipBackwardLength").to_string()),
    );
    let overlay = f("EnableNextVideoInfoOverlay").as_bool().unwrap_or(false);
    custom.insert(
        "enableNextVideoInfoOverlay".into(),
        json!(if overlay { "True" } else { "False" }),
    );
    custom.insert("tvhome".into(), f("TvHome"));
    custom.insert("dashboardTheme".into(), f("DashboardTheme"));
    for (k, v) in rec["Custom"].as_object().into_iter().flatten() {
        custom.entry(k.clone()).or_insert_with(|| v.clone());
    }
    let mut out = Map::new();
    out.insert("Id".into(), json!(dashed(prefs)));
    out.insert("SortBy".into(), f("SortBy"));
    if let Some(i) = rec.get("IndexBy").filter(|v| !v.is_null()) {
        out.insert("IndexBy".into(), i.clone());
    }
    out.insert("RememberIndexing".into(), f("RememberIndexing"));
    out.insert("PrimaryImageHeight".into(), json!(250));
    out.insert("PrimaryImageWidth".into(), json!(250));
    out.insert("CustomPrefs".into(), Value::Object(custom));
    for k in [
        "ScrollDirection",
        "ShowBackdrop",
        "RememberSorting",
        "SortOrder",
        "ShowSidebar",
    ] {
        out.insert(k.into(), f(k));
    }
    out.insert("Client".into(), json!(client));
    ok_json(Value::Object(out))
}

fn body_key<'a>(o: &'a Map<String, Value>, k: &str) -> Option<&'a Value> {
    o.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(k))
        .map(|(_, v)| v)
}

fn flag(o: &Map<String, Value>, k: &str, default: bool) -> Result<bool, StatusCode> {
    match body_key(o, k) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(StatusCode::BAD_REQUEST),
    }
}

fn named(
    o: &Map<String, Value>,
    k: &str,
    names: &[&'static str],
    default: &'static str,
) -> Result<&'static str, StatusCode> {
    match body_key(o, k) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(s)) => parse_name(names, s).ok_or(StatusCode::BAD_REQUEST),
        Some(_) => Err(StatusCode::BAD_REQUEST),
    }
}

/// Take `key` out of the custom map; `None` when absent or null.
fn take(custom: &mut Map<String, Value>, key: &str) -> Option<String> {
    let k = custom
        .keys()
        .find(|k| k.eq_ignore_ascii_case(key))
        .cloned()?;
    custom
        .shift_remove(&k)
        .and_then(|v| v.as_str().map(str::to_string))
}

fn int_pref(custom: &mut Map<String, Value>, key: &str, default: i64) -> Result<i64, StatusCode> {
    match take(custom, key).filter(|s| !s.is_empty()) {
        None => Ok(default),
        Some(s) => s
            .trim()
            .parse::<i32>()
            .map(i64::from)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR),
    }
}

fn post(st: &Shared, r: &Req, c: &Client, prefs: &str, client: &str) -> Response {
    match build(r) {
        Ok(rec) => {
            st.local.prefs.put(&c.user_id, prefs, client, rec);
            no_content()
        }
        Err(e) => status(e),
    }
}

fn build(r: &Req) -> Result<Value, StatusCode> {
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(&r.body) else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let mut custom = match body_key(&body, "CustomPrefs") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) if m.values().all(|v| v.is_string() || v.is_null()) => m.clone(),
        Some(_) => return Err(StatusCode::BAD_REQUEST),
    };
    let index_by = body_key(&body, "IndexBy")
        .and_then(Value::as_str)
        .and_then(|s| parse_name(&INDEXING, s));
    let scroll = named(
        &body,
        "ScrollDirection",
        &["Horizontal", "Vertical"],
        "Horizontal",
    )?;
    let order = named(
        &body,
        "SortOrder",
        &["Ascending", "Descending"],
        "Ascending",
    )?;
    let chromecast = match take(&mut custom, "chromecastVersion").filter(|s| !s.is_empty()) {
        None => "Stable",
        Some(s) => parse_name(&["Stable", "Unstable"], &s).ok_or(StatusCode::BAD_REQUEST)?,
    };
    let overlay = match take(&mut custom, "enableNextVideoInfoOverlay").filter(|s| !s.is_empty()) {
        None => true,
        Some(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => true,
            "false" => false,
            _ => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        },
    };
    let back = int_pref(&mut custom, "skipBackLength", 10000)?;
    let forward = int_pref(&mut custom, "skipForwardLength", 30000)?;
    let theme = take(&mut custom, "dashboardTheme").unwrap_or_default();
    let tvhome = take(&mut custom, "tvhome").unwrap_or_default();
    let mut sections = Vec::new();
    let home_keys: Vec<String> = custom
        .keys()
        .filter(|k| k.to_ascii_lowercase().starts_with("homesection"))
        .cloned()
        .collect();
    for k in home_keys {
        let n: i64 = k["homesection".len()..]
            .trim()
            .parse::<i32>()
            .map(i64::from)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let given = custom
            .shift_remove(&k)
            .and_then(|v| v.as_str().map(str::to_string));
        let ty = match given.as_deref().and_then(|s| parse_name(&HOME_TYPES, s)) {
            Some(t) => t,
            None if n < 0 => return Err(StatusCode::INTERNAL_SERVER_ERROR),
            None if n < 8 => HOME_DEFAULTS[n as usize],
            None => "None",
        };
        sections.push(json!({"Order": n, "Type": ty}));
    }
    custom.retain(|k, v| {
        !k.to_ascii_lowercase().starts_with("landing-")
            || v.as_str()
                .is_some_and(|s| parse_name(&VIEW_TYPES, s).is_some())
    });
    let sort_by = body_key(&body, "SortBy")
        .and_then(Value::as_str)
        .unwrap_or("SortName");
    let mut rec = json!({
        "ShowBackdrop": flag(&body, "ShowBackdrop", true)?,
        "ShowSidebar": flag(&body, "ShowSidebar", false)?,
        "ScrollDirection": scroll,
        "ChromecastVersion": chromecast,
        "EnableNextVideoInfoOverlay": overlay,
        "SkipBackwardLength": back,
        "SkipForwardLength": forward,
        "DashboardTheme": theme,
        "TvHome": tvhome,
        "HomeSections": sections,
        "SortBy": sort_by,
        "SortOrder": order,
        "RememberIndexing": flag(&body, "RememberIndexing", false)?,
        "RememberSorting": flag(&body, "RememberSorting", false)?,
        "Custom": Value::Object(custom)
    });
    if let Some(i) = index_by {
        rec["IndexBy"] = json!(i);
    }
    Ok(rec)
}
