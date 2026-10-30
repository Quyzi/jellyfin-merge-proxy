//! Per-proxy-user watch state ("user data"): played, play count, resume
//! position, favourite, likes, rating and last played date.
//!
//! The proxy keeps this state itself, per proxy user, keyed by the item's
//! canonical id ([`crate::index`]), so two proxy users sharing one upstream
//! account still have independent state, and a show split across servers
//! has one state per episode whichever copy was played. Changes are written
//! through to the upstream copies when `[userdata] write_through` is on.
//!
//! * [`WatchState`] is the seam: [`UserDataTable`] (in memory, persisted
//!   through a [`crate::store::UserDataStore`]) and the metric decorator
//!   [`MeteredWatchState`].
//! * [`rules`]: Jellyfin's play-state transitions (start, progress,
//!   stopped with resume thresholds, mark played/unplayed, DTO updates).
//! * [`view`]: per-user `UserData` DTOs (series/season counts from the
//!   index) and the [`view::UserDataOverlay`] response rewriter.
//! * [`nextup`]: NextUp from merged state (one entry per canonical series).
//! * [`seed`]: one-time import of upstream state per (user, server).

pub mod nextup;
pub mod rules;
pub mod seed;
pub mod service;
mod table;
pub mod view;

pub use service::UserDataService;
pub use table::{MeteredWatchState, UserDataTable};

use crate::types::ProxyItemId;
use serde::{Deserialize, Serialize};

/// One user's state of one item (Jellyfin's `UserItemData`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ItemState {
    pub played: bool,
    pub play_count: i64,
    pub position_ticks: i64,
    pub favorite: bool,
    pub likes: Option<bool>,
    pub rating: Option<f64>,
    /// ISO-8601 UTC, as Jellyfin formats it.
    pub last_played: Option<String>,
}

/// Per-user item state.
pub trait WatchState: Send + Sync {
    fn get(&self, user: &str, item: &ProxyItemId) -> Option<ItemState>;
    /// Apply `f` to the user's state of each item (default state when it
    /// has none), persist, and return the new states. `op` names the
    /// change for metrics.
    fn update(
        &self,
        user: &str,
        items: &[ProxyItemId],
        op: &'static str,
        f: &dyn Fn(&mut ItemState),
    ) -> Vec<ItemState>;
    /// Insert states only for items the user has no state for. Returns
    /// how many were inserted.
    fn seed(&self, user: &str, rows: Vec<(ProxyItemId, ItemState)>) -> usize;
    /// Every item the user has state for.
    fn user_items(&self, user: &str) -> Vec<(ProxyItemId, ItemState)>;
    /// Number of (user, item) states.
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests;
