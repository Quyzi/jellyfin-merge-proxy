//! Per-proxy-user parental limits (`[[user]] max_parental_rating`,
//! `block_unrated`), Jellyfin's MaxParentalRating / BlockUnratedItems.
//!
//! * [`ContentPolicy`] is the seam: does a rating (score, item type) pass?
//!   [`RatingPolicy`] is Jellyfin's rule: rated items pass when their
//!   score is at most the limit; unrated ones pass unless `block_unrated`
//!   (which only applies to video items, never to folders or libraries).
//! * Episodes and seasons without their own rating inherit the series'.
//! * [`service::ParentalService`] resolves each user's policy (rating
//!   names through the index's rating tables) and is hot-reloadable;
//!   [`filter::ParentalFilter`] drops blocked items from fan-out
//!   responses. Index queries and NextUp filter with the same policy.

pub mod filter;
pub mod service;

use crate::index::snapshot::Snapshot;

/// Whether content may be shown to a user.
pub trait ContentPolicy: Send + Sync {
    /// `score`: the rating score (None = unrated); `kind`: item type.
    fn allows(&self, score: Option<i64>, kind: &str) -> bool;
}

/// Item types `block_unrated` applies to (Jellyfin's UnratedItem kinds
/// that are video; folders, libraries and people are never blocked).
const UNRATED_KINDS: &[&str] = &[
    "Movie",
    "Series",
    "Season",
    "Episode",
    "Trailer",
    "MusicVideo",
    "Video",
    "BoxSet",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatingPolicy {
    pub max: i64,
    pub block_unrated: bool,
}

impl ContentPolicy for RatingPolicy {
    fn allows(&self, score: Option<i64>, kind: &str) -> bool {
        match score {
            Some(s) => s <= self.max,
            None => !self.block_unrated || !UNRATED_KINDS.contains(&kind),
        }
    }
}

/// The score an index item is judged by: its own, else its series'.
pub fn effective_score(snap: &Snapshot, i: usize) -> Option<i64> {
    let it = &snap.items[i];
    it.rating_score
        .or_else(|| it.series.and_then(|s| snap.items[s].rating_score))
}

/// Whether index item `i` passes `p`.
pub fn allows_item(p: &dyn ContentPolicy, snap: &Snapshot, i: usize) -> bool {
    let kind = snap.primary(&snap.items[i]).kind.as_str();
    p.allows(effective_score(snap, i), kind)
}

#[cfg(test)]
mod tests;
