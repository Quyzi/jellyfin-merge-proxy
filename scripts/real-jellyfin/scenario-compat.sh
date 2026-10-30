# Client compatibility against a real Jellyfin, for
# scripts/real-jellyfin-test.sh (sourced; uses its helpers).
#
# compat_single: the proxy in front of s1 alone must answer the requests
# real clients send (jellyfin-web 10.x, Android TV, Swiftfin, Findroid)
# like s1 itself does: same items in the same order, same totals, every key
# s1 sends (with the same JSON type), same UserData, same image tags (and
# the image behind the tag). Ids differ (proxy ids) and are mapped by name.
# compat_merged: the same requests across all three servers give the union.

D_TOKEN=""; D_USER=""
direct_login() { # s1 as the upstream account, like the proxy does
  local r; r=$(curl -s -H 'Authorization: MediaBrowser Client="jmp-verify", Device="verify", DeviceId="jmp-compat", Version="1"' \
    -H 'Content-Type: application/json' -X POST "$S1/Users/AuthenticateByName" -d "{\"Username\":\"$UP_USER\",\"Pw\":\"$UP_PASS\"}")
  D_TOKEN=$(jq -r .AccessToken <<<"$r"); D_USER=$(jq -r .User.Id <<<"$r")
}
dapi() { curl -s -H "Authorization: MediaBrowser Client=\"jmp-verify\", Device=\"verify\", DeviceId=\"jmp-compat\", Version=\"1\", Token=\"$D_TOKEN\"" "$S1/$1"; }
dpost() { curl -s -o /dev/null -w '%{http_code}' -H "Authorization: MediaBrowser Client=\"jmp-verify\", Device=\"verify\", DeviceId=\"jmp-compat\", Version=\"1\", Token=\"$D_TOKEN\"" -H 'Content-Type: application/json' -X "${3:-POST}" "$S1/$1" -d "${2:-}"; }
d_view() { dapi "UserViews?userId=$D_USER" | jq -r --arg n "$1" '.Items[] | select(.Name==$n) | .Id'; }
d_id() { dapi "Items?Recursive=true&IncludeItemTypes=${2:-Movie,Series}&searchTerm=$(jq -rn --arg v "$1" '$v|@uri')" | jq -r --arg n "$1" '[.Items[] | select(.Name==$n)][0].Id'; }
p_id() { api "Items?Recursive=true&IncludeItemTypes=${2:-Movie,Series}&searchTerm=$(jq -rn --arg v "$1" '$v|@uri')" | jq -r --arg n "$1" '[.Items[] | select(.Name==$n)][0].Id'; }

