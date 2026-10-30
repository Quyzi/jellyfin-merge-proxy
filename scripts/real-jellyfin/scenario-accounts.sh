# Per-user upstream accounts and parental limits, for
# scripts/real-jellyfin-test.sh (sourced; uses its helpers). The proxy user
# "kids" gets its own upstream account "kid" on s1 and s2 (s3 stays on the
# shared account) and max_parental_rating = "PG".

KID_USER=kid; KID_PASS=kidpw
admin_api() { # base method path [body]
  local t; t=$(up_token "$1" "$ADMIN_USER" "$ADMIN_PASS")
  curl -s -X "$2" -H "Authorization: MediaBrowser Client=\"jmp-verify\", Device=\"verify\", DeviceId=\"jmp-verify-admin\", Version=\"1\", Token=\"$t\"" \
    -H 'Content-Type: application/json' "$1/$3" ${4:+-d "$4"}
}
# Fresh upstream "kid" user (no watch state) on a server.
fresh_kid() {
  local id; id=$(admin_api "$1" GET Users | jq -r --arg u "$KID_USER" '.[] | select(.Name==$u) | .Id')
  [ -n "$id" ] && admin_api "$1" DELETE "Users/$id" >/dev/null
  admin_api "$1" POST Users/New "{\"Name\":\"$KID_USER\",\"Password\":\"$KID_PASS\"}" | jq -r .Id
}
kid_get() { up_get "$1" "$(up_token "$1" "$KID_USER" "$KID_PASS")" "$2"; }
kid_ud() { # base name -> UserData of that movie for the upstream kid user
  kid_get "$1" "Items?Recursive=true&IncludeItemTypes=Movie&searchTerm=$(jq -rn --arg v "$2" '$v|@uri')&enableUserData=true" \
    | jq -c --arg n "$2" '[.Items[] | select(.Name==$n)][0].UserData'
}
up_id_of() { # base token name -> item id
  up_get "$1" "$2" "Items?Recursive=true&IncludeItemTypes=Movie&searchTerm=$(jq -rn --arg v "$3" '$v|@uri')" \
    | jq -r --arg n "$3" '[.Items[] | select(.Name==$n)][0].Id'
}
# kids' settings, then the servers ([[user.account]] follows kids in write_config).
kids_config() { # max_parental_rating line, block_unrated
  printf '%s\nblock_unrated = %s\n' "$1" "$2"
  printf '\n[[user.account]]\nserver = "s1"\nuser = "%s"\npassword = "%s"\n' "$KID_USER" "$KID_PASS"
  printf '\n[[user.account]]\nserver = "s2"\nuser = "%s"\npassword = "%s"\n' "$KID_USER" "$KID_PASS"
  three_servers
}
total_of() { api "$1" | jq -r .TotalRecordCount; }
series_names() { api "Items?Recursive=true&IncludeItemTypes=Series&SortBy=SortName" | names; }

