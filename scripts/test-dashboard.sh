#!/usr/bin/env bash
# Smoke test for the observability stack in docker-compose.yml: starts Prometheus
# and Grafana, then checks datasource + dashboard provisioning, the scrape target
# and the alert rules. Run `cargo xtask check` for the dashboard JSON itself.
set -euo pipefail
cd "$(dirname "$0")/.."

if command -v docker >/dev/null && docker compose version >/dev/null 2>&1; then COMPOSE=(docker compose)
elif command -v docker-compose >/dev/null; then COMPOSE=(docker-compose)
elif command -v podman >/dev/null; then COMPOSE=(podman compose)
else echo "need docker compose, docker-compose or podman compose" >&2; exit 1; fi

# Same credentials as docker-compose.yml (.env overrides the defaults).
[ -f .env ] && set -a && . ./.env && set +a
AUTH="${GRAFANA_ADMIN_USER:-admin}:${GRAFANA_ADMIN_PASSWORD:-changeme}"
G=http://localhost:3000 P=http://localhost:9090

wait_for() { # url name
  for _ in $(seq 60); do curl -fs "$1" >/dev/null 2>&1 && { echo "ok   $2 is up"; return; }; sleep 1; done
  echo "FAIL $2 did not come up"; "${COMPOSE[@]}" logs --tail 50; exit 1
}
check() { # description command...
  if "${@:2}" >/dev/null 2>&1; then echo "ok   $1"; else echo "FAIL $1"; fail=1; fi
}
fail=0

[ -n "${NO_START:-}" ] || "${COMPOSE[@]}" up -d prometheus grafana   # NO_START=1: test a running stack
wait_for "$P/-/ready" Prometheus
wait_for "$G/api/health" Grafana
sleep 3

check "datasource 'prometheus' provisioned" curl -fs -u "$AUTH" "$G/api/datasources/uid/prometheus"
check "datasource reaches Prometheus" curl -fs -u "$AUTH" "$G/api/datasources/uid/prometheus/health"
check "dashboard provisioned" curl -fs -u "$AUTH" "$G/api/dashboards/uid/jellyfin-merge-proxy"
check "scrape job configured" sh -c "curl -fs $P/api/v1/targets | grep -q jellyfin-merge-proxy"
check "alert rules loaded and healthy" sh -c \
  "curl -fs $P/api/v1/rules | jq -e '[.data.groups[].rules[]] | length > 0 and all(.health != \"err\")'"
if curl -fs "$P/api/v1/query?query=up%7Bjob%3D%22jellyfin-merge-proxy%22%7D" | jq -e '.data.result[0].value[1] == "1"' >/dev/null; then
  echo "ok   proxy is being scraped"
else
  echo "warn proxy target not up yet (start it: ${COMPOSE[*]} up -d jellyfin-merge-proxy)"
fi

echo
echo "Grafana:    $G  (dashboard: $G/d/jellyfin-merge-proxy)"
echo "Prometheus: $P"
exit $fail
