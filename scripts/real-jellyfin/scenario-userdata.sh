# Per-user state scenarios for scripts/real-jellyfin-test.sh (sourced; uses
# its helpers). Two proxy users share one upstream account; the proxy keeps
# their played/favourite/resume state apart, shows a split series as one
# series with one NextUp, and writes changes through to every copy.

seeded() { [ -n "$(curl -s "${CURL_TLS[@]}" "$PURL/metrics" | grep '^jmp_userdata_seeds_total{outcome="ok"}')" ]; }
as_family() { TOKEN=$FT; USERID=$FU; }
as_kids() { TOKEN=$KT; USERID=$KU; }
ud_of() { api "Items/$1?userId=$USERID" | jq -c .UserData; }
up_has() { # base name -> true if that server has the title
  local t; t=$(up_token "$1" "$UP_USER" "$UP_PASS")
  up_get "$1" "$t" "Items?Recursive=true&IncludeItemTypes=Movie&searchTerm=$(jq -rn --arg v "$2" '$v|@uri')" \
    | jq -e --arg n "$2" '[.Items[] | select(.Name==$n)] | length > 0' >/dev/null
}
ep_id() { api "Shows/$1/Episodes?userId=$USERID" | jq -r --argjson s "$2" --argjson e "$3" '.Items[] | select(.ParentIndexNumber==$s and .IndexNumber==$e) | .Id'; }
nextup_of() { api "Shows/NextUp?Limit=20${1:-}" | jq -r '[.Items[] | "\(.SeriesName):S\(.ParentIndexNumber)E\(.IndexNumber)"] | join(",")'; }

