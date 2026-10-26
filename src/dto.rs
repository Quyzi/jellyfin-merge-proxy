//! Synthesised Jellyfin DTOs for things the proxy answers itself.

use crate::auth::Session;
use serde_json::{json, Value};

pub fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f0Z")
        .to_string()
}

pub fn public_info(name: &str, id: &str, version: &str, local_address: &str) -> Value {
    json!({
        "LocalAddress": local_address,
        "ServerName": name,
        "Version": version,
        "ProductName": "Jellyfin Server",
        "OperatingSystem": "Linux",
        "Id": id,
        "StartupWizardCompleted": true
    })
}

pub fn system_info(name: &str, id: &str, version: &str, local_address: &str) -> Value {
    let mut v = public_info(name, id, version, local_address);
    let extra = json!({
        "OperatingSystemDisplayName": "Linux",
        "PackageName": "jellyfin-merge-proxy",
        "HasPendingRestart": false,
        "IsShuttingDown": false,
        "SupportsLibraryMonitor": false,
        "WebSocketPortNumber": 8096,
        "CompletedInstallations": [],
        "CanSelfRestart": false,
        "CanLaunchWebBrowser": false,
        "ProgramDataPath": "/config",
        "WebPath": "/web",
        "ItemsByNamePath": "/config/metadata",
        "CachePath": "/cache",
        "LogPath": "/config/log",
        "InternalMetadataPath": "/config/metadata",
        "TranscodingTempPath": "/cache/transcodes",
        "CastReceiverApplications": [],
        "HasUpdateAvailable": false,
        "EncoderLocation": "System",
        "SystemArchitecture": "X64"
    });
    if let (Value::Object(a), Value::Object(b)) = (&mut v, extra) {
        a.extend(b);
    }
    v
}

pub fn user_dto(name: &str, id: &str, server_id: &str, server_name: &str) -> Value {
    let now = now_iso();
    json!({
        "Name": name,
        "ServerId": server_id,
        "ServerName": server_name,
        "Id": id,
        "HasPassword": true,
        "HasConfiguredPassword": true,
        "HasConfiguredEasyPassword": false,
        "EnableAutoLogin": false,
        "LastLoginDate": now,
        "LastActivityDate": now,
        "Configuration": {
            "PlayDefaultAudioTrack": true,
            "SubtitleLanguagePreference": "",
            "DisplayMissingEpisodes": false,
            "GroupedFolders": [],
            "SubtitleMode": "Default",
            "DisplayCollectionsView": false,
            "EnableLocalPassword": false,
            "OrderedViews": [],
            "LatestItemsExcludes": [],
            "MyMediaExcludes": [],
            "HidePlayedInLatest": true,
            "RememberAudioSelections": true,
            "RememberSubtitleSelections": true,
            "EnableNextEpisodeAutoPlay": true,
            "CastReceiverId": "F007D354"
        },
        "Policy": {
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
        }
    })
}

pub fn session_info(s: &Session, server_id: &str) -> Value {
    let now = now_iso();
    json!({
        "PlayState": {"CanSeek": false, "IsPaused": false, "IsMuted": false, "RepeatMode": "RepeatNone", "PlaybackOrder": "Default"},
        "AdditionalUsers": [],
        "Capabilities": {"PlayableMediaTypes": [], "SupportedCommands": [], "SupportsMediaControl": false, "SupportsPersistentIdentifier": true},
        "RemoteEndPoint": "",
        "PlayableMediaTypes": [],
        "Id": s.session_id,
        "UserId": s.user_id,
        "UserName": s.user_name,
        "Client": s.client,
        "LastActivityDate": now,
        "LastPlaybackCheckIn": "0001-01-01T00:00:00.0000000Z",
        "DeviceName": s.device_name,
        "DeviceId": s.device_id,
        "ApplicationVersion": s.app_version,
        "IsActive": true,
        "SupportsMediaControl": false,
        "SupportsRemoteControl": false,
        "NowPlayingQueue": [],
        "NowPlayingQueueFullItems": [],
        "HasCustomDeviceName": false,
        "ServerId": server_id,
        "SupportedCommands": []
    })
}

pub fn auth_result(user: Value, s: &Session, server_id: &str) -> Value {
    json!({
        "User": user,
        "SessionInfo": session_info(s, server_id),
        "AccessToken": s.token,
        "ServerId": server_id
    })
}

pub fn branding() -> Value {
    json!({"LoginDisclaimer": "", "CustomCss": "", "SplashscreenEnabled": false})
}

pub fn root_folder(id: &str, server_id: &str) -> Value {
    json!({
        "Name": "Media Folders",
        "ServerId": server_id,
        "Id": id,
        "Etag": id,
        "DateCreated": "2020-01-01T00:00:00.0000000Z",
        "CanDelete": false,
        "CanDownload": false,
        "SortName": "media folders",
        "IsFolder": true,
        "Type": "UserRootFolder",
        "UserData": {"PlaybackPositionTicks": 0, "PlayCount": 0, "IsFavorite": false, "Played": false, "Key": id},
        "ChildCount": 0,
        "DisplayPreferencesId": id,
        "ImageTags": {},
        "BackdropImageTags": [],
        "LocationType": "FileSystem",
        "MediaType": "Unknown"
    })
}

pub fn display_prefs(id: &str, client: &str) -> Value {
    json!({
        "Id": id,
        "SortBy": "SortName",
        "RememberIndexing": false,
        "PrimaryImageHeight": 250,
        "PrimaryImageWidth": 250,
        "CustomPrefs": {},
        "ScrollDirection": "Horizontal",
        "ShowBackdrop": true,
        "RememberSorting": false,
        "SortOrder": "Ascending",
        "ShowSidebar": false,
        "Client": client
    })
}

pub fn query_result(items: Vec<Value>, total: usize, start: usize) -> Value {
    json!({"Items": items, "TotalRecordCount": total, "StartIndex": start})
}
