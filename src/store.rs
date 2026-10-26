//! Durable proxy state: id mappings, series sibling links and sessions.
//!
//! The in-memory tables ([`crate::ids::HashedIdTable`],
//! [`crate::siblings::SiblingTable`], [`crate::auth::Sessions`]) stay the
//! source of truth on the request path; they load from a store at startup
//! and write new entries through to it. [`IdStore`], [`SiblingStore`] and
//! [`SessionStore`] are the seams, [`Store`] bundles them, and
//! [`MemoryStore`] is the non-durable implementation used in tests and when
//! `data_dir` is not writable.

use crate::auth::Session;
use crate::types::{ProxyItemId, UpstreamItemId};
use std::path::PathBuf;

mod memory;
pub mod migrations;

pub use memory::MemoryStore;

/// Errors from a [`Store`].
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than this build supports ({supported}); upgrade jellyfin-merge-proxy")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Invalid(String),
}

/// One persisted id mapping: `proxy_id` decodes to `upstream_id` on the
/// server called `server`. The proxy id is stored as handed to clients, so
/// it never has to be recomputed to stay valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdRow {
    pub proxy_id: ProxyItemId,
    pub server: String,
    pub upstream_id: UpstreamItemId,
}

/// Persistence for the proxy id table.
pub trait IdStore: Send + Sync {
    /// Every mapping, in insertion order.
    fn load_ids(&self) -> Result<Vec<IdRow>, StoreError>;
    /// Record mappings; rows whose proxy id is already stored are ignored.
    fn append_ids(&self, rows: &[IdRow]) -> Result<(), StoreError>;
}

/// Persistence for series sibling links (pairs of proxy ids).
pub trait SiblingStore: Send + Sync {
    fn load_links(&self) -> Result<Vec<(ProxyItemId, ProxyItemId)>, StoreError>;
    /// Record links; already stored pairs are ignored.
    fn append_links(&self, links: &[(ProxyItemId, ProxyItemId)]) -> Result<(), StoreError>;
}

/// Persistence for client sessions, keyed by token.
pub trait SessionStore: Send + Sync {
    fn load_sessions(&self) -> Result<Vec<Session>, StoreError>;
    /// Insert or replace the session with this token.
    fn put_session(&self, s: &Session) -> Result<(), StoreError>;
    fn remove_sessions(&self, tokens: &[String]) -> Result<(), StoreError>;
}

/// State read from somewhere else (e.g. the pre-database files) to be
/// written into a [`Store`] in one go.
#[derive(Debug, Default, Clone)]
pub struct ImportBatch {
    pub ids: Vec<IdRow>,
    pub links: Vec<(ProxyItemId, ProxyItemId)>,
    pub sessions: Vec<Session>,
}

impl ImportBatch {
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty() && self.links.is_empty() && self.sessions.is_empty()
    }
}

/// All proxy state behind one handle.
pub trait Store: IdStore + SiblingStore + SessionStore {
    /// Short name for logs ("sqlite", "memory").
    fn kind(&self) -> &'static str;

    /// Write a whole batch. Implementations with transactions override this
    /// so the batch lands completely or not at all.
    fn import(&self, batch: &ImportBatch) -> Result<(), StoreError> {
        self.append_ids(&batch.ids)?;
        self.append_links(&batch.links)?;
        for s in &batch.sessions {
            self.put_session(s)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
