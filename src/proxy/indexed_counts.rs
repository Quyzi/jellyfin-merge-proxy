//! `Items/Counts` with merged copies counted once: the per-type counts come
//! from the catalog index (what the user can see, duplicates collapsed);
//! `ItemCount` (which also counts folders the index does not keep) is the
//! servers' sum lowered by the copies the index merged.

use super::indexed::{index_outcome, run_query, warm_snapshot};
use super::*;

/// Jellyfin's `ItemCounts` keys and the item type each one counts.
const KEYS: &[(&str, &str)] = &[
    ("MovieCount", "Movie"),
    ("SeriesCount", "Series"),
    ("EpisodeCount", "Episode"),
    ("ArtistCount", "MusicArtist"),
    ("ProgramCount", "Program"),
    ("TrailerCount", "Trailer"),
    ("SongCount", "Audio"),
    ("AlbumCount", "MusicAlbum"),
    ("MusicVideoCount", "MusicVideo"),
    ("BoxSetCount", "BoxSet"),
    ("BookCount", "Book"),
];

/// Replace the summed per-type counts in `summed` (the servers' merged
/// answer) with the index's; `false` when the index cannot answer.
pub fn indexed_counts(st: &Shared, r: &Req, c: &Client, summed: &mut Value) -> bool {
    let endpoint = "counts";
    let Some(snap) = warm_snapshot(st, endpoint) else {
        return false;
    };
    let mut q: Vec<(String, String)> = r
        .query
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("UserId"))
        .cloned()
        .collect();
    q.push(("Recursive".into(), "true".into()));
    let Some((_, all)) = run_query(st, &snap, &q, endpoint, &c.user_id) else {
        return false;
    };
    index_outcome(endpoint, "index");
    let mut counts = [0i64; KEYS.len()];
    for i in all {
        let e = snap.primary(&snap.items[i]);
        if let Some(k) = KEYS.iter().position(|(_, t)| e.is(t)) {
            counts[k] += 1;
        }
    }
    let mut removed = 0i64;
    for ((key, _), n) in KEYS.iter().zip(counts) {
        let was = summed.get(*key).and_then(Value::as_i64).unwrap_or(0);
        removed += was - n;
        summed[*key] = json!(n);
    }
    if let Some(total) = summed.get("ItemCount").and_then(Value::as_i64) {
        summed["ItemCount"] = json!((total - removed).max(0));
    }
    true
}
