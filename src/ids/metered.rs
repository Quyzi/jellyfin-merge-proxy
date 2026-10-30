//! [`MeteredIdMapper`]: the `jmp_ids_*` metrics for any [`IdMapper`].

use super::*;

/// Decorator adding `jmp_ids_encoded_total`, `jmp_ids_new_total`,
/// `jmp_ids_decoded_total` and `jmp_ids_table_size` to any [`IdMapper`].
pub struct MeteredIdMapper<M> {
    inner: M,
    servers: Arc<ServerRegistry>,
}

impl<M: IdMapper> MeteredIdMapper<M> {
    pub fn new(inner: M, servers: Arc<ServerRegistry>) -> Self {
        let m = MeteredIdMapper { inner, servers };
        m.publish_gauges();
        m
    }
    pub fn inner(&self) -> &M {
        &self.inner
    }
    fn name(&self, s: ServerId) -> String {
        self.servers.name(s).unwrap_or_default()
    }
}

impl<M: IdMapper> IdMapper for MeteredIdMapper<M> {
    fn encode(&self, server: ServerId, id: &UpstreamItemId) -> Encoded {
        let e = self.inner.encode(server, id);
        let name = self.name(server);
        counter!(names::IDS_ENCODED, "server" => name.clone()).increment(1);
        if e.new {
            counter!(names::IDS_NEW, "server" => name.clone()).increment(1);
            gauge!(names::IDS_TABLE_SIZE, "server" => name).set(self.inner.size_of(server) as f64);
        }
        e
    }
    fn decode(&self, id: &ProxyItemId) -> Option<Target> {
        let r = self.inner.decode(id);
        let outcome = match &r {
            Some(Target::Item { .. }) => "hit",
            Some(Target::View(_)) => "view",
            None => "miss",
        };
        counter!(names::IDS_DECODED, "outcome" => outcome).increment(1);
        r
    }
    fn resolve(&self, raw: &str) -> Option<Target> {
        match ProxyItemId::parse(raw) {
            Some(p) => self.decode(&p),
            None => {
                counter!(names::IDS_DECODED, "outcome" => "invalid").increment(1);
                None
            }
        }
    }
    fn register_view(&self, view: MergedView) -> Arc<MergedView> {
        self.inner.register_view(view)
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
        for (n, size) in self.inner.sizes() {
            gauge!(names::IDS_TABLE_SIZE, "server" => n).set(size as f64);
        }
    }
}
