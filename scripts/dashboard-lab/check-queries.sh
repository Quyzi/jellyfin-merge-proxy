#!/usr/bin/env bash
# Run every query of deploy/grafana-dashboard.json through Grafana's datasource
# API (as the panels do) and list queries that error or return no series.
# usage: scripts/dashboard-lab/check-queries.sh [from=now-30m]
# env:   GRAFANA (http://localhost:3000), GRAFANA_AUTH (admin:changeme), DS_UID (prometheus)
set -euo pipefail
FROM=${1:-now-30m}
G=${GRAFANA:-http://localhost:3000}; AUTH=${GRAFANA_AUTH:-admin:changeme}; DS=${DS_UID:-prometheus}
DASH=$(dirname "$0")/../../deploy/grafana-dashboard.json
bad=0; total=0
while IFS=$'\t' read -r title ref instant expr; do
  total=$((total + 1))
  e=${expr//\$instance/.*}; e=${e//\$server/.*}; e=${e//\$__rate_interval/1m}; e=${e//\$__range/30m}
  body=$(jq -n --arg e "$e" --arg f "$FROM" --argjson i "$instant" --arg ds "$DS" \
    '{from:$f,to:"now",queries:[{refId:"A",datasource:{type:"prometheus",uid:$ds},expr:$e,instant:$i,range:($i|not),intervalMs:15000,maxDataPoints:500}]}')
  r=$(curl -s -u "$AUTH" -H 'Content-Type: application/json' "$G/api/ds/query" -d "$body")
  err=$(jq -r '.results.A.error // empty' <<<"$r")
  n=$(jq '[.results.A.frames[]? | select((.data.values[1] // []) | length > 0)] | length' <<<"$r")
  if [ -n "$err" ]; then bad=$((bad + 1)); printf 'ERROR  %-34s %s: %s\n' "$title" "$ref" "$err"
  elif [ "$n" = 0 ]; then printf 'EMPTY  %-34s %s: %s\n' "$title" "$ref" "${expr:0:110}"
  fi
done < <(jq -r '.panels[] | select(.type != "row") | .title as $t | .targets[] | [$t, .refId, (.instant // false), .expr] | @tsv' "$DASH")
echo "$total queries, $bad errors"
[ "$bad" = 0 ]