scenario_userdata() {
  reset_userdata
  rm -f "$WORK/data/"*.db* 2>/dev/null || true
  write_config "$(three_servers)"; start_proxy || { fail "proxy start"; return; }
  login family familypw; FT=$TOKEN; FU=$USERID
  login kids kidspw; KT=$TOKEN; KU=$USERID
  check "index warm" 'wait_for 120 index_warm'
  check "upstream state imported once per user and server" 'wait_for 60 seeded'

  # Played: per proxy user, written through to every copy.
  as_family
  local bbb; bbb=$(movie_id "Big Buck Bunny")
  check "mark played -> 200 with proxy UserData" '[ "$(api "UserItems/$bbb/UserData" >/dev/null; post "UserPlayedItems/$bbb")" = 200 ]'
  check "family sees it played" '[ "$(ud_of "$bbb" | jq -r .Played)" = true ]' "$(ud_of "$bbb")"
  local base n=0 ok=0
  for base in "$S1" "$S2" "$S3"; do
    up_has "$base" "Big Buck Bunny" || continue
    n=$((n + 1)); [ "$(up_userdata "$base" "Big Buck Bunny" | jq -r .Played)" = true ] && ok=$((ok + 1))
  done
  check "written through to every copy ($n servers)" '[ "$n" -ge 2 ] && [ "$ok" = "$n" ]' "$ok of $n"
  as_kids
  check "kids (same upstream account) does not see it played" '[ "$(ud_of "$bbb" | jq -r .Played)" = false ]' "$(ud_of "$bbb")"
  local kp; kp=$(api "Items?Recursive=true&IncludeItemTypes=Movie&Filters=IsPlayed" | names)
  check "kids: Filters=IsPlayed empty" '[ -z "$kp" ]' "$kp"
  as_family
  local fp; fp=$(api "Items?Recursive=true&IncludeItemTypes=Movie&Filters=IsPlayed" | names)
  check "family: Filters=IsPlayed from the index" '[ "$fp" = "Big Buck Bunny" ] && [ -n "$(index_queries items index)" ]' "$fp"
  local fu all; fu=$(api "Items?Recursive=true&IncludeItemTypes=Movie&IsPlayed=false" | jq .TotalRecordCount)
  all=$(api "Items?Recursive=true&IncludeItemTypes=Movie" | jq .TotalRecordCount)
  check "family: IsPlayed=false exact total" '[ "$fu" = $((all - 1)) ]' "$fu of $all"

  # Favourites and likes.
  local sin; sin=$(movie_id Sintel)
  post "UserFavoriteItems/$sin" >/dev/null
  check "family: IsFavorite=true" '[ "$(api "Items?Recursive=true&IncludeItemTypes=Movie&IsFavorite=true" | names)" = Sintel ]'
  as_kids
  check "kids: no favourites" '[ "$(api "Items?Recursive=true&IncludeItemTypes=Movie&Filters=IsFavorite" | jq .TotalRecordCount)" = 0 ]'
  post "Users/$KU/FavoriteItems/$bbb" >/dev/null
  check "kids: favourite via legacy route, own list" '[ "$(api "Items?Recursive=true&IncludeItemTypes=Movie&Filters=IsFavorite" | names)" = "Big Buck Bunny" ]'
  as_family
  post "UserItems/$sin/Rating?Likes=true" >/dev/null
  check "Likes stored" '[ "$(ud_of "$sin" | jq -c "[.IsFavorite, .Likes]")" = "[true,true]" ]' "$(ud_of "$sin")"
  check "Filters=Likes" '[ "$(api "Items?Recursive=true&IncludeItemTypes=Movie&Filters=Likes" | names)" = Sintel ]'

  # Resume points per user.
  local tos; tos=$(movie_id "Tears of Steel")
  for e in Playing Playing/Progress Playing/Stopped; do
    post "Sessions/$e" "{\"ItemId\":\"$tos\",\"MediaSourceId\":\"$tos\",\"PositionTicks\":30000000}" >/dev/null
  done
  check "resume point kept (3 s)" '[ "$(ud_of "$tos" | jq -c "[.PlaybackPositionTicks, .Played, .PlayCount]")" = "[30000000,false,1]" ]' "$(ud_of "$tos")"
  local res; res=$(api "UserItems/Resume?MediaTypes=Video&Limit=12")
  check "family: Resume lists it (from the index)" '[ "$(names <<<"$res")" = "Tears of Steel" ] && [ -n "$(index_queries resume index)" ]' "$(names <<<"$res")"
  as_kids
  check "kids: Resume empty" '[ "$(api "UserItems/Resume?MediaTypes=Video" | jq .TotalRecordCount)" = 0 ]'
  as_family
  post "Sessions/Playing/Stopped" "{\"ItemId\":\"$tos\",\"PositionTicks\":79000000}" >/dev/null
  check "stop past 90% -> played, position reset" '[ "$(ud_of "$tos" | jq -c "[.PlaybackPositionTicks, .Played]")" = "[0,true]" ]' "$(ud_of "$tos")"

  # A show split across s1 and s2: one series, one NextUp.
  local bb; bb=$(api "Items?Recursive=true&IncludeItemTypes=Series&Genres=Drama" | jq -r '.Items[0].Id')
  local e
  for e in "1 1" "1 2" "1 3"; do post "UserPlayedItems/$(ep_id "$bb" $e)" >/dev/null; done
  local nu; nu=$(nextup_of)
  check "split show: one NextUp entry, the s2 episode" '[ "$nu" = "Breaking Bad:S2E1" ]' "$nu"
  check "series UserData: 1 unplayed of 4" '[ "$(ud_of "$bb" | jq -c "[.Played, .UnplayedItemCount, (.PlayedPercentage|floor)]")" = "[false,1,75]" ]' "$(ud_of "$bb")"
  as_kids
  check "kids: no NextUp" '[ -z "$(nextup_of)" ]' "$(nextup_of)"
  check "kids: NextUp?SeriesId -> first episode" '[ "$(nextup_of "&SeriesId=$bb")" = "Breaking Bad:S1E1" ]' "$(nextup_of "&SeriesId=$bb")"
  as_family
  post "UserPlayedItems/$(ep_id "$bb" 2 1)" >/dev/null
  check "all watched: series played, no NextUp" '[ "$(ud_of "$bb" | jq -r .Played)" = true ] && [ -z "$(nextup_of)" ]' "$(nextup_of)"
  post "UserPlayedItems/$bb" "" DELETE >/dev/null
  check "series unplayed marks every episode" '[ "$(ud_of "$bb" | jq -c "[.Played, .UnplayedItemCount]")" = "[false,4]" ]' "$(ud_of "$bb")"
  check "SortBy=DatePlayed answered" '[ "$(api "Items?Recursive=true&IncludeItemTypes=Movie&Filters=IsPlayed&SortBy=DatePlayed&SortOrder=Descending" | jq -r ".Items[0].Name")" = "Tears of Steel" ]'

  check "write-through counted" '[ -n "$(metric "jmp_userdata_write_through_total{outcome=\"ok\"}")" ]'

  # Persistence.
  stop_proxy; start_proxy || { fail "proxy restart"; return; }
  login family familypw; FT=$TOKEN; FU=$USERID
  login kids kidspw; KT=$TOKEN; KU=$USERID
  as_family
  check "restart: family state kept" '[ "$(ud_of "$bbb" | jq -r .Played)" = true ] && [ "$(ud_of "$sin" | jq -r .IsFavorite)" = true ]'
  as_kids
  check "restart: kids state kept" '[ "$(ud_of "$bbb" | jq -c "[.Played, .IsFavorite]")" = "[false,true]" ]' "$(ud_of "$bbb")"
  reset_userdata
}