scenario_accounts() {
  reset_userdata
  rm -f "$WORK/data/"*.db* 2>/dev/null || true
  local k1 k2; k1=$(fresh_kid "$S1"); k2=$(fresh_kid "$S2")
  check "upstream kid accounts created" '[ -n "$k1" ] && [ -n "$k2" ] && [ "$k1" != null ]' "$k1 $k2"
  # State that only the kid account has upstream: seeding must import it.
  local kt sid; kt=$(up_token "$S1" "$KID_USER" "$KID_PASS"); sid=$(up_id_of "$S1" "$kt" Sintel)
  curl -s -o /dev/null -X POST -H "Authorization: MediaBrowser Client=\"jmp-verify\", Device=\"verify\", DeviceId=\"jmp-verify\", Version=\"1\", Token=\"$kt\"" "$S1/UserPlayedItems/$sid"

  write_config "$(kids_config 'max_parental_rating = "PG"' false)"; start_proxy || { fail "proxy start"; return; }
  check "account passwords encrypted on disk" 'wait_for 10 "! grep -q \"$KID_PASS\" \"$WORK/config.toml\"" && [ "$(grep -c "enc:v1:" "$WORK/config.toml")" -ge 4 ]' "$(grep -n password "$WORK/config.toml")"
  login family familypw; FT=$TOKEN; FU=$USERID
  login kids kidspw; KT=$TOKEN; KU=$USERID
  check "index warm" 'wait_for 120 index_warm'
  check "upstream state imported" 'wait_for 60 seeded'

  # Identity: the kids' requests use the kid account.
  as_kids
  local bbb; bbb=$(movie_id "Big Buck Bunny"); sid=$(movie_id Sintel)
  check "kids: seeded from the kid account (Sintel played)" '[ "$(ud_of "$sid" | jq -r .Played)" = true ]' "$(ud_of "$sid")"
  as_family
  check "family: shared account, Sintel not played" '[ "$(ud_of "$sid" | jq -r .Played)" = false ]' "$(ud_of "$sid")"
  check "account logins counted" '[ -n "$(metric "jmp_upstream_user_auth_total{server=\"s1\",outcome=\"success\"}")" ] && [ "$(metric "jmp_upstream_user_accounts{server=\"s2\"}")" = 1 ]'
  as_kids
  post "UserFavoriteItems/$bbb" >/dev/null
  check "kids' favourite written to kid@s1 and kid@s2" '[ "$(kid_ud "$S1" "Big Buck Bunny" | jq -r .IsFavorite)" = true ] && [ "$(kid_ud "$S2" "Big Buck Bunny" | jq -r .IsFavorite)" = true ]' "$(kid_ud "$S1" "Big Buck Bunny")"
  check "... not to the shared account on s1" '[ "$(up_userdata "$S1" "Big Buck Bunny" | jq -r .IsFavorite)" = false ]' "$(up_userdata "$S1" "Big Buck Bunny")"
  check "... and to the shared account on s3 (no kid account there)" '[ "$(up_userdata "$S3" "Big Buck Bunny" | jq -r .IsFavorite)" = true ]'
  post "Sessions/Playing" "{\"ItemId\":\"$bbb\",\"MediaSourceId\":\"$bbb\",\"PositionTicks\":10000000}" >/dev/null
  local us; us=$(admin_api "$S1" GET "Sessions?ActiveWithinSeconds=300" | jq -r '[.[] | select(.Client=="jellyfin-merge-proxy" or .DeviceName=="jellyfin-merge-proxy") | .UserName] | unique | join(",")')
  check "upstream sessions: kid and jmp are separate devices" '[[ "$us" == *"$KID_USER"* && "$us" == *"$UP_USER"* ]]' "$us"
  post "Sessions/Playing/Stopped" "{\"ItemId\":\"$bbb\",\"PositionTicks\":10000000}" >/dev/null

  # Parental limit PG (10): Breaking Bad (TV-MA), The Office (TV-14) and every episode are over it.
  local bb; bb=$(as_family; api "Items?Recursive=true&IncludeItemTypes=Series&searchTerm=Breaking" | jq -r '.Items[0].Id')
  as_family
  check "family: both series" '[ "$(series_names)" = "Breaking Bad,The Office" ]' "$(series_names)"
  check "family: series item 200" '[ "$(code -H "$(auth)" "$PURL/Items/$bb?userId=$FU")" = 200 ]'
  local fe; fe=$(total_of "Items?Recursive=true&IncludeItemTypes=Episode")
  as_kids
  check "kids: no series, exact total 0" '[ -z "$(series_names)" ] && [ "$(total_of "Items?Recursive=true&IncludeItemTypes=Series")" = 0 ]' "$(series_names)"
  check "kids: no episodes (family has $fe)" '[ "$(total_of "Items?Recursive=true&IncludeItemTypes=Episode")" = 0 ] && [ "$fe" -gt 0 ]'
  check "kids: blocked series -> 404" '[ "$(code -H "$(auth)" "$PURL/Items/$bb?userId=$KU")" = 404 ] && [ "$(code -H "$(auth)" "$PURL/Shows/$bb/Episodes?userId=$KU")" = 404 ]'
  local img="$PURL/Items/$bb/Images/Primary"
  check "kids: images of blocked items still served (like Jellyfin)" '[ "$(code -H "$(auth)" "$img")" = "$(as_family; code -H "$(auth)" "$img")" ]'
  local sh; sh=$(api "Search/Hints?searchTerm=Breaking" | jq '.SearchHints | length')
  check "kids: search hides it" '[ "$sh" = 0 ] && [ "$(as_family; api "Search/Hints?searchTerm=Breaking" | jq ".SearchHints | length")" -gt 0 ]' "$sh"
  local shows; shows=$(view_id Shows)
  local kl; kl=$(api "Items/Latest?ParentId=$shows&Limit=20" | jq -r '[.[].Name] | join(",")')
  check "kids: Latest in Shows empty" '[ -z "$kl" ] && [ -n "$(as_family; api "Items/Latest?ParentId=$shows&Limit=20" | jq -r ".[].Name")" ]' "$kl"
  check "kids: no NextUp" '[ -z "$(nextup_of)" ]'
  local km; km=$(total_of "Items?Recursive=true&IncludeItemTypes=Movie")
  check "kids: unrated movies still shown ($km)" '[ "$km" = "$(as_family; total_of "Items?Recursive=true&IncludeItemTypes=Movie")" ]'
  check "parental hiding counted" '[ -n "$(metric "jmp_parental_hidden_total{source=\"index\"}")" ] && [ -n "$(metric "jmp_parental_hidden_total{source=\"item\"}")" ]'

  # Hot reload: block unrated -> only G/PG movies, exact totals and paging.
  write_config "$(kids_config 'max_parental_rating = "PG"' true)"
  local mv='api "Items?Recursive=true&IncludeItemTypes=Movie&SortBy=SortName" | names'
  check "reload block_unrated: kids movies = G/PG only" 'wait_for 15 "[ \"\$($mv)\" = \"Big Buck Bunny,Sintel\" ]"' "$(eval "$mv")"
  local pg; pg=$(api "Items?Recursive=true&IncludeItemTypes=Movie&SortBy=SortName&StartIndex=1&Limit=1")
  check "kids: page 2 of 2" '[ "$(jq -c "[.TotalRecordCount, .StartIndex, [.Items[].Name]]" <<<"$pg")" = "[2,1,[\"Sintel\"]]" ]' "$(jq -c "[.TotalRecordCount, .StartIndex]" <<<"$pg")"
  local tos; tos=$(as_family; movie_id "Tears of Steel")
  check "kids: unrated movie -> 404" '[ "$(code -H "$(auth)" "$PURL/Items/$tos?userId=$KU")" = 404 ]'
  write_config "$(kids_config 'max_parental_rating = "TV-14"' false)"
  check "reload TV-14: The Office visible, Breaking Bad not" 'wait_for 15 "[ \"\$(series_names)\" = \"The Office\" ]"' "$(series_names)"
  write_config "$(kids_config 'max_parental_rating = 17' false)"
  check "reload 17 (numeric): both series" 'wait_for 15 "[ \"\$(series_names)\" = \"Breaking Bad,The Office\" ]"' "$(series_names)"
  check "kids still see their own state after reloads" '[ "$(ud_of "$sid" | jq -r .Played)" = true ]'
  reset_userdata
}
