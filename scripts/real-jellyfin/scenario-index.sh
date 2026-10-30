# Catalog index scenarios for scripts/real-jellyfin-test.sh (sourced; uses its
# helpers). Checks that library queries answered from the merged index behave
# like one Jellyfin server: exact totals, deep paging, sorting by keys the
# DTOs do not carry, name filters across servers, split shows, stable ids.

index_queries() { # endpoint outcome -> counter value (empty if never)
  curl -s "${CURL_TLS[@]}" "$PURL/metrics" | grep '^jmp_index_queries_total' \
    | grep -F "endpoint=\"$1\"" | grep -F "outcome=\"$2\"" | awk '{print $NF}' | head -1
}
index_warm() { [ -n "$(metric 'jmp_index_canonical{kind="items"}')" ]; }
# All pages of a query ($1 = query without paging, $2 = page size) -> "total|names in order|ids"
all_pages() {
  local start=0 total names=() ids=() page n
  while :; do
    page=$(api "$1&StartIndex=$start&Limit=$2")
    total=$(jq -r .TotalRecordCount <<<"$page")
    n=$(jq '.Items | length' <<<"$page")
    mapfile -t -O "${#names[@]}" names < <(jq -r '.Items[].Name' <<<"$page")
    mapfile -t -O "${#ids[@]}" ids < <(jq -r '.Items[].Id' <<<"$page")
    start=$((start + n))
    { [ "$n" -lt "$2" ] || [ "$start" -ge "$total" ]; } && break
  done
  echo "$total|$(IFS=,; echo "${names[*]}")|$(printf '%s\n' "${ids[@]}" | sort -u | wc -l)"
}
# The proxy's order restricted to an upstream's titles equals that upstream's
# own order for the same sort. $1 = SortBy,SortOrder suffix.
sort_matches_upstreams() {
  local q="Items?Recursive=true&IncludeItemTypes=Movie&$1" proxy base t up mine
  proxy=$(api "$q" | jq -c '[.Items[].Name]')
  for base in "$S1" "$S2" "$S3"; do
    t=$(up_token "$base" "$UP_USER" "$UP_PASS")
    up=$(up_get "$base" "$t" "$q" | jq -c '[.Items[].Name]')
    mine=$(jq -c --argjson up "$up" '[.[] | select(. as $n | $up | index($n))]' <<<"$proxy")
    [ "$mine" = "$up" ] || { echo "$base: proxy $mine upstream $up"; return 1; }
  done
}

