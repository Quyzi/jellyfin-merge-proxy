//! Persistence for per-proxy-user watch state ([`crate::userdata`]).

use super::StoreError;
use crate::types::ProxyItemId;
use crate::userdata::ItemState;
use std::collections::{HashMap, HashSet};

/// One user's state of one item.
#[derive(Debug, Clone, PartialEq)]
pub struct UserDataRow {
    pub user: String,
    pub item: ProxyItemId,
    pub state: ItemState,
}

pub trait UserDataStore: Send + Sync {
    fn load_user_data(&self) -> Result<Vec<UserDataRow>, StoreError>;
    /// Insert or replace rows (by user and item).
    fn put_user_data(&self, rows: &[UserDataRow]) -> Result<(), StoreError>;
    /// (user, server name) pairs already seeded from upstream.
    fn load_seeded(&self) -> Result<Vec<(String, String)>, StoreError>;
    fn mark_seeded(&self, user: &str, server: &str) -> Result<(), StoreError>;
}

/// In-memory part of [`super::MemoryStore`].
#[derive(Default)]
pub(super) struct MemUserData {
    rows: HashMap<(String, ProxyItemId), ItemState>,
    seeded: HashSet<(String, String)>,
}

impl MemUserData {
    pub(super) fn load(&self) -> Vec<UserDataRow> {
        let mut v: Vec<UserDataRow> = self
            .rows
            .iter()
            .map(|((user, item), state)| UserDataRow {
                user: user.clone(),
                item: item.clone(),
                state: state.clone(),
            })
            .collect();
        v.sort_by(|a, b| (&a.user, &a.item).cmp(&(&b.user, &b.item)));
        v
    }
    pub(super) fn put(&mut self, rows: &[UserDataRow]) {
        for r in rows {
            self.rows
                .insert((r.user.clone(), r.item.clone()), r.state.clone());
        }
    }
    pub(super) fn seeded(&self) -> Vec<(String, String)> {
        let mut v: Vec<_> = self.seeded.iter().cloned().collect();
        v.sort();
        v
    }
    pub(super) fn mark(&mut self, user: &str, server: &str) {
        self.seeded.insert((user.to_string(), server.to_string()));
    }
}
