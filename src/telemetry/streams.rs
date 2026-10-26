//! Metered streaming bodies.

use super::names::*;
use super::unix_now;
use bytes::Bytes;
use futures_util::Stream;
use metrics::{counter, gauge, histogram};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::Instant;

#[derive(Clone, Debug, serde::Serialize)]
pub struct StreamInfo {
    pub id: u64,
    pub server: String,
    pub kind: &'static str,
    pub route: String,
    pub client: String,
    pub started_unix: f64,
    pub bytes: u64,
}

static STREAM_SEQ: AtomicU64 = AtomicU64::new(1);
static ACTIVE_STREAMS: OnceLock<Mutex<HashMap<u64, StreamInfo>>> = OnceLock::new();

fn active() -> &'static Mutex<HashMap<u64, StreamInfo>> {
    ACTIVE_STREAMS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn active_streams() -> Vec<StreamInfo> {
    active().lock().unwrap().values().cloned().collect()
}

type BoxStream = crate::backend::BodyStream;

pub struct MeteredStream {
    inner: BoxStream,
    id: u64,
    server: String,
    kind: &'static str,
    started: Instant,
    first: bool,
    done: bool,
    errored: bool,
    pending_bytes: u64,
}

impl MeteredStream {
    pub fn new(
        inner: BoxStream,
        server: &str,
        kind: &'static str,
        route: String,
        client: String,
        started: Instant,
    ) -> Self {
        let id = STREAM_SEQ.fetch_add(1, Ordering::Relaxed);
        gauge!(STREAMS_ACTIVE, "server" => server.to_string(), "kind" => kind).increment(1.0);
        active().lock().unwrap().insert(
            id,
            StreamInfo {
                id,
                server: server.to_string(),
                kind,
                route,
                client,
                started_unix: unix_now(),
                bytes: 0,
            },
        );
        MeteredStream {
            inner,
            id,
            server: server.to_string(),
            kind,
            started,
            first: true,
            done: false,
            errored: false,
            pending_bytes: 0,
        }
    }

    fn flush_bytes(&mut self) {
        if self.pending_bytes > 0 {
            counter!(STREAM_BYTES, "server" => self.server.clone(), "kind" => self.kind)
                .increment(self.pending_bytes);
            if let Some(i) = active().lock().unwrap().get_mut(&self.id) {
                i.bytes += self.pending_bytes;
            }
            self.pending_bytes = 0;
        }
    }
}

impl Stream for MeteredStream {
    type Item = Result<Bytes, crate::error::BackendError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let r = self.inner.as_mut().poll_next(cx);
        match &r {
            Poll::Ready(Some(Ok(b))) => {
                if self.first {
                    self.first = false;
                    histogram!(STREAM_TTFB, "server" => self.server.clone(), "kind" => self.kind)
                        .record(self.started.elapsed().as_secs_f64());
                }
                self.pending_bytes += b.len() as u64;
                if self.pending_bytes >= 1 << 20 {
                    self.flush_bytes();
                }
            }
            Poll::Ready(Some(Err(_))) => self.errored = true,
            Poll::Ready(None) => self.done = true,
            Poll::Pending => {}
        }
        r
    }
}

impl Drop for MeteredStream {
    fn drop(&mut self) {
        self.flush_bytes();
        let outcome = if self.errored {
            "upstream_error"
        } else if self.done {
            "complete"
        } else {
            "client_disconnect"
        };
        gauge!(STREAMS_ACTIVE, "server" => self.server.clone(), "kind" => self.kind).decrement(1.0);
        counter!(STREAM_END, "server" => self.server.clone(), "kind" => self.kind, "outcome" => outcome)
            .increment(1);
        active().lock().unwrap().remove(&self.id);
        tracing::debug!(server = %self.server, kind = self.kind, outcome, secs = self.started.elapsed().as_secs_f64(), "stream ended");
    }
}
