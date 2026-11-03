//! Client-name dependent DTO fields (`DtoExtensions.AddClientFields`): some
//! clients are always sent child counts, whatever `Fields` they ask for.
//! Jellyfin keys this on the authorization header's client name, so the
//! proxy asks the upstream for the same fields on behalf of that client.

use super::*;

/// The `Fields` Jellyfin adds for `client` (matched case-insensitively).
pub fn fields(client: &str) -> &'static [&'static str] {
    let c = client.to_ascii_lowercase();
    let has = |names: &[&str]| names.iter().any(|n| c.contains(n));
    if has(&["kodi", "wmc", "media center", "classic"]) {
        &["RecursiveItemCount", "ChildCount"]
    } else if has(&["roku", "samsung", "androidtv"]) {
        &["ChildCount"]
    } else {
        &[]
    }
}

const DTO_ROOTS: &[&str] = &[
    "items",
    "useritems",
    "users",
    "userviews",
    "shows",
    "genres",
    "musicgenres",
    "studios",
    "persons",
    "artists",
    "years",
    "movies",
    "livetv",
    "playlists",
    "library",
    "trailers",
    "albums",
    "songs",
];

const NOT_DTO: &[&str] = &[
    "images",
    "playbackinfo",
    "stream",
    "download",
    "file",
    "subtitles",
    "attachments",
    "hls",
    "hls1",
    "lyrics",
    "mediasegments",
    "trickplay",
    "remoteimages",
    "remotesearch",
    "filters",
    "filters2",
    "counts",
];

/// Whether a GET on `segs` returns item DTOs (so `Fields` matters).
pub fn dto_route(segs: &[String]) -> bool {
    let lower: Vec<String> = segs.iter().map(|s| s.to_ascii_lowercase()).collect();
    let Some(first) = lower.first() else {
        return false;
    };
    let roots = DTO_ROOTS.contains(&first.as_str())
        || (first == "videos" && lower.last().is_some_and(|l| l == "additionalparts"));
    roots
        && !lower
            .iter()
            .any(|s| NOT_DTO.contains(&s.as_str()) || s.ends_with(".m3u8"))
}

/// Add the client's extra `Fields` to a DTO query. A client without a
/// session (anonymous) gets nothing.
pub fn apply(c: &Client, segs: &[String], q: &mut Vec<(String, String)>) {
    let Some(s) = &c.session else { return };
    if !dto_route(segs) {
        return;
    }
    for f in fields(&s.client) {
        ensure_field(q, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(p: &str) -> Vec<String> {
        p.split('/').map(str::to_string).collect()
    }

    /// Kodi-like clients get both counts; `Android TV` (with a space) does
    /// not match Jellyfin's `androidtv` token and gets nothing.
    #[test]
    fn fields_follow_the_client_name() {
        assert_eq!(fields("Kodi"), ["RecursiveItemCount", "ChildCount"]);
        assert_eq!(fields("AndroidTV"), ["ChildCount"]);
        assert!(fields("Android TV").is_empty());
    }

    /// Image, stream and filter routes never return item DTOs.
    #[test]
    fn dto_routes_exclude_binaries_and_filters() {
        assert!(dto_route(&segs("Items")));
        assert!(dto_route(&segs("Shows/abc/Episodes")));
        assert!(dto_route(&segs("Videos/abc/AdditionalParts")));
        assert!(!dto_route(&segs("Items/abc/Images/Primary")));
        assert!(!dto_route(&segs("Items/Filters")));
        assert!(!dto_route(&segs("Videos/abc/stream")));
        assert!(!dto_route(&segs("Audio/abc/main.m3u8")));
    }
}
