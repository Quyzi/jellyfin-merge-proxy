#!/usr/bin/env bash
# Realistic client traffic against a running jellyfin-merge-proxy, so every
# dashboard panel has data: logins (good/bad), browsing, Latest/Resume/NextUp,
# search, images, direct streams with Range, HLS transcodes (segments),
# playstate reports, client disconnects and bad tokens.
#
# usage: scripts/dashboard-lab/traffic.sh [seconds]      (default 900)
# env:   JMP_URL     proxy base URL          (https://localhost:18196)
#        JMP_CACERT  CA for an https proxy   (/srv/jf/pki/ca.pem)
#        JMP_USER/JMP_PASS  proxy login      (family / familypw)
#        JMP_WORKERS browse workers          (3)
set -uo pipefail
DUR=${1:-900}
URL=${JMP_URL:-https://localhost:18196}
CA=${JMP_CACERT:-/srv/jf/pki/ca.pem}
USER_=${JMP_USER:-family}; PASS=${JMP_PASS:-familypw}
WORKERS=${JMP_WORKERS:-3}
END=$((SECONDS + DUR))
CURL=(curl -s --max-time 30)
[[ $URL == https* ]] && CURL+=(--cacert "$CA")

hdr() { echo "MediaBrowser Client=\"Jellyfin Web\", Device=\"$1\", DeviceId=\"lab-$1\", Version=\"10.10.7\"${2:+, Token=\"$2\"}"; }
login() { # login user pass device -> token (empty on failure)
  "${CURL[@]}" -H "Authorization: $(hdr "$3")" -H 'Content-Type: application/json' \
    -X POST "$URL/Users/AuthenticateByName" -d "{\"Username\":\"$1\",\"Pw\":\"$2\"}" | jq -r '.AccessToken // empty' 2>/dev/null
}
get() { "${CURL[@]}" -H "Authorization: $(hdr "$DEV" "$TOK")" "$URL/$1"; }
hit() { "${CURL[@]}" -o /dev/null -H "Authorization: $(hdr "$DEV" "$TOK")" "$URL/$1"; }
post() { "${CURL[@]}" -o /dev/null -H "Authorization: $(hdr "$DEV" "$TOK")" -H 'Content-Type: application/json' -X POST "$URL/$1" -d "${2:-}"; }
pick() { shuf -n1 -e "$@"; }
ids() { jq -r '.Items[]?.Id' 2>/dev/null; }

browse_worker() {
  DEV=browse$1; TOK=$(login "$USER_" "$PASS" "$DEV")
  while [ $SECONDS -lt $END ]; do
    # 1 in 8 iterations: a failed login (bad password / unknown user) and a bad token
    if [ $((RANDOM % 8)) = 0 ]; then
      login "$USER_" wrong-password "$DEV" >/dev/null
      login nobody whatever "$DEV" >/dev/null
      "${CURL[@]}" -o /dev/null -H "Authorization: $(hdr "$DEV" deadbeefdeadbeefdeadbeefdeadbeef)" "$URL/UserViews"
    fi
    [ $((RANDOM % 20)) = 0 ] && TOK=$(login "$USER_" "$PASS" "$DEV")   # re-login now and then
    [ -z "$TOK" ] && { TOK=$(login "$USER_" "$PASS" "$DEV"); sleep 1; continue; }
    local views; views=$(get UserViews | ids)
    for v in $views; do
      local list items; list=$(get "Items?ParentId=$v&SortBy=SortName&Limit=50&Fields=PrimaryImageAspectRatio")
      items=$(ids <<<"$list")
      get "Items/Latest?ParentId=$v&Limit=16" >/dev/null
      for i in $(shuf -n2 -e $items 2>/dev/null); do
        hit "Items/$i"
        hit "Items/$i/Images/Primary?maxHeight=$(pick 200 300 400 600)&quality=90"
        [ $((RANDOM % 2)) = 0 ] && hit "Items/$i/Images/Backdrop?maxWidth=1280"
      done
      # open a series: seasons, then the episodes of one season
      for i in $(jq -r '.Items[]? | select(.Type == "Series") | .Id' <<<"$list" 2>/dev/null | shuf -n1); do
        local s; s=$(get "Shows/$i/Seasons" | ids | shuf -n1)
        hit "Shows/$i/Episodes${s:+?SeasonId=$s}"
      done
    done
    hit "UserItems/Resume?MediaTypes=Video&Limit=12"
    hit "Shows/NextUp?Limit=10"
    hit "Items/Latest?Limit=30"
    hit "Search/Hints?searchTerm=$(pick bunny steel bad office sintel plan night)&Limit=10"
    hit "Items?searchTerm=$(pick bu st ch th)&Recursive=true&IncludeItemTypes=Movie,Series"
    hit "Items?Recursive=true&IncludeItemTypes=Movie&SortBy=$(pick SortName DateCreated Random)&Limit=$(pick 5 10 20)"
    hit "System/Info"; hit "DisplayPreferences/usersettings?client=emby"
    [ $((RANDOM % 4)) = 0 ] && hit "Items/00000000000000000000000000000000"   # stale/unknown id
    sleep "0.$((RANDOM % 9 + 1))"
  done
}

stream_worker() {
  DEV=player; TOK=$(login "$USER_" "$PASS" "$DEV")
  while [ $SECONDS -lt $END ]; do
    [ -z "$TOK" ] && { TOK=$(login "$USER_" "$PASS" "$DEV"); sleep 2; continue; }
    local all; all=$(get "Items?Recursive=true&IncludeItemTypes=Movie,Episode" | ids)
    [ -z "$all" ] && { sleep 2; continue; }
    local it; it=$(pick $all)
    # playstate: start, a few progress reports, stop
    post Sessions/Playing "{\"ItemId\":\"$it\",\"MediaSourceId\":\"$it\",\"PositionTicks\":0}"
    for off in 0 65536 262144 1048576; do   # seeking: Range requests
      "${CURL[@]}" -o /dev/null -H "Range: bytes=$off-$((off + 262143))" "$URL/Videos/$it/stream?static=true&mediaSourceId=$it&api_key=$TOK"
      post Sessions/Playing/Progress "{\"ItemId\":\"$it\",\"MediaSourceId\":\"$it\",\"PositionTicks\":$((off * 10)),\"IsPaused\":false}"
    done
    case $((RANDOM % 4)) in
      0) "${CURL[@]}" -o /dev/null "$URL/Videos/$it/stream.mkv?static=true&api_key=$TOK" ;;          # full download
      1) "${CURL[@]}" -o /dev/null --limit-rate 50k --max-time 2 "$URL/Videos/$it/stream?static=true&api_key=$TOK" ;;  # client gives up
    esac
    post Sessions/Playing/Stopped "{\"ItemId\":\"$it\",\"MediaSourceId\":\"$it\",\"PositionTicks\":$((RANDOM * 1000))}"
    [ $((RANDOM % 3)) = 0 ] && post "UserPlayedItems/$it"
    [ $((RANDOM % 3)) = 0 ] && "${CURL[@]}" -o /dev/null -X DELETE -H "Authorization: $(hdr "$DEV" "$TOK")" "$URL/UserPlayedItems/$it"
    sleep 1
  done
}

