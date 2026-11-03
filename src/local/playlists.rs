//! Playlists the proxy owns: per proxy user, with Jellyfin's sharing model
//! (owner, edit/read shares, open access). Entries are canonical item ids,
//! so one playlist can mix items from several upstream servers. Persisted
//! through [`crate::store::LocalStore`]; upstream playlists are not touched.

use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Share {
    pub user_id: String,
    pub can_edit: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistRec {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub media_type: String,
    pub open_access: bool,
    pub shares: Vec<Share>,
    /// Canonical item ids in playlist order; each is also its entry id.
    pub items: Vec<String>,
    pub created: String,
}

impl PlaylistRec {
    /// `Playlist.IsVisible`: the owner, anyone with open access, shares.
    pub fn visible(&self, user: &str) -> bool {
        self.owner == user || self.open_access || self.shares.iter().any(|s| s.user_id == user)
    }

    pub fn can_edit(&self, user: &str) -> bool {
        self.owner == user || self.shares.iter().any(|s| s.user_id == user && s.can_edit)
    }

    /// Add `ids` that are not in the playlist yet (and not repeated).
    pub fn add(&mut self, ids: &[String]) {
        for id in ids {
            if !self.items.contains(id) {
                self.items.push(id.clone());
            }
        }
    }

    /// `PlaylistManager.MoveItemAsync`: `new_index` counts only the entries
    /// the user may see (`blocked` hides the rest), the move is applied to
    /// the whole list.
    pub fn move_item(&mut self, entry: &str, new_index: usize, blocked: impl Fn(&str) -> bool) {
        let acc: Vec<usize> = (0..self.items.len())
            .filter(|&i| !blocked(&self.items[i]))
            .collect();
        let Some(old) = acc.iter().position(|&i| self.items[i] == entry) else {
            return;
        };
        if old == new_index {
            return;
        }
        let prior = acc[new_index.saturating_sub(1).min(acc.len() - 1)];
        let adjusted = if new_index == 0 {
            prior.saturating_sub(1)
        } else {
            prior + 1
        };
        let item = self
            .items
            .remove(self.items.iter().position(|i| i == entry).unwrap());
        if new_index >= self.items.len() {
            self.items.push(item);
        } else {
            self.items.insert(adjusted.min(self.items.len()), item);
        }
    }
}

pub struct Playlists {
    store: Arc<dyn Store>,
    map: RwLock<HashMap<String, PlaylistRec>>,
}

impl Playlists {
    pub fn load(store: Arc<dyn Store>) -> Self {
        let map = match store.load_playlists() {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|v| match serde_json::from_value::<PlaylistRec>(v) {
                    Ok(r) => Some((r.id.clone(), r)),
                    Err(e) => {
                        tracing::warn!("skipping playlist row: {e}");
                        None
                    }
                })
                .collect(),
            Err(e) => {
                tracing::warn!("loading playlists: {e}");
                HashMap::new()
            }
        };
        Self {
            store,
            map: RwLock::new(map),
        }
    }

    pub fn len(&self) -> usize {
        self.map.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, id: &str) -> Option<PlaylistRec> {
        self.map.read().unwrap().get(id).cloned()
    }

    pub fn is_playlist(&self, id: &str) -> bool {
        self.map.read().unwrap().contains_key(id)
    }

    /// Playlists `user` can see, by name.
    pub fn for_user(&self, user: &str) -> Vec<PlaylistRec> {
        let mut v: Vec<PlaylistRec> = self
            .map
            .read()
            .unwrap()
            .values()
            .filter(|p| p.visible(user))
            .cloned()
            .collect();
        v.sort_by(|a, b| (a.name.to_lowercase(), &a.id).cmp(&(b.name.to_lowercase(), &b.id)));
        v
    }

    /// Insert or replace, writing through to the store.
    pub fn upsert(&self, rec: PlaylistRec) {
        match serde_json::to_value(&rec) {
            Ok(v) => {
                if let Err(e) = self.store.put_playlist(&rec.id, &rec.owner, &v) {
                    tracing::warn!("persisting playlist: {e}");
                }
            }
            Err(e) => tracing::warn!("serialising playlist: {e}"),
        }
        self.map.write().unwrap().insert(rec.id.clone(), rec);
    }

    /// Change a playlist in place and persist it. `None` if it is gone.
    pub fn update<R>(&self, id: &str, f: impl FnOnce(&mut PlaylistRec) -> R) -> Option<R> {
        let mut rec = self.get(id)?;
        let r = f(&mut rec);
        self.upsert(rec);
        Some(r)
    }

    pub fn delete(&self, id: &str) -> bool {
        if let Err(e) = self.store.delete_playlist(id) {
            tracing::warn!("deleting playlist: {e}");
        }
        self.map.write().unwrap().remove(id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(items: &[&str]) -> PlaylistRec {
        PlaylistRec {
            id: "p".into(),
            owner: "o".into(),
            name: "n".into(),
            media_type: "Video".into(),
            open_access: false,
            shares: vec![Share {
                user_id: "r".into(),
                can_edit: false,
            }],
            items: items.iter().map(|s| s.to_string()).collect(),
            created: String::new(),
        }
    }

    /// Visibility and edit rights follow Jellyfin: open access reads, only
    /// owner and edit-shares write.
    #[test]
    fn visibility_and_edit_rights() {
        let mut p = rec(&[]);
        assert!(p.visible("o") && p.visible("r") && !p.visible("x"));
        assert!(p.can_edit("o") && !p.can_edit("r"));
        p.open_access = true;
        assert!(p.visible("x") && !p.can_edit("x"));
    }

    /// Moving an entry lands it where the client sees the new index, even
    /// when entries the user may not see sit in between.
    #[test]
    fn move_counts_only_visible_entries() {
        let mut p = rec(&["a", "b", "c", "d"]);
        p.move_item("d", 0, |_| false);
        assert_eq!(p.items, ["d", "a", "b", "c"]);
        p.move_item("d", 3, |_| false);
        assert_eq!(p.items, ["a", "b", "c", "d"]);
        p.move_item("a", 4, |_| false);
        assert_eq!(p.items, ["b", "c", "d", "a"]);
        let mut p = rec(&["a", "x", "b", "c"]);
        // "x" is hidden: visible list is a, b, c; moving c to index 1.
        p.move_item("c", 1, |i| i == "x");
        assert_eq!(p.items, ["a", "c", "x", "b"]);
        let before = p.items.clone();
        p.move_item("zzz", 0, |_| false);
        p.move_item("a", 0, |_| false);
        assert_eq!(p.items, before, "unknown entry or same index: unchanged");
    }
}
