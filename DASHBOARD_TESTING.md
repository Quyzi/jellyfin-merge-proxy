# Grafana dashboard and alerts: development and testing

`deploy/grafana-dashboard.json` is **generated**. Do not edit it by hand (or
export it from the Grafana UI); change `xtask/src/dashboard.rs` and run:

```sh
cargo xtask dashboard   # regenerate deploy/grafana-dashboard.json, then check
cargo xtask check       # fail if the JSON is stale or invalid (also part of `cargo test`)
```

`cargo xtask` is an alias from `.cargo/config.toml` for `cargo run -p xtask --`.

`cargo xtask check` (and the `xtask` tests run by `cargo test --workspace`) verify:

* the committed JSON is byte-identical to what `xtask/src/dashboard.rs` generates
  (staleness);
* every metric in panel queries, annotations and `deploy/prometheus/alerts.yml`
  exists in `jmp::telemetry::METRICS` (or is a `process_*` metric), with
  `_bucket`/`_sum`/`_count` only on histograms;
* every label used in matchers, `by (..)`/`on (..)` and `{{label}}` legends is a
  label of the queried metrics (plus `instance`, `job`, `le`);
* panels have a description, a unit, the `${datasource}` datasource, the
  `$instance` filter and `$__rate_interval`/`$__range` windows; ids are unique
  and the grid has no overlaps.

## Layout

| Row | Panels |
| --- | --- |
| Overview | stats with sparklines (req/s, 5xx ratio, p95, upstreams down, streams, throughput, sessions, next cert expiry), upstream health state timeline, responses by status class |
| Client traffic | top routes, latency heatmap, p50/p90/p99, proxy errors by kind, concurrency, per-route table |
| Upstream servers | per-upstream summary table, p95 by upstream, requests / failures bar gauges, errors & logins |
| Request pipeline | stacked time per stage, fan-out wait heatmap, fan-out results, partial responses |
| Streaming | throughput, streams by kind, TTFB heatmap, outcomes, Range / HLS |
| Merge, dedupe & ids | items in vs out, duplicates by key, dedupe winners, id lookups |
| Catalog cache | hit ratio, lookups, refresh p95, per-upstream table, failures & evictions |
| Clients & auth | proxy logins, token checks, argon2 time |
| Configuration & TLS | version, uptime, last config, upstreams; reloads, upstream changes, backends, certificate table, TLS reloads |
| Process | CPU, memory, file descriptors, Tokio |

Variables: `datasource`, `instance`, `server` (upstream). Annotations: config
applied (blue) and TLS reloads (purple).

Colors are consistent: green = healthy, orange = warning, red = failure,
blue = informational volume, purple = streaming/TLS accents.

## Running the stack

```sh
cp .env.example .env            # set GRAFANA_ADMIN_PASSWORD
docker compose up -d            # or: podman compose up -d
scripts/test-dashboard.sh       # provisioning, scrape and rule checks
```

Grafana (`:3000`) is provisioned from `deploy/grafana/provisioning/datasources`
(datasource uid `prometheus`) and `deploy/grafana/provisioning/dashboards`, which
loads the dashboard from `/etc/grafana/dashboards`. Only those two directories are
mounted so the image's own `plugins/` and `alerting/` provisioning dirs stay intact.
The dashboard is a single-file bind mount, so Grafana picks up `cargo xtask
dashboard` (an in-place write) within 30 s, but after a `git checkout` or an editor
that replaces the file, run `docker compose restart grafana`.

Prometheus (`:9090`) loads `deploy/prometheus/alerts.yml`. Check rules with:

```sh
promtool check rules deploy/prometheus/alerts.yml
curl -s localhost:9090/api/v1/rules | jq '.data.groups[].rules[] | {name, health, state}'
```

When `[metrics].listen`/`path` are set, point `deploy/prometheus.yml` at that port
and path (e.g. `jellyfin-merge-proxy:9090`, `metrics_path: /metrics`).

## Lab: traffic, query check and screenshots

`scripts/dashboard-lab/` has the tools used to exercise every panel against a
running proxy:

* `traffic.sh [seconds]`: logins (good and bad), browsing, Latest/Resume/NextUp,
  search, images, direct streams with Range seeks, HLS transcodes with segments,
  playstate reports, client give-ups. Configure with `JMP_URL`, `JMP_CACERT`,
  `JMP_USER`, `JMP_PASS`, `JMP_WORKERS`.
* `check-queries.sh [from]`: runs every dashboard query through Grafana's
  datasource API and lists errors and queries without series.

Operational events worth covering while traffic runs: a config hot reload (add or
remove a `[[server]]`, change a priority), a broken config (rejected, the old one
stays), stopping an upstream for a few minutes, and replacing the TLS certificate.

Known behavior: a `[[server]]` removed by a reload keeps exporting
`jmp_upstream_up 0` (and its other per-server series) until the proxy restarts, so it
shows as down in the health timeline and `UpstreamServerDown` fires for it.
