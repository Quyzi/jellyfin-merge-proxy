//! [`PersistentViews`]: merged library views survive restarts.
//!
//! Merged view ids are derived from the member libraries, so they are
//! stable, but before this decorator the proxy only knew them after it had
//! listed the libraries once. Views are now saved when registered (or
//! changed) and re-registered on start, so a client's saved view id works
//! immediately.

use super::*;
use crate::store::{IndexStore, StoredView};

pub struct PersistentViews<M> {
    inner: M,
    servers: Arc<ServerRegistry>,
    store: Arc<dyn IndexStore>,
}

impl<M: IdMapper> PersistentViews<M> {
    /// Wrap `inner`, registering every stored view.
    pub fn load(
        inner: M,
        servers: Arc<ServerRegistry>,
        store: Arc<dyn IndexStore>,
    ) -> Result<Self, StoreError> {
        let views = store.load_views()?;
        let n = views.len();
        for v in views {
            inner.register_view(MergedView {
                id: v.id,
                name: v.name,
                collection_type: v.collection_type,
                members: v
                    .members
                    .into_iter()
                    .map(|(s, id)| (servers.id_for(&s), id))
                    .collect(),
                template: v.template,
            });
        }
        tracing::debug!("loaded {n} merged library views");
        Ok(PersistentViews {
            inner,
            servers,
            store,
        })
    }

    fn stored(&self, v: &MergedView) -> StoredView {
        StoredView {
            id: v.id.clone(),
            name: v.name.clone(),
            collection_type: v.collection_type.clone(),
            members: v
                .members
                .iter()
                .map(|(s, id)| (self.servers.name(*s).unwrap_or_default(), id.clone()))
                .collect(),
            template: v.template.clone(),
        }
    }
}

impl<M: IdMapper> IdMapper for PersistentViews<M> {
    fn encode(&self, server: ServerId, id: &UpstreamItemId) -> Encoded {
        self.inner.encode(server, id)
    }
    fn decode(&self, id: &ProxyItemId) -> Option<Target> {
        self.inner.decode(id)
    }
    fn register_view(&self, view: MergedView) -> Arc<MergedView> {
        let changed = match self.inner.decode(&view.id) {
            Some(Target::View(old)) => {
                old.name != view.name
                    || old.collection_type != view.collection_type
                    || old.members != view.members
                    || old.template != view.template
            }
            _ => true,
        };
        let v = self.inner.register_view(view);
        if changed {
            let outcome = match self.store.save_view(&self.stored(&v)) {
                Ok(()) => "success",
                Err(e) => {
                    tracing::warn!(view = %v.id, "saving merged view failed: {e}");
                    "error"
                }
            };
            counter!(names::PERSIST_OPS, "store" => "views", "op" => "save", "outcome" => outcome)
                .increment(1);
        }
        v
    }
    fn size_of(&self, server: ServerId) -> u64 {
        self.inner.size_of(server)
    }
    fn sizes(&self) -> Vec<(String, u64)> {
        self.inner.sizes()
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
    fn publish_gauges(&self) {
        self.inner.publish_gauges()
    }
}
