#!/usr/bin/env bash
# Complete a fresh Jellyfin server's startup wizard via the API, create a
# second user, add libraries and wait for the scan.
# usage: setup-server.sh <base-url> <admin-user> <admin-pass> "Name|type|/path" ...
set -euo pipefail
BASE=$1; ADMIN=$2; PASS=$3; shift 3
H='Authorization: MediaBrowser Client="jmp-setup", Device="script", DeviceId="jmp-setup", Version="1.0"'
j() { curl -fsS -H "$H" -H 'Content-Type: application/json' "$@"; }

for _ in $(seq 60); do curl -fsS "$BASE/System/Info/Public" >/dev/null 2>&1 && break; sleep 1; done
if [ "$(curl -fsS "$BASE/System/Info/Public" | jq -r .StartupWizardCompleted)" != "true" ]; then
  j -X POST "$BASE/Startup/Configuration" -d '{"UICulture":"en-US","MetadataCountryCode":"US","PreferredMetadataLanguage":"en"}'
  j "$BASE/Startup/User" >/dev/null
  j -X POST "$BASE/Startup/User" -d "{\"Name\":\"$ADMIN\",\"Password\":\"$PASS\"}"
  j -X POST "$BASE/Startup/RemoteAccess" -d '{"EnableRemoteAccess":true,"EnableAutomaticPortMapping":false}'
  j -X POST "$BASE/Startup/Complete"
fi
TOKEN=$(j -X POST "$BASE/Users/AuthenticateByName" -d "{\"Username\":\"$ADMIN\",\"Pw\":\"$PASS\"}" | jq -r .AccessToken)
A="Authorization: MediaBrowser Client=\"jmp-setup\", Device=\"script\", DeviceId=\"jmp-setup\", Version=\"1.0\", Token=\"$TOKEN\""
a() { curl -fsS -H "$A" -H 'Content-Type: application/json' "$@"; }

existing=$(a "$BASE/Library/VirtualFolders" | jq -r '.[].Name')
for spec in "$@"; do
  IFS='|' read -r name type path <<<"$spec"
  if ! grep -qxF "$name" <<<"$existing"; then
    a -X POST "$BASE/Library/VirtualFolders?name=$(jq -rn --arg v "$name" '$v|@uri')&collectionType=$type&refreshLibrary=false" \
      -d "{\"LibraryOptions\":{\"PathInfos\":[{\"Path\":\"$path\"}],\"EnableRealtimeMonitor\":false}}"
  fi
done
a -X POST "$BASE/Library/Refresh"
sleep 3
for _ in $(seq 300); do
  st=$(a "$BASE/ScheduledTasks?isHidden=false" | jq -r '.[] | select(.Key=="RefreshLibrary") | .State')
  [ "$st" = "Idle" ] && break; sleep 2
done
# Full metadata refresh so remote providers (TMDB) fill ProviderIds even for
# items first scanned without network access.
for id in $(a "$BASE/Library/VirtualFolders" | jq -r '.[].ItemId'); do
  a -X POST "$BASE/Items/$id/Refresh?metadataRefreshMode=FullRefresh&imageRefreshMode=FullRefresh&replaceAllMetadata=true&replaceAllImages=false"
done
# wait for the scan task to finish
sleep 3
for _ in $(seq 300); do
  st=$(a "$BASE/ScheduledTasks?isHidden=false" | jq -r '.[] | select(.Key=="RefreshLibrary") | .State')
  [ "$st" = "Idle" ] && break; sleep 2
done
# Non-admin user the proxy logs in as (JMP_UPSTREAM_USER / JMP_UPSTREAM_PASS).
U=${JMP_UPSTREAM_USER:-jmp}; UP=${JMP_UPSTREAM_PASS:-jmppw}
if ! a "$BASE/Users" | jq -e --arg u "$U" '.[] | select(.Name==$u)' >/dev/null; then
  a -X POST "$BASE/Users/New" -d "{\"Name\":\"$U\",\"Password\":\"$UP\"}" >/dev/null
fi
# The test media is 8 s long: allow resume points on short files so
# /UserItems/Resume can be tested (Jellyfin's default minimum is 300 s).
a "$BASE/System/Configuration" | jq '.MinResumeDurationSeconds=1 | .MinResumePct=1' \
  | a -X POST "$BASE/System/Configuration" -d @- >/dev/null
echo "$BASE: libraries $(a "$BASE/Library/VirtualFolders" | jq -c '[.[]|{Name,CollectionType}]')"
echo "$TOKEN"
