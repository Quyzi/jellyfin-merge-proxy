#!/usr/bin/env bash
# Black-box test of jellyfin-merge-proxy against three REAL Jellyfin servers,
# acting as a client (login, browse, play, HLS, playstate) under many proxy
# configs. Each check prints PASS/FAIL/SKIP; the exit code is the FAIL count.
#
# Prerequisites (see scripts/real-jellyfin/README.md):
#   scripts/real-jellyfin/make-media.sh   generate the test media
#   scripts/real-jellyfin/start-servers.sh three Jellyfin containers + setup
#   cargo build --release                 (or JMP_IMAGE=... to test an image)
#
# usage: scripts/real-jellyfin-test.sh [scenario ...]
#   scenarios: base merge_off dedupe_off priority server_down api_key
#              https_upstream listener_tls hot_reload cache unknown_empty
#   (default: all)
#
# Environment (defaults match start-servers.sh):
#   JMP_S1/JMP_S2/JMP_S3  upstream base URLs   (http://127.0.0.1:8101..8103)
#   JMP_S3_HTTPS          S3 over HTTPS with a private CA (https://localhost:8923)
#   JMP_PKI               dir with ca.pem, proxy.pem, proxy.key (/srv/jf/pki)
#   JMP_UP_USER/PASS      upstream account the proxy uses (jmp / jmppw)
#   JMP_ADMIN_USER/PASS   upstream admin, for API keys     (admin / adminpw)
#   JMP_PORT              proxy port for the test           (18096)
#   JMP_BIN               proxy binary  (target/release/jellyfin-merge-proxy)
#   JMP_IMAGE             run the proxy from this container image instead
#   JMP_RUNTIME           container runtime for JMP_IMAGE   (podman)
#   JMP_IMAGE_USER        --user for the container (current uid:gid; Unraid: 99:100)
#   JMP_DOWN_CMD/UP_CMD   stop/start server 2 (e.g. "sudo podman stop jf2");
#                         server_down is skipped when unset
#   JMP_WORK              scratch dir (/tmp/jmp-real-test); holds generated
#                         configs with test passwords - not for a repo.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
S1=${JMP_S1:-http://127.0.0.1:8101}
S2=${JMP_S2:-http://127.0.0.1:8102}
S3=${JMP_S3:-http://127.0.0.1:8103}
S3_HTTPS=${JMP_S3_HTTPS:-https://localhost:8923}
PKI=${JMP_PKI:-/srv/jf/pki}
UP_USER=${JMP_UP_USER:-jmp}
UP_PASS=${JMP_UP_PASS:-jmppw}
ADMIN_USER=${JMP_ADMIN_USER:-admin}
ADMIN_PASS=${JMP_ADMIN_PASS:-adminpw}
PORT=${JMP_PORT:-18096}
BIN=${JMP_BIN:-$ROOT/target/release/jellyfin-merge-proxy}
IMAGE=${JMP_IMAGE:-}
RUNTIME=${JMP_RUNTIME:-podman}
IMAGE_USER=${JMP_IMAGE_USER:-$(id -u):$(id -g)}
DOWN_CMD=${JMP_DOWN_CMD:-}
UP_CMD=${JMP_UP_CMD:-}
WORK=${JMP_WORK:-/tmp/jmp-real-test}
# argon2id hash of "kidspw" (the second proxy user).
KIDS_HASH='$argon2id$v=19$m=65536,t=3,p=4$o97eo0VsNK06zJnP5JJkzw$i5evQUwOskKK3JL2uAGfOo1tLO40aMVZZKN+DvZH+QE'
CLIENT='MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="jmp-real-test", Version="10.10.7"'

mkdir -p "$WORK/data" "$WORK/certs" "$WORK/out"
# Paths as the proxy sees them (inside the container, /config = $WORK).
if [ -n "$IMAGE" ]; then CDIR=/config; else CDIR=$WORK; fi
[ -f "$PKI/ca.pem" ] && cp "$PKI"/ca.pem "$PKI"/proxy.pem "$PKI"/proxy.key "$WORK/certs/" 2>/dev/null
chmod -R a+rX "$WORK"; chmod a+rwx "$WORK/data"

PASS=0; FAIL=0; SKIP=0
pass() { PASS=$((PASS + 1)); echo "PASS  [$SCEN] $1"; }
fail() { FAIL=$((FAIL + 1)); echo "FAIL  [$SCEN] $1${2:+ -- $2}"; }
skip() { SKIP=$((SKIP + 1)); echo "SKIP  [$SCEN] $1${2:+ -- $2}"; }
# check "name" "shell condition" ["detail shown on failure"]
check() { if eval "$2"; then pass "$1"; else fail "$1" "${3:-}"; fi; }

# ---------------------------------------------------------------- proxy ----
PURL="http://127.0.0.1:$PORT"; CURL_TLS=()
stop_proxy() {
  if [ -n "$IMAGE" ]; then
    sudo "$RUNTIME" rm -f jmp-real-test >/dev/null 2>&1
  elif [ -f "$WORK/proxy.pid" ]; then
    kill "$(cat "$WORK/proxy.pid")" 2>/dev/null; sleep 0.3; rm -f "$WORK/proxy.pid"
  fi
}
start_proxy() {
  stop_proxy
  if [ -n "$IMAGE" ]; then
    # Arbitrary uid (like Unraid's 99:100): the image must not need 65532.
    sudo "$RUNTIME" run -d --name jmp-real-test --network host --user "$IMAGE_USER" \
      -v "$WORK:/config" -e JMP_CONFIG=/config/config.toml "$IMAGE" >/dev/null
  else
    "$BIN" -c "$WORK/config.toml" >>"$WORK/proxy.log" 2>&1 &
    echo $! >"$WORK/proxy.pid"
  fi
  for _ in $(seq 100); do
    curl -s "${CURL_TLS[@]}" "$PURL/System/Info/Public" >/dev/null 2>&1 && return 0
    sleep 0.2
  done
  echo "proxy did not start"; [ -n "$IMAGE" ] && sudo "$RUNTIME" logs jmp-real-test | tail -20
  tail -20 "$WORK/proxy.log" 2>/dev/null; return 1
}
proxy_log() { if [ -n "$IMAGE" ]; then sudo "$RUNTIME" logs jmp-real-test 2>&1; else cat "$WORK/proxy.log"; fi; }

# server_block name url [extra toml lines...]
server_block() {
  local name=$1 url=$2; shift 2
  printf '\n[[server]]\nname = "%s"\nserver = "%s"\nuser = "%s"\npassword = "%s"\n' "$name" "$url" "$UP_USER" "$UP_PASS"
  for l in "$@"; do printf '%s\n' "$l"; done
}
# write_config "<servers toml>" ["<extra toml>"]; knobs: MERGE DEDUPE UNKNOWN CACHE
write_config() {
  local tmp="$WORK/config.toml.tmp"
  cat >"$tmp" <<EOF
[proxy]
listen = "127.0.0.1:$PORT"
name = "JMP Real Test"
data_dir = "$CDIR/data"
reload_interval_secs = 1
unknown_routes = "${UNKNOWN:-primary}"

[[user]]
name = "family"
password = "familypw"

[[user]]
name = "kids"
password = "$KIDS_HASH"
$1

[merge]
merge_libraries_by_name = ${MERGE:-true}
dedupe = ${DEDUPE:-true}

[cache]
${CACHE:-enabled = true}

[logging]
filter = "info"
format = "compact"

[metrics]
enabled = true

[debug]
enabled = true
${2:-}
EOF
  mv "$tmp" "$WORK/config.toml"
}
three_servers() { server_block s1 "$S1" "$@"; server_block s2 "$S2"; server_block s3 "$S3"; }

# ---------------------------------------------------------------- client ---
TOKEN=""; USERID=""
login() { # login user pass -> sets TOKEN, USERID; returns HTTP status in LOGIN_STATUS
  local body
  body=$(curl -s "${CURL_TLS[@]}" -w '\n%{http_code}' -H "Authorization: $CLIENT" -H 'Content-Type: application/json' \
    -X POST "$PURL/Users/AuthenticateByName" -d "{\"Username\":\"$1\",\"Pw\":\"$2\"}")
  LOGIN_STATUS=$(tail -n1 <<<"$body")
  if [ "$LOGIN_STATUS" = 200 ]; then
    TOKEN=$(head -n -1 <<<"$body" | jq -r .AccessToken); USERID=$(head -n -1 <<<"$body" | jq -r .User.Id)
  fi
}
auth() { echo "Authorization: $CLIENT, Token=\"$TOKEN\""; }
api() { curl -s "${CURL_TLS[@]}" -H "$(auth)" "$PURL/$1"; }
code() { curl -s "${CURL_TLS[@]}" -o /dev/null -w '%{http_code}' "$@"; }
post() { curl -s "${CURL_TLS[@]}" -o /dev/null -w '%{http_code}' -H "$(auth)" -H 'Content-Type: application/json' -X "${3:-POST}" "$PURL/$1" -d "${2:-}"; }
names() { jq -r '[.Items[]?.Name] | sort | join(",")'; }
view_id() { api UserViews | jq -r --arg n "$1" '.Items[] | select(.Name==$n) | .Id' | head -1; }
movies() { api "Items?ParentId=$(view_id Movies)&Recursive=true&IncludeItemTypes=Movie&SortBy=SortName"; }
movie_id() { movies | jq -r --arg n "$1" '.Items[] | select(.Name==$n) | .Id' | head -1; }
width_of() { api "Items/$1?userId=$USERID" | jq -r '[.MediaStreams[]? | select(.Type=="Video") | .Width][0]'; }
wait_for() { # wait_for seconds "condition"
  local end=$((SECONDS + $1))
  while [ $SECONDS -lt $end ]; do eval "$2" && return 0; sleep 1; done; return 1
}
metric() { curl -s "${CURL_TLS[@]}" "$PURL/metrics" | grep -F "$1" | grep -v '^#' | head -1 | awk '{print $NF}'; }

# Upstream, directly (to verify what the proxy did there).
up_token() { # base user pass
  curl -s ${4:-} -H 'Authorization: MediaBrowser Client="jmp-verify", Device="verify", DeviceId="jmp-verify", Version="1"' \
    -H 'Content-Type: application/json' -X POST "$1/Users/AuthenticateByName" -d "{\"Username\":\"$2\",\"Pw\":\"$3\"}" | jq -r .AccessToken
}
up_get() { # base token path
  curl -s -H "Authorization: MediaBrowser Client=\"jmp-verify\", Device=\"verify\", DeviceId=\"jmp-verify\", Version=\"1\", Token=\"$2\"" "$1/$3"
}
up_userdata() { # base name -> UserData of that movie/episode for the upstream user
  local t; t=$(up_token "$1" "$UP_USER" "$UP_PASS")
  up_get "$1" "$t" "Items?Recursive=true&IncludeItemTypes=Movie,Episode&searchTerm=$(jq -rn --arg v "$2" '$v|@uri')&enableUserData=true" \
    | jq -c --arg n "$2" '[.Items[] | select(.Name==$n)][0].UserData'
}
# Clear played/favorite/resume state of the upstream user on every server, so
# Latest/Resume/NextUp start from a known state (Latest hides played items).
reset_userdata() {
  local base t id
  for base in "$S1" "$S2" "$S3"; do
    t=$(up_token "$base" "$UP_USER" "$UP_PASS")
    for id in $(up_get "$base" "$t" "Items?Recursive=true&IncludeItemTypes=Movie,Episode&enableUserData=true" \
        | jq -r '.Items[] | select(.UserData.Played or .UserData.IsFavorite or (.UserData.PlaybackPositionTicks > 0)) | .Id'); do
      for p in UserPlayedItems UserFavoriteItems; do
        curl -s -o /dev/null -X DELETE -H "Authorization: MediaBrowser Client=\"jmp-verify\", Device=\"verify\", DeviceId=\"jmp-verify\", Version=\"1\", Token=\"$t\"" "$base/$p/$id"
      done
    done
  done
}
ffprobe_ok() { ffprobe -v error -show_entries stream=codec_name -of csv=p=0 "$1" 2>/dev/null | grep -q h264; }

# ============================================================== scenarios ===
scenario_base() {
  write_config "$(three_servers)"; start_proxy || { fail "proxy start"; return; }
  local info; info=$(curl -s "$PURL/System/Info/Public")
  check "public system info" '[ "$(jq -r .ServerName <<<"$info")" = "JMP Real Test" ]' "$info"

  # --- login
  login family wrong; check "wrong password -> 401" '[ "$LOGIN_STATUS" = 401 ]' "$LOGIN_STATUS"
  login nobody familypw; check "unknown user -> 401" '[ "$LOGIN_STATUS" = 401 ]' "$LOGIN_STATUS"
  login kids kidspw; check "argon2 [[user]] login" '[ "$LOGIN_STATUS" = 200 ] && [ -n "$TOKEN" ]' "$LOGIN_STATUS"
  login family familypw; check "plaintext [[user]] login" '[ "$LOGIN_STATUS" = 200 ] && [ -n "$TOKEN" ]' "$LOGIN_STATUS"
  check "token: X-Emby-Token" '[ "$(code -H "X-Emby-Token: $TOKEN" "$PURL/UserViews")" = 200 ]'
  check "token: X-MediaBrowser-Token" '[ "$(code -H "X-MediaBrowser-Token: $TOKEN" "$PURL/UserViews")" = 200 ]'
  check "token: Authorization MediaBrowser Token=" '[ "$(code -H "$(auth)" "$PURL/UserViews")" = 200 ]'
  check "token: X-Emby-Authorization" '[ "$(code -H "X-Emby-Authorization: $CLIENT, Token=\"$TOKEN\"" "$PURL/UserViews")" = 200 ]'
  check "token: ?api_key=" '[ "$(code "$PURL/UserViews?api_key=$TOKEN")" = 200 ]'
  check "token: ?ApiKey=" '[ "$(code "$PURL/UserViews?ApiKey=$TOKEN")" = 200 ]'
  check "no token -> 401" '[ "$(code "$PURL/UserViews")" = 401 ]'
  check "bogus token -> 401" '[ "$(code -H "X-Emby-Token: 0123456789abcdef" "$PURL/UserViews")" = 401 ]'

  # --- browsing
  local v; v=$(api UserViews | names)
  check "views merged by name+type" '[ "$v" = "Kids,Movies,Shows,Shows (s3)" ]' "$v"
  local m; m=$(movies | names)
  check "merged Movies deduped (no Fields from client)" '[ "$m" = "Big Buck Bunny,Charade,Night of the Living Dead,Sintel,Tears of Steel" ]' "$m"
  local lat; lat=$(api "Items/Latest?ParentId=$(view_id Movies)&Limit=16&Fields=PrimaryImageAspectRatio" | jq -r '[.[].Name] | sort | join(",")')
  check "Latest (movies) deduped" '[ "$lat" = "Big Buck Bunny,Charade,Night of the Living Dead,Sintel,Tears of Steel" ]' "$lat"
  local lall; lall=$(api "Items/Latest?Limit=30" | jq -r '[.[].Name] | length as $n | (unique | length) == $n')
  check "Latest (all) has no duplicate names" '[ "$lall" = true ]'
  local s; s=$(api "Items?searchTerm=bunny&Recursive=true&IncludeItemTypes=Movie,Series" | names)
  check "Items search deduped" '[ "$s" = "Big Buck Bunny" ]' "$s"
  s=$(api "Search/Hints?searchTerm=bunny&IncludeItemTypes=Movie" | jq -r '[.SearchHints[].Name] | join(",")')
  check "Search/Hints deduped" '[ "$s" = "Big Buck Bunny" ]' "$s"
  local g; g=$(api "Genres?Recursive=true" | jq -r '[.Items[].Name] | length as $n | ($n > 0) and ((unique | length) == $n)')
  check "Genres merged, unique" '[ "$g" = true ]'
  local kids; kids=$(api "Items?ParentId=$(view_id Kids)&Recursive=true&IncludeItemTypes=Movie" | names)
  check "uniquely named library (Kids)" '[ "$kids" = "Elephants Dream" ]' "$kids"
  local s3s; s3s=$(api "Items?ParentId=$(view_id 'Shows (s3)')&Recursive=true&IncludeItemTypes=Movie" | names)
  check "same name, other type -> separate 'Shows (s3)'" '[ "$s3s" = "Plan 9 from Outer Space" ]' "$s3s"

  local bbb; bbb=$(movie_id "Big Buck Bunny")
  local d; d=$(api "Items/$bbb?userId=$USERID")
  check "item details (proxy ids)" '[ "$(jq -r .Id <<<"$d")" = "$bbb" ] && [ "$(jq -r .MediaSources[0].Id <<<"$d")" = "$bbb" ] && [ "$(jq -r .ServerId <<<"$d")" = "$(jq -r .Id <<<"$info")" ]' "$(jq -c '{Id,ServerId}' <<<"$d")"
  check "dedupe winner = s1 by default (320px BBB)" '[ "$(width_of "$bbb")" = 320 ]'
  check "image (Primary)" '[ "$(curl -s -o "$WORK/out/img" -w "%{http_code} %{content_type}" "$PURL/Items/$bbb/Images/Primary?maxHeight=300")" = "200 image/jpeg" ]'

  local shows; shows=$(api "Items?ParentId=$(view_id Shows)&SortBy=SortName" | names)
  check "Shows library: split series listed once" '[ "$shows" = "Breaking Bad,The Office" ]' "$shows"
  local bb; bb=$(api "Items?ParentId=$(view_id Shows)" | jq -r '.Items[] | select(.Name=="Breaking Bad") | .Id')
  local seasons; seasons=$(api "Shows/$bb/Seasons?userId=$USERID")
  check "seasons merged across servers" '[ "$(names <<<"$seasons")" = "Season 1,Season 2" ]' "$(names <<<"$seasons")"
  local eps; eps=$(api "Shows/$bb/Episodes?userId=$USERID" | jq -r '[.Items[] | "S\(.ParentIndexNumber)E\(.IndexNumber)"] | join(",")')
  check "episodes merged across servers" '[ "$eps" = "S1E1,S1E2,S1E3,S2E1" ]' "$eps"
  local se1; se1=$(jq -r '.Items[] | select(.IndexNumber==1) | .Id' <<<"$seasons")
  eps=$(api "Shows/$bb/Episodes?userId=$USERID&seasonId=$se1" | jq -r '[.Items[].IndexNumber] | join(",")')
  check "season 1 episodes from both servers" '[ "$eps" = "1,2,3" ]' "$eps"
  eps=$(api "Items?ParentId=$se1&SortBy=IndexNumber" | jq -r '[.Items[].IndexNumber] | join(",")')
  check "Items?ParentId=<season> merged" '[ "$eps" = "1,2,3" ]' "$eps"

  # --- playback
  local pi; pi=$(curl -s -H "$(auth)" -H 'Content-Type: application/json' -X POST \
    "$PURL/Items/$bbb/PlaybackInfo?userId=$USERID&StartTimeTicks=0&IsPlayback=true&AutoOpenLiveStream=true&MaxStreamingBitrate=140000000" \
    -d '{"DeviceProfile":{"MaxStreamingBitrate":120000000,"DirectPlayProfiles":[{"Container":"webm","Type":"Video","VideoCodec":"vp8","AudioCodec":"vorbis"}],"TranscodingProfiles":[{"Container":"ts","Type":"Video","VideoCodec":"h264","AudioCodec":"aac","Protocol":"hls","Context":"Streaming","MaxAudioChannels":"2","BreakOnNonKeyFrames":true}],"SubtitleProfiles":[{"Format":"vtt","Method":"External"}]}}')
  local tu; tu=$(jq -r '.MediaSources[0].TranscodingUrl // empty' <<<"$pi")
  check "PlaybackInfo: transcoding URL with the proxy token" '[ -n "$tu" ] && grep -q "api_key=$TOKEN" <<<"$tu"' "${tu:0:120}"
  curl -s "$PURL$tu" -o "$WORK/out/master.m3u8"
  check "HLS master.m3u8" 'head -1 "$WORK/out/master.m3u8" | grep -q EXTM3U'
  local var; var=$(grep -v '^#' "$WORK/out/master.m3u8" | head -1)
  curl -s "$PURL/videos/$bbb/$var" -o "$WORK/out/main.m3u8"
  check "HLS variant playlist" 'grep -q EXTINF "$WORK/out/main.m3u8"'
  local seg; seg=$(grep -v '^#' "$WORK/out/main.m3u8" | head -1)
  curl -s "$PURL/videos/$bbb/$seg" -o "$WORK/out/seg0.ts"
  check "HLS segment is real H.264 (ffprobe)" 'ffprobe_ok "$WORK/out/seg0.ts"'
  check "playlists carry no upstream token" '! grep -qE "api_key=($(up_token "$S1" "$UP_USER" "$UP_PASS"))" "$WORK/out/main.m3u8"'
  local sub; sub=$(jq -r '[.MediaSources[0].MediaStreams[] | select(.Type=="Subtitle") | .DeliveryUrl][0] // empty' <<<"$pi")
  check "external subtitle (vtt)" '[ -n "$sub" ] && curl -s "$PURL$sub" | head -1 | grep -q WEBVTT' "$sub"
  local h; h=$(curl -s -D - -o "$WORK/out/range.bin" -H "Range: bytes=0-1023" "$PURL/Videos/$bbb/stream?static=true&mediaSourceId=$bbb&api_key=$TOKEN")
  check "direct stream Range -> 206" 'grep -q "^HTTP/1.1 206" <<<"$h" && grep -qi "^content-range: bytes 0-1023/" <<<"$h" && [ "$(stat -c %s "$WORK/out/range.bin")" = 1024 ]' "$(head -1 <<<"$h")"
  curl -s -o "$WORK/out/full.mkv" "$PURL/Videos/$bbb/stream.mkv?static=true&api_key=$TOKEN"
  check "direct stream full file (ffprobe)" 'ffprobe_ok "$WORK/out/full.mkv"'
  local s2ep; s2ep=$(api "Shows/$bb/Episodes?userId=$USERID" | jq -r '.Items[] | select(.ParentIndexNumber==2) | .Id')
  curl -s -o "$WORK/out/s2ep.mkv" "$PURL/Videos/$s2ep/stream?static=true&api_key=$TOKEN"
  check "episode only on s2 streams" 'ffprobe_ok "$WORK/out/s2ep.mkv"'

  # --- user data routed to the owning server
  local tos; tos=$(movie_id "Tears of Steel")
  post "UserPlayedItems/$tos" "" DELETE >/dev/null
  for e in Playing Playing/Progress Playing/Stopped; do
    post "Sessions/$e" "{\"ItemId\":\"$tos\",\"MediaSourceId\":\"$tos\",\"PositionTicks\":30000000}" >/dev/null
  done
  local ud; ud=$(up_userdata "$S2" "Tears of Steel")
  check "playstate stored on s2 (position 3 s)" '[ "$(jq -r .PlaybackPositionTicks <<<"$ud")" = 30000000 ]' "$ud"
  local res; res=$(api "UserItems/Resume?MediaTypes=Video&Limit=12" | names)
  check "Resume lists it" 'grep -q "Tears of Steel" <<<"$res"' "$res"
  local ch; ch=$(movie_id Charade)
  check "mark played (s3 item)" '[ "$(post "UserPlayedItems/$ch")" = 200 ] && [ "$(up_userdata "$S3" Charade | jq -r .Played)" = true ]'
  check "unmark played" '[ "$(post "UserPlayedItems/$ch" "" DELETE)" = 200 ] && [ "$(up_userdata "$S3" Charade | jq -r .Played)" = false ]'
  local sin; sin=$(movie_id Sintel)
  check "favorite (s1 copy, the dedupe winner)" '[ "$(post "UserFavoriteItems/$sin")" = 200 ] && [ "$(up_userdata "$S1" Sintel | jq -r .IsFavorite)" = true ]'
  check "unfavorite via legacy /Users/{id}/FavoriteItems" '[ "$(post "Users/$USERID/FavoriteItems/$sin" "" DELETE)" = 200 ] && [ "$(up_userdata "$S1" Sintel | jq -r .IsFavorite)" = false ]'
  local e1; e1=$(api "Shows/$bb/Episodes?userId=$USERID" | jq -r '.Items[] | select(.ParentIndexNumber==1 and .IndexNumber==1) | .Id')
  post "UserPlayedItems/$e1" >/dev/null
  local nu; nu=$(api "Shows/NextUp?Limit=10" | names)
  check "NextUp after S01E01 played" 'grep -q "Cat" <<<"$nu"' "$nu"
  post "UserPlayedItems/$e1" "" DELETE >/dev/null

  # --- endpoints
  check "/metrics has upstream gauges" '[ "$(metric "jmp_upstream_up{server=\"s3\"}")" = 1 ]'
  check "debug endpoint needs auth" '[ "$(code "$PURL/jmp/debug")" = 401 ]'
  local dbg; dbg=$(api jmp/debug)
  check "debug endpoint redacts" 'jq -e .upstreams >/dev/null <<<"$dbg" && ! grep -qF "$TOKEN" <<<"$dbg" && ! grep -qF "$UP_PASS" <<<"$dbg" && ! grep -qF familypw <<<"$dbg"'
  check "logs contain no secrets" '! proxy_log | grep -qF -e "$TOKEN" -e "$UP_PASS" -e familypw -e kidspw'
  check "primary-passthrough route (Localization)" '[ "$(api Localization/Countries | jq length)" -gt 100 ]'
  check "unhandled route -> primary (upstream status kept)" '[ "$(code -H "$(auth)" "$PURL/Library/VirtualFolders")" = 403 ]'
  check "/web/ index through the proxy" '[ "$(curl -s -o "$WORK/out/web.html" -w "%{http_code}" "$PURL/web/")" = 200 ] && grep -q "<html" "$WORK/out/web.html"'
  local mainjs; mainjs=$(grep -oE 'main\.jellyfin\.bundle\.js[^"]*' "$WORK/out/web.html" | head -1)
  check "/web main JS identical to upstream" '[ -n "$mainjs" ] && cmp -s <(curl -s "$PURL/web/$mainjs") <(curl -s "$S1/web/$mainjs")'
}

scenario_merge_off() {
  MERGE=false write_config "$(three_servers)"; start_proxy || { fail "proxy start"; return; }
  login family familypw
  local v; v=$(api UserViews | names)
  check "merge_libraries_by_name=false: 7 labelled libraries" '[ "$v" = "Kids,Movies (s1),Movies (s2),Movies (s3),Shows (s1),Shows (s2),Shows (s3)" ]' "$v"
}

scenario_dedupe_off() {
  DEDUPE=false write_config "$(three_servers)"; start_proxy || { fail "proxy start"; return; }
  login family familypw
  local n; n=$(movies | jq '.Items | length')
  check "dedupe=false: all 8 movie copies" '[ "$n" = 8 ]' "$n"
  local sh; sh=$(api "Items?ParentId=$(view_id Shows)&SortBy=SortName" | names)
  check "dedupe=false: both Breaking Bad copies" '[ "$sh" = "Breaking Bad,Breaking Bad,The Office" ]' "$sh"
}

scenario_priority() {
  write_config "$(server_block s1 "$S1" "priority = 5"; server_block s2 "$S2" "priority = 0"; server_block s3 "$S3" "priority = 9")"
  start_proxy || { fail "proxy start"; return; }
  login family familypw
  check "priority: s2 wins BBB (640px copy)" '[ "$(width_of "$(movie_id "Big Buck Bunny")")" = 640 ]'
  local m; m=$(movies | names)
  check "priority: still 5 movies" '[ "$m" = "Big Buck Bunny,Charade,Night of the Living Dead,Sintel,Tears of Steel" ]' "$m"
}

scenario_server_down() {
  if [ -z "$DOWN_CMD" ] || [ -z "$UP_CMD" ]; then skip "server down" "JMP_DOWN_CMD/JMP_UP_CMD not set"; return; fi
  write_config "$(three_servers)"; start_proxy || { fail "proxy start"; return; }
  login family familypw
  eval "$DOWN_CMD" >/dev/null
  wait_for 30 '! curl -s -m 2 "$S2/System/Ping" >/dev/null'
  local m; m=$(movies | names)
  check "s2 down: partial movie list" '[ "$m" = "Big Buck Bunny,Charade,Night of the Living Dead,Sintel" ]' "$m"
  check "s2 down: views still served" '[ -n "$(view_id Movies)" ]'
  check "s2 down: jmp_upstream_up{s2}=0" '[ "$(metric "jmp_upstream_up{server=\"s2\"}")" = 0 ]'
  check "s2 down: fan-out partial counted" '[ -n "$(metric "jmp_fanout_partial_total")" ]'
  eval "$UP_CMD" >/dev/null
  wait_for 120 'curl -s -m 2 "$S2/System/Info/Public" | grep -q StartupWizardCompleted'
  check "s2 back: Tears of Steel returns" 'wait_for 60 "movies | names | grep -q \"Tears of Steel\""'
  check "s2 back: jmp_upstream_up{s2}=1" 'wait_for 30 "[ \"\$(metric \"jmp_upstream_up{server=\\\"s2\\\"}\")\" = 1 ]"'
}

scenario_api_key() {
  local at; at=$(up_token "$S1" "$ADMIN_USER" "$ADMIN_PASS")
  curl -s -o /dev/null -X POST -H "Authorization: MediaBrowser Client=\"jmp-verify\", Device=\"verify\", DeviceId=\"jmp-verify\", Version=\"1\", Token=\"$at\"" "$S1/Auth/Keys?app=jmp-real-test"
  local key; key=$(up_get "$S1" "$at" "Auth/Keys" | jq -r '[.Items[] | select(.AppName=="jmp-real-test")][0].AccessToken')
  if [ -z "$key" ] || [ "$key" = null ]; then fail "create API key on s1"; return; fi
  write_config "$(printf '\n[[server]]\nname = "s1"\nserver = "%s"\napi_key = "%s"\nuser = "%s"\n' "$S1" "$key" "$UP_USER"; server_block s2 "$S2"; server_block s3 "$S3")"
  start_proxy || { fail "proxy start"; return; }
  login family familypw
  local m; m=$(movies | names)
  check "api_key upstream: s1-only movie listed" 'grep -q "Night of the Living Dead" <<<"$m"' "$m"
  local n; n=$(movie_id "Night of the Living Dead")
  check "api_key upstream: stream" '[ "$(code -H "Range: bytes=0-99" "$PURL/Videos/$n/stream?static=true&api_key=$TOKEN")" = 206 ]'
  check "api_key upstream: favorite lands on user $UP_USER" '[ "$(post "UserFavoriteItems/$n")" = 200 ] && [ "$(up_userdata "$S1" "Night of the Living Dead" | jq -r .IsFavorite)" = true ]'
  post "UserFavoriteItems/$n" "" DELETE >/dev/null
  check "api key not in logs" '! proxy_log | grep -qF "$key"'
  curl -s -o /dev/null -X DELETE -H "Authorization: MediaBrowser Client=\"jmp-verify\", Device=\"verify\", DeviceId=\"jmp-verify\", Version=\"1\", Token=\"$at\"" "$S1/Auth/Keys/$key"
}

scenario_https_upstream() {
  if ! curl -s --cacert "$WORK/certs/ca.pem" "$S3_HTTPS/System/Ping" >/dev/null; then skip "https upstream" "$S3_HTTPS not reachable with $PKI/ca.pem"; return; fi
  write_config "$(server_block s1 "$S1"; server_block s2 "$S2"; server_block s3 "$S3_HTTPS" "[server.tls]" "ca_path = \"$CDIR/certs/ca.pem\"")"
  start_proxy || { fail "proxy start"; return; }
  login family familypw
  check "https upstream + ca_path: s3 content" 'movies | names | grep -q Charade'
  check "https upstream: stream with Range" '[ "$(code -H "Range: bytes=0-99" "$PURL/Videos/$(movie_id Charade)/stream?static=true&api_key=$TOKEN")" = 206 ]'
  write_config "$(server_block s1 "$S1"; server_block s2 "$S2"; server_block s3 "$S3_HTTPS")"
  start_proxy; login family familypw
  check "https upstream without the CA: s3 skipped, rest served" '! movies | names | grep -q Charade && movies | names | grep -q Sintel'
  check "https upstream without the CA: tls error counted" '[ -n "$(metric "jmp_upstream_errors_total{kind=\"tls\",server=\"s3\"}")$(metric "jmp_upstream_errors_total{server=\"s3\",kind=\"tls\"}")" ]'
  write_config "$(server_block s1 "$S1"; server_block s2 "$S2"; server_block s3 "$S3_HTTPS" "[server.tls]" "insecure_skip_verify = true")"
  start_proxy; login family familypw
  check "https upstream + insecure_skip_verify" 'movies | names | grep -q Charade'
  check "insecure_skip_verify warns loudly" 'proxy_log | grep -qi "insecure"'
}

scenario_listener_tls() {
  [ -f "$WORK/certs/proxy.pem" ] || { skip "listener TLS" "no $PKI/proxy.pem"; return; }
  write_config "$(three_servers)" "$(printf '[tls]\ncert_path = "%s"\nkey_path = "%s"\n' "$CDIR/certs/proxy.pem" "$CDIR/certs/proxy.key")"
  PURL="https://localhost:$PORT"; CURL_TLS=(--cacert "$WORK/certs/ca.pem")
  start_proxy || { fail "proxy start (TLS)"; PURL="http://127.0.0.1:$PORT"; CURL_TLS=(); return; }
  login family familypw
  check "TLS listener: login over https" '[ -n "$TOKEN" ] && [ "$LOGIN_STATUS" = 200 ]'
  check "TLS listener: views" '[ -n "$(view_id Movies)" ]'
  check "TLS listener: stream Range" '[ "$(code -H "Range: bytes=0-99" "$PURL/Videos/$(movie_id Sintel)/stream?static=true&api_key=$TOKEN")" = 206 ]'
  check "TLS listener: plain http refused" '[ "$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$PORT/System/Ping")" != 200 ]'
  PURL="http://127.0.0.1:$PORT"; CURL_TLS=()
}

scenario_hot_reload() {
  write_config "$(server_block s1 "$S1"; server_block s2 "$S2")"; start_proxy || { fail "proxy start"; return; }
  login family familypw
  check "reload: start without s3" '! api UserViews | names | grep -q "Shows (s3)"'
  write_config "$(three_servers)"
  check "reload: s3 added live" 'wait_for 10 "api UserViews | names | grep -q \"Shows (s3)\""'
  check "reload: s3 movies merged" 'wait_for 10 "movies | names | grep -q Charade"'
  local ch; ch=$(movie_id Charade)
  check "reload: default priority, s1 wins BBB" '[ "$(width_of "$(movie_id "Big Buck Bunny")")" = 320 ]'
  write_config "$(server_block s1 "$S1" "priority = 5"; server_block s2 "$S2" "priority = 0"; server_block s3 "$S3")"
  check "reload: priority change, s2 wins BBB" 'wait_for 10 "[ \"\$(width_of \"\$(movie_id \"Big Buck Bunny\")\")\" = 640 ]"'
  write_config "$(server_block s1 "$S1"; server_block s2 "$S2")"
  check "reload: s3 removed from views" 'wait_for 10 "! api UserViews | names | grep -q \"Shows (s3)\""'
  check "reload: removed server's item -> 410" '[ "$(code -H "$(auth)" "$PURL/Items/$ch?userId=$USERID")" = 410 ]'
  check "reload: session survives" '[ "$(code -H "$(auth)" "$PURL/UserViews")" = 200 ]'
}

scenario_cache() {
  CACHE='enabled = false' write_config "$(three_servers)"; start_proxy || { fail "proxy start"; return; }
  login family familypw; api UserViews >/dev/null; api UserViews >/dev/null
  check "cache off: views served" '[ -n "$(view_id Movies)" ]'
  check "cache off: no cache hits" '[ -z "$(metric "jmp_cache_requests_total{cache=\"views\",outcome=\"hit\"}")" ]'
  rm -f "$WORK/data/catalog.json"
  CACHE="enabled = true
ttl_secs = 600
persist_path = \"$CDIR/data/catalog.json\"" write_config "$(three_servers)"
  start_proxy; login family familypw; api UserViews >/dev/null; api UserViews >/dev/null
  check "cache on: hits counted" '[ -n "$(curl -s "$PURL/metrics" | grep "^jmp_cache_requests_total" | grep "outcome=\"hit\"")" ]'
  stop_proxy
  check "cache persisted to disk" '[ -s "$WORK/data/catalog.json" ]'
  start_proxy; login family familypw; api UserViews >/dev/null
  check "cache warm after restart (hit on first views)" '[ -n "$(curl -s "$PURL/metrics" | grep "^jmp_cache_requests_total" | grep "outcome=\"hit\"")" ]'
}

scenario_unknown_empty() {
  UNKNOWN=empty write_config "$(three_servers)"; start_proxy || { fail "proxy start"; return; }
  login family familypw
  local r; r=$(api Library/VirtualFolders)
  check "unknown_routes=empty: GET -> empty result" '[ "$(jq -c "[.Items, .TotalRecordCount]" <<<"$r")" = "[[],0]" ]' "$r"
  check "unknown_routes=empty: POST -> 204" '[ "$(post Library/VirtualFolders/Refresh)" = 204 ]'
  check "unknown_routes=empty: compat routes still forwarded" '[ "$(api Localization/Countries | jq length)" -gt 100 ]'
}

ALL=(base merge_off dedupe_off priority server_down api_key https_upstream listener_tls hot_reload cache unknown_empty)
[ $# -gt 0 ] && ALL=("$@")
trap stop_proxy EXIT
reset_userdata
echo "proxy: ${IMAGE:-$BIN}   upstreams: $S1 $S2 $S3   port: $PORT"
for SCEN in "${ALL[@]}"; do
  rm -f "$WORK/data/"*.tsv "$WORK/data/sessions.json"
  : >"$WORK/proxy.log"
  "scenario_$SCEN"
  stop_proxy
done
SCEN=total
echo "== $PASS passed, $FAIL failed, $SKIP skipped"
exit "$FAIL"