hls_worker() {
  DEV=tv; TOK=$(login "$USER_" "$PASS" "$DEV")
  local profile='{"DeviceProfile":{"MaxStreamingBitrate":120000000,"DirectPlayProfiles":[{"Container":"webm","Type":"Video","VideoCodec":"vp8","AudioCodec":"vorbis"}],"TranscodingProfiles":[{"Container":"ts","Type":"Video","VideoCodec":"h264","AudioCodec":"aac","Protocol":"hls","Context":"Streaming","MaxAudioChannels":"2","BreakOnNonKeyFrames":true}]}}'
  while [ $SECONDS -lt $END ]; do
    [ -z "$TOK" ] && { TOK=$(login "$USER_" "$PASS" "$DEV"); sleep 2; continue; }
    local movies; movies=$(get "Items?Recursive=true&IncludeItemTypes=Movie" | ids)
    [ -z "$movies" ] && { sleep 2; continue; }
    local it; it=$(pick $movies)
    local tu; tu=$("${CURL[@]}" -H "Authorization: $(hdr "$DEV" "$TOK")" -H 'Content-Type: application/json' -X POST \
      "$URL/Items/$it/PlaybackInfo?StartTimeTicks=0&IsPlayback=true&AutoOpenLiveStream=true&MaxStreamingBitrate=140000000" \
      -d "$profile" | jq -r '.MediaSources[0].TranscodingUrl // empty' 2>/dev/null)
    if [ -n "$tu" ]; then
      local var seg n=0
      var=$("${CURL[@]}" "$URL$tu" | grep -v '^#' | head -1)
      if [ -n "$var" ]; then
        local pl; pl=$("${CURL[@]}" "$URL/videos/$it/$var")
        for seg in $(grep -v '^#' <<<"$pl" | head -6); do
          "${CURL[@]}" -o /dev/null --max-time 60 "$URL/videos/$it/$seg"; n=$((n + 1))
        done
      fi
      post Sessions/Playing/Stopped "{\"ItemId\":\"$it\",\"MediaSourceId\":\"$it\",\"PositionTicks\":0}"
    fi
    sleep $((RANDOM % 10 + 5))
  done
}

echo "traffic against $URL for ${DUR}s"
for w in $(seq "$WORKERS"); do browse_worker "$w" & done
stream_worker & stream_worker &
hls_worker &
wait
echo "traffic done"
