//! The server set.
//!
//! * [`ServerRegistry`]: append-only `name ⇄ ServerId` table. Ids are never
//!   reused, so an id handed out for a server that was later removed still
//!   resolves to *that* server instead of pointing at some other server.

use crate::types::ServerId;
use std::sync::{Arc, RwLock};

/// Append-only `name ⇄ ServerId` table (names compared case-insensitively).
#[derive(Debug, Default)]
pub struct ServerRegistry {
    names: RwLock<Vec<String>>,
}

impl ServerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with `names` registered in order (ids 0, 1, ...).
    pub fn with_names<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> Arc<Self> {
        let r = Self::new();
        for n in names {
            r.id_for(n.as_ref());
        }
        Arc::new(r)
    }

    /// The id for `name`, registering it if it is new.
    pub fn id_for(&self, name: &str) -> ServerId {
        if let Some(id) = self.lookup(name) {
            return id;
        }
        let mut n = self.names.write().unwrap();
        if let Some(i) = n.iter().position(|x| x.eq_ignore_ascii_case(name)) {
            return ServerId(i);
        }
        n.push(name.to_string());
        ServerId(n.len() - 1)
    }

    /// The id `name` has or would get next, without registering it.
    pub fn peek_ids<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Vec<ServerId> {
        let n = self.names.read().unwrap();
        let mut next = n.len();
        names
            .into_iter()
            .map(
                |name| match n.iter().position(|x| x.eq_ignore_ascii_case(name)) {
                    Some(i) => ServerId(i),
                    None => {
                        next += 1;
                        ServerId(next - 1)
                    }
                },
            )
            .collect()
    }

    pub fn lookup(&self, name: &str) -> Option<ServerId> {
        self.names
            .read()
            .unwrap()
            .iter()
            .position(|x| x.eq_ignore_ascii_case(name))
            .map(ServerId)
    }

    pub fn name(&self, id: ServerId) -> Option<String> {
        self.names.read().unwrap().get(id.0).cloned()
    }

    /// All names ever registered, indexed by `ServerId`.
    pub fn names(&self) -> Vec<String> {
        self.names.read().unwrap().clone()
    }

    pub fn len(&self) -> usize {
        self.names.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_append_only() {
        let r = ServerRegistry::new();
        assert_eq!(r.id_for("a"), ServerId(0));
        assert_eq!(r.id_for("b"), ServerId(1));
        assert_eq!(r.id_for("A"), ServerId(0));
        assert_eq!(
            r.peek_ids(["b", "c", "d"]),
            [ServerId(1), ServerId(2), ServerId(3)]
        );
        assert_eq!(r.len(), 2, "peek does not register");
        assert_eq!(r.id_for("c"), ServerId(2));
        assert_eq!(r.name(ServerId(1)).as_deref(), Some("b"));
    }
}
