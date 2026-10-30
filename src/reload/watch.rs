//! Watchers: what triggers a reload (file polling, SIGHUP).

use super::*;

// ---------------------------------------------------------------------------
// Watchers
// ---------------------------------------------------------------------------

/// A detected change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    /// Referenced files (not just the config text) changed.
    pub files_changed: bool,
}

/// Something that tells us when to reload.
#[async_trait]
pub trait ConfigWatcher: Send {
    fn name(&self) -> &'static str;
    /// Wait for the next change; `None` ends the watcher.
    async fn next_change(&mut self, st: &Shared) -> Option<Change>;
}

/// File fingerprint: modification time and size (catches editors that keep
/// the mtime, and same-second edits that change the length).
type Stamp = Option<(SystemTime, u64)>;

/// Polls the config file and every file it references every
/// `[proxy] reload_interval_secs` (default 3, 0 = paused; SIGHUP still works).
pub struct PollingWatcher {
    snapshot: Option<Vec<(PathBuf, Stamp)>>,
}

impl PollingWatcher {
    pub fn new() -> Self {
        PollingWatcher { snapshot: None }
    }
    fn take(st: &Shared) -> Vec<(PathBuf, Stamp)> {
        let Some(src) = &st.source else {
            return vec![];
        };
        src.watched_paths(&st.cfg())
            .into_iter()
            .map(|p| {
                let m = stamp(&p);
                (p, m)
            })
            .collect()
    }
}

impl Default for PollingWatcher {
    fn default() -> Self {
        Self::new()
    }
}

fn stamp(p: &Path) -> Stamp {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

#[async_trait]
impl ConfigWatcher for PollingWatcher {
    fn name(&self) -> &'static str {
        "poll"
    }
    async fn next_change(&mut self, st: &Shared) -> Option<Change> {
        st.source.as_ref()?;
        if self.snapshot.is_none() {
            self.snapshot = Some(Self::take(st));
        }
        loop {
            let secs = st.cfg().proxy.reload_interval_secs;
            tokio::time::sleep(Duration::from_secs(secs.max(1))).await;
            if secs == 0 {
                continue;
            }
            let now = Self::take(st);
            let before = self.snapshot.as_ref().unwrap();
            // Compare only paths present in both snapshots; a changed path
            // *set* (after a reload) just re-baselines.
            let mut cfg_changed = false;
            let mut files_changed = false;
            for (i, (p, m)) in now.iter().enumerate() {
                if let Some((_, om)) = before.iter().find(|(op, _)| op == p) {
                    if om != m {
                        if i == 0 {
                            cfg_changed = true;
                        } else {
                            files_changed = true;
                        }
                    }
                }
            }
            self.snapshot = Some(now);
            if cfg_changed || files_changed {
                tracing::info!(
                    config_changed = cfg_changed,
                    files_changed,
                    "change detected, reloading"
                );
                return Some(Change { files_changed });
            }
        }
    }
}

/// Reloads on SIGHUP (unix only; never fires elsewhere).
pub struct SighupWatcher {
    #[cfg(unix)]
    sig: Option<tokio::signal::unix::Signal>,
}

impl SighupWatcher {
    pub fn new() -> Self {
        SighupWatcher {
            #[cfg(unix)]
            sig: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok(),
        }
    }
}

impl Default for SighupWatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ConfigWatcher for SighupWatcher {
    fn name(&self) -> &'static str {
        "sighup"
    }
    async fn next_change(&mut self, _st: &Shared) -> Option<Change> {
        #[cfg(unix)]
        {
            self.sig.as_mut()?.recv().await?;
            tracing::info!("SIGHUP: reloading config");
            Some(Change {
                files_changed: true,
            })
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

/// Drive a watcher: every change triggers `st.reload`.
pub fn spawn_watcher(mut w: Box<dyn ConfigWatcher>, st: Shared) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tracing::debug!(watcher = w.name(), "config watcher started");
        while let Some(c) = w.next_change(&st).await {
            let _ = st.reload(c.files_changed).await;
        }
    })
}
