//! A proxy user's `UserPolicy`: fixed non-admin capabilities, plus the
//! parental rating limits the proxy enforces for that user.

use crate::Shared;
use serde_json::{json, Value};

const BLOCKED_KINDS: [&str; 5] = ["Movie", "Trailer", "Series", "Music", "Other"];

pub fn policy(st: &Shared, user: &str) -> Value {
    let mut v = json!({
        "IsAdministrator": false,
        "IsHidden": true,
        "EnableCollectionManagement": false,
        "EnableSubtitleManagement": false,
        "EnableLyricManagement": false,
        "IsDisabled": false,
        "BlockedTags": [],
        "AllowedTags": [],
        "EnableUserPreferenceAccess": true,
        "AccessSchedules": [],
        "BlockUnratedItems": [],
        "EnableRemoteControlOfOtherUsers": false,
        "EnableSharedDeviceControl": false,
        "EnableRemoteAccess": true,
        "EnableLiveTvManagement": false,
        "EnableLiveTvAccess": true,
        "EnableMediaPlayback": true,
        "EnableAudioPlaybackTranscoding": true,
        "EnableVideoPlaybackTranscoding": true,
        "EnablePlaybackRemuxing": true,
        "ForceRemoteSourceTranscoding": false,
        "EnableContentDeletion": false,
        "EnableContentDeletionFromFolders": [],
        "EnableContentDownloading": true,
        "EnableSyncTranscoding": true,
        "EnableMediaConversion": false,
        "EnabledDevices": [],
        "EnableAllDevices": true,
        "EnabledChannels": [],
        "EnableAllChannels": true,
        "EnabledFolders": [],
        "EnableAllFolders": true,
        "InvalidLoginAttemptCount": 0,
        "LoginAttemptsBeforeLockout": -1,
        "MaxActiveSessions": 0,
        "EnablePublicSharing": false,
        "BlockedMediaFolders": [],
        "BlockedChannels": [],
        "RemoteClientBitrateLimit": 0,
        "AuthenticationProviderId": "Jellyfin.Server.Implementations.Users.DefaultAuthenticationProvider",
        "PasswordResetProviderId": "Jellyfin.Server.Implementations.Users.DefaultPasswordResetProvider",
        "SyncPlayAccess": "None"
    });
    if let Some((p, _)) = st.parental.current(user) {
        v["MaxParentalRating"] = json!(p.max);
        if p.block_unrated {
            v["BlockUnratedItems"] = json!(BLOCKED_KINDS);
        }
    }
    v
}
