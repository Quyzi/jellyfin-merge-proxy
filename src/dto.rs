//! Synthesised Jellyfin DTOs for things the proxy answers itself.

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

pub fn user_dto(
    name: &str,
    id: &str,
    server_id: &str,
    server_name: &str,
    configuration: Value,
    policy: Value,
) -> Value {
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
        "Configuration": configuration,
        "Policy": policy
    })
}

pub fn auth_result(user: Value, session_info: Value, token: &str, server_id: &str) -> Value {
    json!({
        "User": user,
        "SessionInfo": session_info,
        "AccessToken": token,
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

pub fn query_result(items: Vec<Value>, total: usize, start: usize) -> Value {
    json!({"Items": items, "TotalRecordCount": total, "StartIndex": start})
}
