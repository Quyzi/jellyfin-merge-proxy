//! State the proxy owns itself rather than forwarding to an upstream:
//! per-user settings, display preferences, playlists and live sessions.
//! Each piece is persisted through [`crate::store::LocalStore`].

pub mod password;
pub mod playlists;
pub mod policy;
pub mod prefs;
pub mod settings;

pub use policy::policy;

use crate::live::play_sessions::PlaySessions;
use crate::live::LiveSessions;
use crate::store::Store;
use std::sync::Arc;

pub struct Local {
    pub settings: settings::UserSettings,
    /// Which upstream issued each `PlaySessionId` (item 10).
    pub play_sessions: Arc<PlaySessions>,
    /// Connected client sessions and their sockets.
    pub live: Arc<LiveSessions>,
    pub prefs: prefs::DisplayPrefs,
    /// Playlists owned by proxy users (upstream playlists stay upstream).
    pub playlists: playlists::Playlists,
    /// Serialises password changes (config file rewrite + reload).
    pub password_lock: tokio::sync::Mutex<()>,
}

impl Local {
    pub fn new(
        store: Arc<dyn Store>,
        play_sessions: Arc<PlaySessions>,
        live: Arc<LiveSessions>,
    ) -> Self {
        Self {
            settings: settings::UserSettings::load(store.clone()),
            play_sessions,
            live,
            prefs: prefs::DisplayPrefs::load(store.clone()),
            playlists: playlists::Playlists::load(store),
            password_lock: tokio::sync::Mutex::new(()),
        }
    }
}