# Keys whose values are ids, server ids, paths, or timestamps the proxy
# legitimately changes; they are compared by presence and type only.
COMPAT_LOOSE='["Id","ItemId","ServerId","ParentId","SeriesId","SeasonId","ParentBackdropItemId","ParentThumbItemId","ParentLogoItemId","ParentPrimaryImageItemId","DisplayPreferencesId","Key","Etag","LastPlayedDate","PlaylistItemId","ChannelId","AlbumId","PrimaryImageItemId","PrimaryImageTag","ThumbImageItemId","BackdropImageItemId","DateLastMediaAdded"]'
# Compare one proxy response with s1's (stdin: [proxy, direct]); prints
# the differences (empty = compatible).
compat_diff() {
  jq -r --argjson loose "$COMPAT_LOOSE" '
    def items: if type == "array" then . elif has("Items") then .Items elif has("SearchHints") then .SearchHints else [.] end;
    def lbl: "\(.Name // "?")/\(.Type // "")";
    def tname: if type == "number" then "number" else type end;
    def walk2($p; $d; $path):
      if ($d | type) == "object" then
        if ($p | type) != "object" then ["\($path): object vs \($p | type)"] else
        [ $d | keys[] as $k
          | if ($p | has($k) | not) then "\($path).\($k) missing"
            elif ($loose | index($k)) then
              (if ($p[$k] | tname) != ($d[$k] | tname) and $d[$k] != null then "\($path).\($k) type \($p[$k] | tname) vs \($d[$k] | tname)" else empty end)
            elif ($k == "ImageTags" or $k == "ImageBlurHashes") then
              (if ($p[$k] | keys) != ($d[$k] | keys) then "\($path).\($k) kinds \($p[$k] | keys) vs \($d[$k] | keys)" else empty end)
            elif ($k == "BackdropImageTags" or $k == "ParentBackdropImageTags") then
              (if ($p[$k] | length) != ($d[$k] | length) then "\($path).\($k) count" else empty end)
            elif ($k | test("ImageTag$")) then empty
            else walk2($p[$k]; $d[$k]; "\($path).\($k)")[] end ]
        end
      elif ($d | type) == "array" then
        if ($p | type) != "array" then ["\($path): array vs \($p | type)"]
        elif ($p | length) != ($d | length) then ["\($path): \($p | length) vs \($d | length) entries"]
        else [ range(0; $d | length) as $i | walk2($p[$i]; $d[$i]; "\($path)[\($i)]")[] ] end
      elif ($p | tname) != ($d | tname) then ["\($path): \($p | tojson) vs \($d | tojson)"]
      elif ($d | type) == "number" and ((($p - $d) | fabs) <= (1e-9 * ([($d | fabs), 1] | max))) then []
      elif $p != $d then ["\($path): \($p | tojson) vs \($d | tojson)"]
      else [] end;
    .[0] as $p | .[1] as $d
    | ( [$p | items[] | lbl] ) as $pl | ( [$d | items[] | lbl] ) as $dl
    | if $pl != $dl then "items: \($pl | join(",")) vs \($dl | join(","))"
      else
        ( if ($d | type) == "object" and ($d | has("TotalRecordCount")) and $p.TotalRecordCount != $d.TotalRecordCount
          then "TotalRecordCount \($p.TotalRecordCount) vs \($d.TotalRecordCount)" else empty end ),
        ( [range(0; $d | items | length) as $i | walk2($p | items[$i]; $d | items[$i]; ($d | items[$i] | lbl))[]] | .[:6][] )
      end'
}
# compat "<label>" "<proxy path>" "<direct path>"
compat() {
  local p d diff; p=$(api "$2"); d=$(dapi "$3")
  if ! jq -e . >/dev/null 2>&1 <<<"$d" || [ -z "$d" ]; then skip "$1 (s1 gave no JSON)"; return; fi
  diff=$(jq -s . <<<"$p
$d" | compat_diff 2>&1)
  check "compat: $1" '[ -z "$diff" ]' "$(head -c 600 <<<"$diff")"
}

# Run a client request on both sides: {U} user id, {MOV}/{SHOW} library,
# {ITEM} Big Buck Bunny, {SER} the s1 series, {GENRE}/{PERSON} ids.
both() { # label template
  local p=$2 d=$2 k
  for k in U MOV SHOW ITEM SER PERSON; do
    local pv dv; pv=$(eval echo "\$P_$k"); dv=$(eval echo "\$D_$k")
    p=${p//\{$k\}/$pv}; d=${d//\{$k\}/$dv}
  done
  compat "$1" "$p" "$d"
}
LIB_FIELDS="Fields=PrimaryImageAspectRatio,MediaSourceCount&ImageTypeLimit=1&EnableImageTypes=Primary,Backdrop,Banner,Thumb"
MOVIES="Users/{U}/Items?IncludeItemTypes=Movie&Recursive=true&$LIB_FIELDS&ParentId={MOV}"

scenario_compat_single() {
  reset_userdata
  rm -f "$WORK/data/"*.db* 2>/dev/null || true
  write_config "$(server_block s1 "$S1")"; start_proxy || { fail "proxy start"; return; }
  login family familypw; direct_login
  check "index warm" 'wait_for 120 index_warm'
  check "upstream state imported" 'wait_for 60 seeded'
  P_U=$USERID D_U=$D_USER
  P_MOV=$(view_id Movies) D_MOV=$(d_view Movies) P_SHOW=$(view_id Shows) D_SHOW=$(d_view Shows)
  P_ITEM=$(p_id "Big Buck Bunny") D_ITEM=$(d_id "Big Buck Bunny")
  P_SER=$(p_id "The Office" Series) D_SER=$(d_id "The Office" Series)
  D_PERSON=$(dapi "Users/$D_USER/Items/$D_ITEM" | jq -r '[.People[]? | select(.Type=="Actor")][0].Id // empty')
  P_PERSON=$(api "Users/$USERID/Items/$P_ITEM" | jq -r '[.People[]? | select(.Type=="Actor")][0].Id // empty')

  # Library browsing (jellyfin-web movies/tv tabs, Android TV grid).
  both "UserViews" "UserViews?userId={U}"
  both "movies, SortName" "$MOVIES&SortBy=SortName,ProductionYear&SortOrder=Ascending&StartIndex=0&Limit=100"
  both "movies, no total" "$MOVIES&SortBy=SortName&EnableTotalRecordCount=false&Limit=3"
  both "movies, page 2" "$MOVIES&SortBy=SortName&SortOrder=Ascending&StartIndex=2&Limit=2"
  local s
  for s in DateCreated,SortName CommunityRating,SortName PremiereDate,SortName ProductionYear,SortName Runtime,SortName OfficialRating,SortName CriticRating,SortName; do
    both "movies, SortBy=$s desc" "$MOVIES&SortBy=$s&SortOrder=Descending"
  done
  both "series" "Users/{U}/Items?SortBy=SortName&SortOrder=Ascending&IncludeItemTypes=Series&Recursive=true&Fields=PrimaryImageAspectRatio&ParentId={SHOW}"
  both "episodes" "Users/{U}/Items?IncludeItemTypes=Episode&Recursive=true&SortBy=SeriesSortName,ParentIndexNumber,IndexNumber&Fields=Overview,PrimaryImageAspectRatio"
  both "seasons" "Shows/{SER}/Seasons?userId={U}&Fields=ItemCounts,PrimaryImageAspectRatio"
  both "show episodes" "Shows/{SER}/Episodes?userId={U}&Fields=ItemCounts,PrimaryImageAspectRatio,Overview"

  # Filters (jellyfin-web filter dialog / Swiftfin filter bar).
  local g; g=$(dapi "Items/Filters?UserId=$D_USER&ParentId=$D_MOV&IncludeItemTypes=Movie" | jq -r '.Genres[0]')
  both "movies, Genres=$g" "$MOVIES&SortBy=SortName&Genres=$(jq -rn --arg v "$g" '$v|@uri')"
  both "movies, Years=2008" "$MOVIES&SortBy=SortName&Years=2008"
  both "movies, OfficialRatings=G|PG" "$MOVIES&SortBy=SortName&OfficialRatings=G|PG"
  [ -n "$D_PERSON" ] && both "movies, PersonIds" "$MOVIES&SortBy=SortName&PersonIds={PERSON}"
  both "Items/Filters" "Items/Filters?UserId={U}&ParentId={MOV}&IncludeItemTypes=Movie"
  both "Items/Filters2" "Items/Filters2?UserId={U}&ParentId={MOV}&IncludeItemTypes=Movie"
  both "Genres" "Genres?SortBy=SortName&SortOrder=Ascending&IncludeItemTypes=Movie&Recursive=true&EnableTotalRecordCount=false&ParentId={MOV}&userId={U}"
  both "Studios" "Studios?SortBy=SortName&SortOrder=Ascending&IncludeItemTypes=Series&Recursive=true&Fields=PrimaryImageAspectRatio&StartIndex=0&ParentId={SHOW}&userId={U}"
  both "Genres (all, user data)" "Genres?SortBy=SortName&Recursive=true&EnableUserData=true&userId={U}"
  both "Studios (no user data)" "Studios?SortBy=SortName&Recursive=true&EnableUserData=false&userId={U}"
  both "Persons" "Persons?SortBy=SortName&SortOrder=Ascending&PersonTypes=Actor&Recursive=true&Limit=20&userId={U}"
  both "Years" "Years?SortBy=SortName&SortOrder=Descending&IncludeItemTypes=Movie&Recursive=true&ParentId={MOV}&userId={U}"

  # Search (jellyfin-web 10.9+ searches Items; Android TV uses hints).
  both "search Items" "Items?searchTerm=Big&Recursive=true&IncludeItemTypes=Movie,Series,Episode&Limit=24&Fields=PrimaryImageAspectRatio&userId={U}"
  both "Search/Hints" "Search/Hints?searchTerm=Big&Limit=20&userId={U}"

  # Item details and images (tags must fetch the same picture).
  both "item details" "Users/{U}/Items/{ITEM}"
  local pt dt; pt=$(api "Users/$USERID/Items/$P_ITEM" | jq -r .ImageTags.Primary); dt=$(dapi "Users/$D_USER/Items/$D_ITEM" | jq -r .ImageTags.Primary)
  check "compat: image tag fetches the same image" '[ "$(api "Items/$P_ITEM/Images/Primary?tag=$pt&maxWidth=200" | sha256sum)" = "$(dapi "Items/$D_ITEM/Images/Primary?tag=$dt&maxWidth=200" | sha256sum)" ]'

  # User data written through the proxy, read back on both sides.
  check "compat: mark played 200" '[ "$(post "UserPlayedItems/$P_ITEM")" = 200 ]'
  post "UserFavoriteItems/$P_ITEM" >/dev/null
  both "UserData" "UserItems/{ITEM}/UserData?userId={U}"
  both "movies, Filters=IsPlayed" "$MOVIES&SortBy=SortName&Filters=IsPlayed"
  both "movies, Filters=IsUnplayed" "$MOVIES&SortBy=SortName&Filters=IsUnplayed"
  both "movies, IsFavorite=true" "$MOVIES&SortBy=SortName&IsFavorite=true"
  local sin; sin=$(p_id Sintel)
  for e in Playing Playing/Progress Playing/Stopped; do
    post "Sessions/$e" "{\"ItemId\":\"$sin\",\"MediaSourceId\":\"$sin\",\"PositionTicks\":30000000}" >/dev/null
  done
  both "Resume" "UserItems/Resume?Limit=12&Recursive=true&Fields=PrimaryImageAspectRatio&ImageTypeLimit=1&EnableImageTypes=Primary,Backdrop,Thumb&EnableTotalRecordCount=false&MediaTypes=Video&userId={U}"
  local ep; ep=$(api "Shows/$P_SER/Episodes?userId=$USERID" | jq -r '.Items[0].Id')
  post "UserPlayedItems/$ep" >/dev/null
  both "NextUp" "Shows/NextUp?Limit=24&Fields=PrimaryImageAspectRatio,DateCreated,MediaSourceCount&UserId={U}&ImageTypeLimit=1&EnableImageTypes=Primary,Backdrop,Banner,Thumb&EnableTotalRecordCount=false&DisableFirstEpisode=false&EnableResumable=false&EnableRewatching=false"
  both "Latest (movies)" "Items/Latest?Limit=16&Fields=PrimaryImageAspectRatio&ImageTypeLimit=1&EnableImageTypes=Primary,Backdrop,Thumb&ParentId={MOV}&userId={U}"
  both "Latest (shows)" "Items/Latest?Limit=16&Fields=PrimaryImageAspectRatio&ImageTypeLimit=1&EnableImageTypes=Primary,Backdrop,Thumb&ParentId={SHOW}&userId={U}"
  both "Items/Counts" "Items/Counts?userId={U}"
  reset_userdata
}

# ---------------------------------------------------------------- merged ---
# Each direct server's answer to a path (as the upstream account): one JSON
# per server, one per line.
each_server() {
  local base t
  for base in "$S1" "$S2" "$S3"; do
    t=$(up_token "$base" "$UP_USER" "$UP_PASS")
    up_get "$base" "$t" "$1" | jq -c .
  done
}
# Sorted unique values of a jq expression over every server's answer.
union_of() { each_server "$1" | jq -r "$2" | sort -u; }
uniq_names() { jq -r "$1" | sort; }

scenario_compat_merged() {
  reset_userdata
  rm -f "$WORK/data/"*.db* 2>/dev/null || true
  write_config "$(three_servers)"; start_proxy || { fail "proxy start"; return; }
  login family familypw
  check "index warm" 'wait_for 120 index_warm'
  local want got

  want=$(union_of "Years?Recursive=true" '.Items[].Name')
  got=$(api "Years?Recursive=true&SortBy=SortName&userId=$USERID" | uniq_names '.Items[].Name')
  check "merged: Years is the union of every server's years, once each" '[ "$got" = "$want" ] && [ -z "$(uniq -d <<<"$got")" ]' "$got | $want"
  got=$(api "Years?Recursive=true&SortBy=SortName&SortOrder=Descending&userId=$USERID" | jq -r '[.Items[].Name] | join(",")')
  check "merged: Years sort descending" '[ "$got" = "$(sort -r <<<"$want" | paste -sd,)" ]' "$got"

  want=$(union_of "Genres?Recursive=true" '.Items[].Name')
  got=$(api "Genres?Recursive=true&userId=$USERID" | uniq_names '.Items[].Name')
  check "merged: Genres is the union, once each" '[ "$got" = "$want" ]' "$got | $want"
  check "merged: Genres carry upstream images" '[ "$(api "Genres?Recursive=true&userId=$USERID" | jq "[.Items[] | select(.ImageTags.Primary)] | length")" -gt 0 ]'
  want=$(union_of "Persons?Recursive=true" '.Items[].Name' | wc -l)
  got=$(api "Persons?Recursive=true&userId=$USERID" | jq .TotalRecordCount)
  check "merged: Persons total is the distinct union ($want)" '[ "$got" = "$want" ]' "$got"
  want=$(union_of "Studios?Recursive=true" '.Items[].Name')
  got=$(api "Studios?Recursive=true&userId=$USERID" | uniq_names '.Items[].Name')
  check "merged: Studios is the union" '[ "$got" = "$want" ]' "$got | $want"

  want=$(union_of "Items/Filters2?Recursive=true" '.AudioLanguages[]?.Value')
  got=$(api "Items/Filters2?Recursive=true&userId=$USERID" | uniq_names '.AudioLanguages[]?.Value')
  check "merged: Filters2 audio languages are the union" '[ "$got" = "$want" ]' "$got | $want"
  # Legacy Filters needs a ParentId: each server's Movies vs the merged one.
  local base t lib
  want=$(for base in "$S1" "$S2" "$S3"; do
    t=$(up_token "$base" "$UP_USER" "$UP_PASS")
    lib=$(up_get "$base" "$t" "UserViews" | jq -r '.Items[] | select(.Name=="Movies") | .Id')
    [ -n "$lib" ] && up_get "$base" "$t" "Items/Filters?ParentId=$lib&IncludeItemTypes=Movie" | jq -r '.Years[]?'
  done | sort -u)
  got=$(api "Items/Filters?ParentId=$(view_id Movies)&IncludeItemTypes=Movie&userId=$USERID" | jq -r '.Years[]?' | sort -u)
  check "merged: Filters years (Movies) are the union" '[ "$got" = "$want" ]' "$got | $want"

  local c movies series eps sum
  c=$(api "Items/Counts?userId=$USERID")
  movies=$(api "Items?Recursive=true&IncludeItemTypes=Movie&Limit=0&userId=$USERID" | jq .TotalRecordCount)
  series=$(api "Items?Recursive=true&IncludeItemTypes=Series&Limit=0&userId=$USERID" | jq .TotalRecordCount)
  eps=$(api "Items?Recursive=true&IncludeItemTypes=Episode&Limit=0&userId=$USERID" | jq .TotalRecordCount)
  check "merged: Items/Counts matches the merged library ($movies movies, $series series, $eps episodes)" \
    '[ "$(jq -c "[.MovieCount,.SeriesCount,.EpisodeCount]" <<<"$c")" = "[$movies,$series,$eps]" ]' "$c"
  sum=$(each_server "Items/Counts" | jq -s 'map(.MovieCount) | add')
  check "merged: duplicates are not double counted ($sum upstream movies)" '[ "$(jq .MovieCount <<<"$c")" -lt "$sum" ]' "$c"

  got=$(api "Search/Hints?searchTerm=Big&Limit=20&userId=$USERID" | jq '[.SearchHints[] | select(.Name=="Big Buck Bunny")] | length')
  check "merged: search hints list a title on two servers once" '[ "$got" = 1 ]' "$got"
  got=$(api "Items?searchTerm=Big&Recursive=true&IncludeItemTypes=Movie,Series,Episode&Limit=24&userId=$USERID" | jq '[.Items[] | select(.Name=="Big Buck Bunny")] | length')
  check "merged: search Items lists it once" '[ "$got" = 1 ]' "$got"
  local g; g=$(api "Genres?Recursive=true&IncludeItemTypes=Movie&userId=$USERID" | jq -c '[.Items[] | select(.Name=="Animation")][0] | [.MovieCount, .ChildCount]')
  movies=$(api "Items?Recursive=true&IncludeItemTypes=Movie&Genres=Animation&Limit=0&userId=$USERID" | jq .TotalRecordCount)
  check "merged: genre MovieCount counts merged movies ($movies)" '[ "$g" = "[$movies,$movies]" ]' "$g"
  stop_proxy
}