scenario_index() {
  write_config "$(three_servers)"; start_proxy || { fail "proxy start"; return; }
  login family familypw
  check "index warm (crawled or loaded)" 'wait_for 120 index_warm'
  local mv; mv=$(view_id Movies)
  local first; first=$(api "Items?ParentId=$mv&Recursive=true&IncludeItemTypes=Movie&SortBy=SortName&Limit=2")
  check "Items answered from the index" '[ -n "$(index_queries items index)" ]'
  check "exact TotalRecordCount on a short page" '[ "$(jq -c "[(.Items|length), .TotalRecordCount]" <<<"$first")" = "[2,5]" ]' "$(jq -c '[(.Items|length), .TotalRecordCount]' <<<"$first")"
  local p; p=$(all_pages "Items?ParentId=$mv&Recursive=true&IncludeItemTypes=Movie&SortBy=SortName" 2)
  check "paging (Limit=2) walks the merged list once, in order" '[ "$p" = "5|Big Buck Bunny,Charade,Night of the Living Dead,Sintel,Tears of Steel|5" ]' "$p"
  local past; past=$(api "Items?ParentId=$mv&Recursive=true&IncludeItemTypes=Movie&StartIndex=50&Limit=5" | jq -c '[(.Items|length), .TotalRecordCount]')
  check "StartIndex past the end: empty page, exact total" '[ "$past" = "[0,5]" ]' "$past"
  local deep; deep=$(all_pages "Items?Recursive=true&SortBy=SortName" 3)
  check "deep paging over everything: no duplicates, count = total" '[ "${deep%%|*}" -gt 10 ] && [ "${deep%%|*}" = "${deep##*|}" ]' "${deep%%|*} vs ${deep##*|}"

  # Sorting, including keys the DTOs do not carry.
  local k o
  for k in OfficialRating CommunityRating CriticRating ProductionYear PremiereDate Runtime Name; do
    for o in Ascending Descending; do
      check "SortBy=$k $o matches every upstream" 'sort_matches_upstreams "SortBy=$k,SortName&SortOrder=$o"' "$(sort_matches_upstreams "SortBy=$k,SortName&SortOrder=$o")"
    done
  done
  local bare dated
  bare=$(api "Items?Recursive=true&IncludeItemTypes=Movie&SortBy=DateCreated,SortName&SortOrder=Descending" | jq -c '[.Items[].Name]')
  dated=$(api "Items?Recursive=true&IncludeItemTypes=Movie&SortBy=DateCreated,SortName&SortOrder=Descending&Fields=DateCreated")
  check "SortBy=DateCreated without Fields=DateCreated" '[ "$bare" = "$(jq -c "[.Items[].Name]" <<<"$dated")" ] && jq -e "[.Items[].DateCreated] | . == (sort | reverse)" <<<"$dated" >/dev/null' "$bare"
  # Tears of Steel has no rating, "NR" (Charade on s3, Night of the Living
  # Dead on s1) has no score: all three sort first, then G, PG.
  local nulls; nulls=$(api "Items?ParentId=$mv&Recursive=true&IncludeItemTypes=Movie&SortBy=OfficialRating,SortName" | jq -r '[.Items[].Name] | join(",")')
  check "unrated sorts first, across servers (like Jellyfin)" '[ "$nulls" = "Charade,Night of the Living Dead,Tears of Steel,Big Buck Bunny,Sintel" ]' "$nulls"

  # Names merged across servers, and filters on them.
  local sf; sf=$(api "Items?Recursive=true&IncludeItemTypes=Movie&Genres=Science%20Fiction")
  check "Genres= filter spans servers" '[ "$(names <<<"$sf")|$(jq .TotalRecordCount <<<"$sf")" = "Elephants Dream,Night of the Living Dead,Plan 9 from Outer Space,Tears of Steel|4" ]' "$(names <<<"$sf")"
  local gl; gl=$(api "Genres?Recursive=true&IncludeItemTypes=Movie")
  check "Genres listed once each" '[ "$(jq "[.Items[] | select(.Name==\"Science Fiction\")] | length" <<<"$gl")" = 1 ] && jq -e "[.Items[].Name] | length == (unique | length)" <<<"$gl" >/dev/null'
  local gid; gid=$(jq -r '.Items[] | select(.Name=="Science Fiction") | .Id' <<<"$gl")
  check "GenreIds= (merged genre id) spans servers" '[ "$(api "Items?Recursive=true&IncludeItemTypes=Movie&GenreIds=$gid" | names)" = "$(names <<<"$sf")" ]'
  local per; per=$(api "Persons?searchTerm=Roosendaal")
  check "Persons: one merged Ton Roosendaal" '[ "$(jq -c "[.Items[].Name]" <<<"$per")" = "[\"Ton Roosendaal\"]" ]' "$(jq -c '[.Items[].Name]' <<<"$per")"
  local pid; pid=$(jq -r '.Items[0].Id' <<<"$per")
  local pm; pm=$(api "Items?Recursive=true&IncludeItemTypes=Movie&PersonIds=$pid" | names)
  check "PersonIds= spans servers" '[ "$pm" = "Big Buck Bunny,Elephants Dream,Sintel,Tears of Steel" ]' "$pm"
  local f; f=$(api "Items/Filters?ParentId=$mv&IncludeItemTypes=Movie")
  check "Items/Filters: merged genres, years" 'jq -e "(.Genres | index(\"Science Fiction\")) and (.Genres | length == (unique | length)) and (.Years | index(1963))" <<<"$f" >/dev/null' "$(jq -c .Genres <<<"$f")"

  # One canonical id per item, everywhere.
  local bbb; bbb=$(movie_id "Big Buck Bunny")
  local hint; hint=$(api "Search/Hints?searchTerm=bunny&IncludeItemTypes=Movie" | jq -r '.SearchHints[0].Id')
  check "search hint id = list id (canonical)" '[ "$hint" = "$bbb" ]' "$hint vs $bbb"
  local ser; ser=$(api "Items?Recursive=true&IncludeItemTypes=Series&Genres=Drama")
  check "split series: one item, exact total" '[ "$(names <<<"$ser")|$(jq .TotalRecordCount <<<"$ser")" = "Breaking Bad|1" ]' "$(names <<<"$ser")"
  local bb; bb=$(jq -r '.Items[0].Id' <<<"$ser")
  local eps; eps=$(api "Items?ParentId=$bb&Recursive=true&IncludeItemTypes=Episode&SortBy=ParentIndexNumber,IndexNumber")
  check "split series: Items?ParentId=<series> has all 4 episodes" '[ "$(jq -c "[.TotalRecordCount, [.Items[] | \"S\(.ParentIndexNumber)E\(.IndexNumber)\"]]" <<<"$eps")" = "[4,[\"S1E1\",\"S1E2\",\"S1E3\",\"S2E1\"]]" ]' "$(jq -c '[.TotalRecordCount, [.Items[].Name]]' <<<"$eps")"
  check "split series: episodes point at the canonical series" 'jq -e --arg s "$bb" "[.Items[].SeriesId] | all(. == \$s)" <<<"$eps" >/dev/null'

  # Persistence: restart serves from the index at once, with the same ids.
  stop_proxy; start_proxy || { fail "proxy restart"; return; }
  login family familypw
  local again; again=$(movie_id "Big Buck Bunny")
  check "restart: answered from the persisted index" '[ -n "$(index_queries items index)" ]'
  check "restart: canonical id stable" '[ "$again" = "$bbb" ]' "$again vs $bbb"

  # Unsupported query parameters fall back to live fan-out.
  api "Items?Recursive=true&IncludeItemTypes=Movie&HasSubtitles=true" >/dev/null
  check "unsupported filter -> live fan-out (counted)" '[ -n "$(index_queries items unsupported)" ]'
  local subs; subs=$(api "Items?Recursive=true&IncludeItemTypes=Movie&HasSubtitles=true" | names)
  check "HasSubtitles via fan-out" '[ "$subs" = "Big Buck Bunny" ]' "$subs"
}
