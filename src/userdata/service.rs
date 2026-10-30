//! [`UserDataService`]: the per-user state operations used by endpoints,
//! the index queries and the response overlay.

use super::rules::{self, ResumeRules};
use super::{ItemState, WatchState};
use crate::config::UserDataConfig;
use crate::index::service::IndexService;
use crate::index::snapshot::Snapshot;
use crate::types::ProxyItemId;
use arc_swap::ArcSwap;
use std::sync::Arc;

/// Which playback report a `Sessions/Playing*` request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayEvent {
    Start,
    Progress,
    Stopped,
}

impl PlayEvent {
    /// From the last path segment (`playing`, `progress`, `stopped`).
    pub fn from_seg(seg: &str) -> Option<Self> {
        match seg.to_ascii_lowercase().as_str() {
            "playing" => Some(Self::Start),
            "progress" => Some(Self::Progress),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

/// Played/unplayed episode counts of a series or season.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderCounts {
    pub total: usize,
    pub unplayed: usize,
}

pub struct UserDataService {
    table: Arc<dyn WatchState>,
    index: Arc<IndexService>,
    cfg: ArcSwap<UserDataConfig>,
}

impl UserDataService {
    pub fn new(table: Arc<dyn WatchState>, index: Arc<IndexService>, cfg: &UserDataConfig) -> Self {
        UserDataService {
            table,
            index,
            cfg: ArcSwap::from_pointee(cfg.clone()),
        }
    }

    pub fn configure(&self, cfg: &UserDataConfig) {
        self.cfg.store(Arc::new(cfg.clone()));
    }

    pub fn config(&self) -> Arc<UserDataConfig> {
        self.cfg.load_full()
    }

    pub fn enabled(&self) -> bool {
        self.cfg.load().enabled
    }

    pub fn rules(&self) -> ResumeRules {
        self.cfg.load().as_ref().into()
    }

    pub fn table(&self) -> &Arc<dyn WatchState> {
        &self.table
    }

    /// The key state is stored under: the canonical id of the item's group.
    pub fn key(&self, id: &ProxyItemId) -> ProxyItemId {
        self.index
            .group(id)
            .map(|g| g.canonical.clone())
            .unwrap_or_else(|| id.clone())
    }

    fn snap(&self) -> Option<Arc<Snapshot>> {
        self.index.latest()
    }

    /// Runtime (ticks) of an item, from the index.
    pub fn runtime(&self, id: &ProxyItemId) -> Option<i64> {
        let snap = self.snap()?;
        let it = snap.item(id)?;
        snap.primary(it).runtime
    }

    /// The user's own state of an item (no folder aggregation).
    pub fn own(&self, user: &str, id: &ProxyItemId) -> ItemState {
        self.table.get(user, &self.key(id)).unwrap_or_default()
    }

    /// State as shown: for a series or season, played/last played come
    /// from its episodes.
    pub fn effective(&self, user: &str, id: &ProxyItemId) -> (ItemState, Option<FolderCounts>) {
        let own = self.own(user, id);
        match self.snap() {
            Some(snap) => match snap.item_index(id) {
                Some(i) => aggregate(&snap, i, own, &|k| self.table.get(user, k)),
                None => (own, None),
            },
            None => (own, None),
        }
    }

    /// Per-item state for index queries (canonical item `idx`).
    pub fn fact(&self, user: &str, snap: &Snapshot, idx: usize) -> ItemState {
        let own = self
            .table
            .get(user, &snap.items[idx].id)
            .unwrap_or_default();
        aggregate(snap, idx, own, &|k| self.table.get(user, k)).0
    }

    /// Change the user's own state of one item.
    pub fn change(
        &self,
        user: &str,
        id: &ProxyItemId,
        op: &'static str,
        f: &dyn Fn(&mut ItemState),
    ) -> ItemState {
        let key = self.key(id);
        self.table
            .update(user, std::slice::from_ref(&key), op, f)
            .pop()
            .unwrap_or_default()
    }

    /// Apply a playback report (Jellyfin's play-state rules). A stop for
    /// an item of unknown runtime keeps the position and does not mark it
    /// played.
    pub fn playback(
        &self,
        user: &str,
        id: &ProxyItemId,
        ev: PlayEvent,
        position: Option<i64>,
        now: &str,
    ) -> ItemState {
        let rules = self.rules();
        match ev {
            PlayEvent::Start => self.change(user, id, "start", &|s| rules::start(s, now)),
            PlayEvent::Progress => {
                self.change(user, id, "progress", &|s| rules::progress(s, position))
            }
            PlayEvent::Stopped => match self.runtime(id) {
                Some(rt) => self.change(user, id, "stopped", &|s| {
                    rules::stopped(s, position, Some(rt), &rules, now);
                }),
                None => self.change(user, id, "stopped", &|s| rules::progress(s, position)),
            },
        }
    }

    /// Mark played/unplayed. A series or season marks all its episodes
    /// (as Jellyfin does for folders).
    pub fn set_played(&self, user: &str, id: &ProxyItemId, played: bool, now: &str) {
        let op = if played { "played" } else { "unplayed" };
        let mut keys = vec![self.key(id)];
        if let Some(snap) = self.snap() {
            if let Some(i) = snap.item_index(id) {
                keys.extend(
                    snap.episodes_of(i)
                        .iter()
                        .map(|e| snap.items[*e].id.clone()),
                );
            }
        }
        self.table
            .update(user, &keys, op, &|s| rules::mark_played(s, played, now));
    }
}

/// `own` with played state derived from the episodes of a series/season.
pub fn aggregate(
    snap: &Snapshot,
    idx: usize,
    mut own: ItemState,
    get: &dyn Fn(&ProxyItemId) -> Option<ItemState>,
) -> (ItemState, Option<FolderCounts>) {
    let eps = snap.episodes_of(idx);
    if eps.is_empty() {
        return (own, None);
    }
    let mut unplayed = 0;
    let mut last: Option<String> = None;
    let mut count = 0;
    for e in eps {
        let s = get(&snap.items[*e].id).unwrap_or_default();
        if !s.played {
            unplayed += 1;
        }
        count += s.play_count;
        if s.last_played > last {
            last = s.last_played.clone();
        }
    }
    own.played = unplayed == 0;
    own.play_count = count;
    own.position_ticks = 0;
    own.last_played = last;
    let counts = FolderCounts {
        total: eps.len(),
        unplayed,
    };
    (own, Some(counts))
}

#[async_trait::async_trait]
impl crate::reload::Reloadable for UserDataService {
    fn reload_name(&self) -> String {
        "userdata".into()
    }
    async fn apply(
        &self,
        cx: &crate::reload::ReloadContext<'_>,
    ) -> Result<bool, crate::error::ReloadError> {
        if cx.old.userdata == cx.new.userdata {
            return Ok(false);
        }
        self.configure(&cx.new.userdata);
        Ok(true)
    }
}
