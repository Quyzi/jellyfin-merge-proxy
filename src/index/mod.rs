//! The canonical catalog index.
//!
//! Every server's catalog is crawled ([`crawl`]) into compact [`entry::Entry`]
//! records and persisted ([`crate::store::IndexStore`]). A [`snapshot::Snapshot`]
//! groups copies of the same item on several servers ([`grouping`]), ranks
//! them ([`rank`]) and merges genres/studios/people by name, each group
//! getting a stable canonical id. Library queries are then answered from
//! the snapshot ([`query`], [`sort`]) with exact totals, paging and
//! Jellyfin's sort semantics; queries it cannot express fall back to the
//! live fan-out.

pub mod canonical;
pub mod clean;
pub mod crawl;
pub mod entry;
pub mod grouping;
pub mod groups;
pub mod query;
pub mod rank;
pub mod service;
pub mod snapshot;
pub mod sort;
