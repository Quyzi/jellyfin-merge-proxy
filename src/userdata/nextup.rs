//! NextUp from the merged catalog and per-user state.
//!
//! Jellyfin's rules (`TVSeriesManager`): for every series the user has
//! played an episode of, the next episode is the first unplayed regular
//! episode (season > 0) after the furthest played one, in season/episode
//! order. Series are listed most recently played first. With a `SeriesId`
//! and nothing played yet, the first episode is returned (unless
//! `DisableFirstEpisode`). Because a series split across servers is one
//! canonical series with merged episodes, it gets exactly one entry.

use super::ItemState;
use crate::index::query::Scope;
use crate::index::snapshot::Snapshot;
use crate::types::ServerId;

#[derive(Debug, Clone, Default)]
pub struct NextUpQuery {
    pub series: Option<usize>,
    /// Library scope (`ParentId` of a view).
    pub scope: Option<Scope>,
    /// `NextUpDateCutoff`: skip series last played before this (ISO).
    pub cutoff: Option<String>,
    pub disable_first_episode: bool,
    /// `EnableResumable=false`: skip next episodes that are in progress.
    pub skip_resumable: bool,
}

fn order(snap: &Snapshot, i: usize) -> (i64, i64) {
    let e = snap.primary(&snap.items[i]);
    (e.parent_index.unwrap_or(0), e.index.unwrap_or(0))
}

/// Next episode (item index) per series, most recently played first.
pub fn next_up(
    snap: &Snapshot,
    live: &dyn Fn(ServerId) -> bool,
    state: &dyn Fn(usize) -> ItemState,
    q: &NextUpQuery,
) -> Vec<usize> {
    let is_live = |i: usize| {
        snap.items[i]
            .copies
            .iter()
            .any(|c| live(snap.copies[*c].server))
    };
    let candidates: Vec<usize> = match q.series {
        Some(s) => vec![s],
        None => (0..snap.items.len())
            .filter(|i| snap.primary(&snap.items[*i]).is("Series"))
            .filter(|i| match &q.scope {
                Some(Scope::Libraries(libs)) => {
                    snap.items[*i].libraries.iter().any(|l| libs.contains(l))
                }
                Some(_) => false,
                None => true,
            })
            .collect(),
    };
    let mut out: Vec<(Option<String>, usize)> = vec![];
    for s in candidates {
        let mut eps: Vec<usize> = snap
            .episodes_of(s)
            .iter()
            .copied()
            .filter(|e| snap.items[*e].series == Some(s) && is_live(*e) && !snap.is_virtual(*e))
            .collect();
        eps.sort_by_key(|e| order(snap, *e));
        let states: Vec<ItemState> = eps.iter().map(|e| state(*e)).collect();
        let last_date = states.iter().filter_map(|s| s.last_played.clone()).max();
        let regular = |k: usize| order(snap, eps[k]).0 > 0;
        let last_played = (0..eps.len())
            .filter(|k| regular(*k) && states[*k].played)
            .max();
        let start = match last_played {
            Some(k) => k + 1,
            None if q.series.is_some() && !q.disable_first_episode => 0,
            None => continue,
        };
        if last_played.is_some() {
            if let (Some(c), Some(d)) = (&q.cutoff, &last_date) {
                if d < c {
                    continue;
                }
            }
        }
        let next = (start..eps.len()).find(|k| regular(*k) && !states[*k].played);
        let Some(k) = next else { continue };
        if q.skip_resumable && states[k].position_ticks > 0 {
            continue;
        }
        out.push((last_date, eps[k]));
    }
    // Most recently played first; never-played (first episode) last.
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, e)| e).collect()
}
