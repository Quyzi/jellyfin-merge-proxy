//! Grouping upstream library views into merged views ([`MergeStrategy`]).

use super::*;

/// Result of grouping library views.
#[derive(Debug)]
pub enum ViewGroup {
    Single(ServerId, Value),
    Merged {
        name: String,
        collection_type: Option<String>,
        members: Vec<(ServerId, Value)>,
    },
}

/// Decides how libraries ("views") from several servers combine.
pub trait MergeStrategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn group_views(&self, per_server: Vec<(ServerId, Vec<Value>)>) -> Vec<ViewGroup>;
}

/// Libraries with the same name and collection type become one view.
#[derive(Debug, Default, Clone, Copy)]
pub struct MergeByNameAndType;

/// Every library stays separate.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeepSeparate;

impl MergeStrategy for MergeByNameAndType {
    fn name(&self) -> &'static str {
        "by_name_and_type"
    }
    fn group_views(&self, per: Vec<(ServerId, Vec<Value>)>) -> Vec<ViewGroup> {
        group_views(per, true)
    }
}

impl MergeStrategy for KeepSeparate {
    fn name(&self) -> &'static str {
        "separate"
    }
    fn group_views(&self, per: Vec<(ServerId, Vec<Value>)>) -> Vec<ViewGroup> {
        group_views(per, false)
    }
}

/// The strategy selected by `[merge] merge_libraries_by_name`.
pub fn strategy_for(cfg: &crate::config::MergeConfig) -> &'static dyn MergeStrategy {
    if cfg.merge_libraries_by_name {
        &MergeByNameAndType
    } else {
        &KeepSeparate
    }
}

pub fn group_views(
    per_upstream: Vec<(ServerId, Vec<Value>)>,
    merge_by_name: bool,
) -> Vec<ViewGroup> {
    let mut order: Vec<(String, Option<String>)> = vec![];
    type Key = (String, Option<String>);
    let mut groups: HashMap<Key, Vec<(ServerId, Value)>> = HashMap::new();
    let mut singles: Vec<ViewGroup> = vec![];
    let mut out_order: Vec<Result<usize, (String, Option<String>)>> = vec![];
    for (u, views) in per_upstream {
        for v in views {
            if !merge_by_name {
                singles.push(ViewGroup::Single(u, v));
                out_order.push(Ok(singles.len() - 1));
                continue;
            }
            let name = v
                .get("Name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let ct = v
                .get("CollectionType")
                .and_then(|n| n.as_str())
                .map(|s| s.to_ascii_lowercase());
            let key = (name.to_lowercase(), ct);
            if !groups.contains_key(&key) {
                order.push(key.clone());
                out_order.push(Err(key.clone()));
            }
            groups.entry(key).or_default().push((u, v));
        }
    }
    let mut singles: Vec<Option<ViewGroup>> = singles.into_iter().map(Some).collect();
    let mut result = vec![];
    for o in out_order {
        match o {
            Ok(i) => result.push(singles[i].take().unwrap()),
            Err(key) => {
                let mut members = groups.remove(&key).unwrap_or_default();
                if members.len() == 1 {
                    let (u, v) = members.pop().unwrap();
                    result.push(ViewGroup::Single(u, v));
                } else if !members.is_empty() {
                    let name = members[0]
                        .1
                        .get("Name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let collection_type = members[0]
                        .1
                        .get("CollectionType")
                        .and_then(|n| n.as_str())
                        .map(String::from);
                    result.push(ViewGroup::Merged {
                        name,
                        collection_type,
                        members,
                    });
                }
            }
        }
    }
    result
}
