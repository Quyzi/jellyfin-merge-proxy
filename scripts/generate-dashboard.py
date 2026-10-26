#!/usr/bin/env python3
"""Generate a comprehensive Grafana dashboard for jellyfin-merge-proxy."""

import json

# Dashboard structure
dashboard_base = {
    "__inputs": [{"name": "DS_PROMETHEUS", "label": "Prometheus", "type": "datasource", "pluginId": "prometheus"}],
    "annotations": {
        "list": [
            {
                "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
                "enable": True,
                "expr": "changes(jmp_config_generation[1m]) > 0",
                "iconColor": "green",
                "name": "Config Reloads",
                "titleFormat": "Config Reload"
            },
            {
                "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
                "enable": True,
                "expr": 'changes(jmp_upstream_up{server=~"$server"}[2m]) > 0',
                "iconColor": "red",
                "name": "Server Up/Down",
                "titleFormat": "Server State Change"
            }
        ]
    },
    "editable": True,
    "fiscalYearStartMonth": 0,
    "graphTooltip": 1,
    "id": None,
    "links": [
        {
            "asDropdown": False,
            "icon": "doc",
            "includeVars": False,
            "keepTime": False,
            "tags": [],
            "targetBlank": True,
            "title": "Metrics Documentation",
            "tooltip": "View complete metrics reference",
            "type": "link",
            "url": "https://github.com/yourusername/jellyfin-merge-proxy/blob/main/METRICS.md"
        }
    ],
    "panels": [],
    "refresh": "30s",
    "schemaVersion": 39,
    "tags": ["jellyfin", "jmp", "proxy", "streaming"],
    "templating": {
        "list": [
            {
                "current": {"selected": False, "text": "Prometheus", "value": "Prometheus"},
                "hide": 0,
                "includeAll": False,
                "label": "Datasource",
                "multi": False,
                "name": "DS_PROMETHEUS",
                "options": [],
                "query": "prometheus",
                "refresh": 1,
                "skipUrlSync": False,
                "type": "datasource"
            },
            {
                "current": {"selected": False, "text": "All", "value": "$__all"},
                "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
                "definition": "label_values(jmp_upstream_up,server)",
                "hide": 0,
                "includeAll": True,
                "label": "Server",
                "multi": True,
                "name": "server",
                "options": [],
                "query": {"query": "label_values(jmp_upstream_up,server)", "refId": "StandardVariableQuery"},
                "refresh": 2,
                "sort": 1,
                "type": "query"
            },
            {
                "current": {"selected": False, "text": "All", "value": "$__all"},
                "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
                "definition": "label_values(jmp_http_requests_total,route)",
                "hide": 0,
                "includeAll": True,
                "label": "Route",
                "multi": True,
                "name": "route",
                "options": [],
                "query": {"query": "label_values(jmp_http_requests_total,route)", "refId": "StandardVariableQuery"},
                "refresh": 2,
                "sort": 1,
                "type": "query"
            },
            {
                "current": {"selected": False, "text": "All", "value": "$__all"},
                "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
                "definition": "label_values(jmp_http_requests_total,instance)",
                "hide": 0,
                "includeAll": True,
                "label": "Instance",
                "multi": True,
                "name": "instance",
                "options": [],
                "query": {"query": "label_values(jmp_http_requests_total,instance)", "refId": "StandardVariableQuery"},
                "refresh": 2,
                "sort": 1,
                "type": "query"
            }
        ]
    },
    "time": {"from": "now-6h", "to": "now"},
    "timepicker": {"refresh_intervals": ["10s", "30s", "1m", "5m", "15m", "30m", "1h"]},
    "timezone": "browser",
    "title": "Jellyfin Merge Proxy",
    "uid": "jellyfin-merge-proxy",
    "version": 2,
    "weekStart": ""
}

# Helper to create stat panels (overview row)
def stat(id, title, x, y, w, h, unit, expr, desc, thresholds=None):
    return {
        "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
        "fieldConfig": {
            "defaults": {
                "color": {"mode": "thresholds" if thresholds and len(thresholds.get("steps", [])) > 1 else "value"},
                "mappings": [],
                "thresholds": thresholds or {"mode": "absolute", "steps": [{"color": "green", "value": None}]},
                "unit": unit
            },
            "overrides": []
        },
        "gridPos": {"h": h, "w": w, "x": x, "y": y},
        "id": id,
        "options": {
            "colorMode": "background" if thresholds and len(thresholds.get("steps", [])) > 1 else "value",
            "graphMode": "area",
            "justifyMode": "auto",
            "orientation": "auto",
            "reduceOptions": {"values": False, "calcs": ["lastNotNull"], "fields": ""},
            "textMode": "auto",
            "wideLayout": True
        },
        "pluginVersion": "11.0.0",
        "targets": [{"datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"}, "expr": expr, "refId": "A"}],
        "title": title,
        "type": "stat",
        "description": desc
    }

# Helper to create timeseries panels
def timeseries(id, title, x, y, w, h, targets, unit="short", desc=""):
    return {
        "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
        "fieldConfig": {
            "defaults": {
                "color": {"mode": "palette-classic"},
                "custom": {
                    "axisBorderShow": False,
                    "axisCenteredZero": False,
                    "axisColorMode": "text",
                    "axisLabel": "",
                    "axisPlacement": "auto",
                    "barAlignment": 0,
                    "drawStyle": "line",
                    "fillOpacity": 10,
                    "gradientMode": "none",
                    "hideFrom": {"tooltip": False, "viz": False, "legend": False},
                    "insertNulls": False,
                    "lineInterpolation": "linear",
                    "lineWidth": 1,
                    "pointSize": 5,
                    "scaleDistribution": {"type": "linear"},
                    "showPoints": "never",
                    "spanNulls": False,
                    "stacking": {"group": "A", "mode": "none"},
                    "thresholdsStyle": {"mode": "off"}
                },
                "mappings": [],
                "thresholds": {"mode": "absolute", "steps": [{"color": "green", "value": None}]},
                "unit": unit
            },
            "overrides": []
        },
        "gridPos": {"h": h, "w": w, "x": x, "y": y},
        "id": id,
        "options": {
            "legend": {"calcs": [], "displayMode": "list", "placement": "bottom", "showLegend": True},
            "tooltip": {"mode": "multi", "sort": "none"}
        },
        "pluginVersion": "11.0.0",
        "targets": targets,
        "title": title,
        "type": "timeseries",
        "description": desc
    }

# Helper to create row
def row(id, title, y):
    return {
        "collapsed": False,
        "gridPos": {"h": 1, "w": 24, "x": 0, "y": y},
        "id": id,
        "panels": [],
        "title": title,
        "type": "row"
    }

# Target helper
def t(expr, legend, ref="A"):
    return {"datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"}, "expr": expr, "legendFormat": legend, "refId": ref}

# Build panels
panels = []
panel_id = 1
y = 0

# ===== OVERVIEW ROW =====
panels.append(row(panel_id, "Overview", y))
panel_id += 1
y += 1

# Top stats
panels.append(stat(panel_id, "Requests/s", 0, y, 3, 4, "reqps",
    'sum(rate(jmp_http_requests_total{instance=~"$instance"}[$__rate_interval]))',
    "Total HTTP requests per second",
    {"mode": "absolute", "steps": [{"color": "green", "value": None}, {"color": "yellow", "value": 100}, {"color": "red", "value": 500}]}))
panel_id += 1

panels.append(stat(panel_id, "Error Rate", 3, y, 3, 4, "percentunit",
    'sum(rate(jmp_http_requests_total{instance=~"$instance",status=~"5.."}[$__rate_interval])) / sum(rate(jmp_http_requests_total{instance=~"$instance"}[$__rate_interval]))',
    "5xx error percentage",
    {"mode": "absolute", "steps": [{"color": "green", "value": None}, {"color": "yellow", "value": 0.01}, {"color": "red", "value": 0.05}]}))
panel_id += 1

panels.append(stat(panel_id, "p95 Latency", 6, y, 3, 4, "s",
    'histogram_quantile(0.95, sum(rate(jmp_http_request_duration_seconds_bucket{instance=~"$instance"}[$__rate_interval])) by (le))',
    "95th percentile request duration",
    {"mode": "absolute", "steps": [{"color": "green", "value": None}, {"color": "yellow", "value": 1}, {"color": "red", "value": 5}]}))
panel_id += 1

panels.append(stat(panel_id, "Servers Up", 9, y, 3, 4, "short",
    'count(jmp_upstream_up{server=~"$server"} == 1) or vector(0)',
    "Number of healthy upstream servers",
    {"mode": "absolute", "steps": [{"color": "red", "value": None}, {"color": "yellow", "value": 1}, {"color": "green", "value": 2}]}))
panel_id += 1

panels.append(stat(panel_id, "Active Streams", 12, y, 3, 4, "short",
    'sum(jmp_streams_active{server=~"$server",instance=~"$instance"})',
    "Response bodies currently streaming"))
panel_id += 1

panels.append(stat(panel_id, "Active Sessions", 15, y, 3, 4, "short",
    'jmp_sessions_active{instance=~"$instance"}',
    "Active proxy client sessions"))
panel_id += 1

panels.append(stat(panel_id, "Cache Hit Ratio", 18, y, 3, 4, "percentunit",
    'sum(rate(jmp_cache_requests_total{outcome="hit",instance=~"$instance"}[$__rate_interval])) / sum(rate(jmp_cache_requests_total{instance=~"$instance"}[$__rate_interval]))',
    "Catalog cache hit percentage",
    {"mode": "absolute", "steps": [{"color": "red", "value": None}, {"color": "yellow", "value": 0.5}, {"color": "green", "value": 0.8}]}))
panel_id += 1

panels.append(stat(panel_id, "Cert Expiry", 21, y, 3, 4, "d",
    'min((jmp_tls_cert_expiry_timestamp_seconds - time()) / 86400)',
    "Days until nearest TLS cert expires",
    {"mode": "absolute", "steps": [{"color": "red", "value": None}, {"color": "yellow", "value": 14}, {"color": "green", "value": 30}]}))
panel_id += 1

y += 4

panels.append(stat(panel_id, "Uptime", 0, y, 4, 2, "s",
    'jmp_uptime_seconds{instance=~"$instance"}',
    "Seconds since process start"))
panel_id += 1

panels.append(timeseries(panel_id, "Build Info", 4, y, 20, 2,
    [t('jmp_build_info{instance=~"$instance"}', '{{version}} ({{git_sha}})')],
    "short", "Proxy version and git commit"))
panel_id += 1

y += 2

# ===== TRAFFIC ROW =====
panels.append(row(panel_id, "Traffic (RED: Rate, Errors, Duration)", y))
panel_id += 1
y += 1

panels.append(timeseries(panel_id, "Requests/s by Route", 0, y, 12, 8,
    [t('sum by (route) (rate(jmp_http_requests_total{route=~"$route",instance=~"$instance"}[$__rate_interval]))', '{{route}}')],
    "reqps", "Request rate per route template (RED: Rate)"))
panel_id += 1

panels.append(timeseries(panel_id, "Requests/s by Status", 12, y, 12, 8,
    [t('sum by (status) (rate(jmp_http_requests_total{route=~"$route",instance=~"$instance"}[$__rate_interval]))', '{{status}}')],
    "reqps", "Request rate by HTTP status code"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Errors by Kind", 0, y, 12, 8,
    [t('sum by (kind) (rate(jmp_http_errors_total{instance=~"$instance"}[$__rate_interval]))', '{{kind}}')],
    "reqps", "Proxy errors: unauthorized, not_found, upstream_unavailable, etc."))
panel_id += 1

panels.append(timeseries(panel_id, "Latency Percentiles", 12, y, 12, 8,
    [t('histogram_quantile(0.50, sum(rate(jmp_http_request_duration_seconds_bucket{route=~"$route",instance=~"$instance"}[$__rate_interval])) by (le))', 'p50', 'A'),
     t('histogram_quantile(0.95, sum(rate(jmp_http_request_duration_seconds_bucket{route=~"$route",instance=~"$instance"}[$__rate_interval])) by (le))', 'p95', 'B'),
     t('histogram_quantile(0.99, sum(rate(jmp_http_request_duration_seconds_bucket{route=~"$route",instance=~"$instance"}[$__rate_interval])) by (le))', 'p99', 'C')],
    "s", "Request duration percentiles (RED: Duration)"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Requests In Flight", 0, y, 24, 6,
    [t('jmp_http_requests_in_flight{instance=~"$instance"}', '{{instance}}')],
    "short", "Client requests being processed (headers not yet sent)"))
panel_id += 1

y += 6

# ===== UPSTREAM SERVERS ROW =====
panels.append(row(panel_id, "Upstream Servers", y))
panel_id += 1
y += 1

panels.append(timeseries(panel_id, "Server Health State", 0, y, 24, 6,
    [t('jmp_upstream_up{server=~"$server"}', '{{server}}')],
    "short", "1 = healthy, 0 = in back-off (down)"))
panel_id += 1

y += 6

panels.append(timeseries(panel_id, "Upstream p95 Latency by Server", 0, y, 12, 8,
    [t('histogram_quantile(0.95, sum by (le, server) (rate(jmp_upstream_request_duration_seconds_bucket{server=~"$server"}[$__rate_interval])))', '{{server}}')],
    "s", "95th percentile upstream response time per server"))
panel_id += 1

panels.append(timeseries(panel_id, "Upstream Requests by Status Class", 12, y, 12, 8,
    [t('sum by (server, status_class) (rate(jmp_upstream_requests_total{server=~"$server"}[$__rate_interval]))', '{{server}} {{status_class}}')],
    "reqps", "Upstream request rate by server and status class"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Upstream Transport Errors", 0, y, 12, 8,
    [t('sum by (server, kind) (rate(jmp_upstream_errors_total{server=~"$server"}[$__rate_interval]))', '{{server}} {{kind}}')],
    "reqps", "Transport errors: connect, dns, tls, timeout, body, other"))
panel_id += 1

panels.append(timeseries(panel_id, "Fan-out Results by Server", 12, y, 12, 8,
    [t('sum by (server, outcome) (rate(jmp_fanout_upstream_results_total{server=~"$server"}[$__rate_interval]))', '{{server}} {{outcome}}')],
    "reqps", "Per-upstream fan-out results: success, http_error, timeout, error, skipped"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Upstream Re-authentication", 0, y, 12, 6,
    [t('sum by (server) (rate(jmp_upstream_reauth_total{server=~"$server"}[$__rate_interval]))', '{{server}}')],
    "reqps", "Re-authentications triggered by upstream 401 responses"))
panel_id += 1

panels.append(timeseries(panel_id, "Upstream Token Age", 12, y, 12, 6,
    [t('jmp_upstream_token_age_seconds{server=~"$server"}', '{{server}}')],
    "s", "Seconds since upstream access token was obtained"))
panel_id += 1

y += 6

# ===== PIPELINE ROW =====
panels.append(row(panel_id, "Request Pipeline", y))
panel_id += 1
y += 1

# Stacked stage duration
stacked_panel = timeseries(panel_id, "Average Time per Pipeline Stage (Stacked)", 0, y, 24, 10,
    [t('sum by (stage) (rate(jmp_stage_duration_seconds_sum{instance=~"$instance"}[$__rate_interval])) / sum by (stage) (rate(jmp_stage_duration_seconds_count{instance=~"$instance"}[$__rate_interval]))', '{{stage}}')],
    "s", "Stacked view of pipeline stages: auth, id_decode, fanout, rewrite, merge, dedupe, sort_page, serialize, views_build")
stacked_panel["fieldConfig"]["defaults"]["custom"]["stacking"] = {"group": "A", "mode": "normal"}
stacked_panel["fieldConfig"]["defaults"]["custom"]["fillOpacity"] = 80
panels.append(stacked_panel)
panel_id += 1

y += 10

panels.append(timeseries(panel_id, "Stage p95 Duration", 0, y, 24, 8,
    [t('histogram_quantile(0.95, sum by (le, stage) (rate(jmp_stage_duration_seconds_bucket{instance=~"$instance"}[$__rate_interval])))', '{{stage}}')],
    "s", "95th percentile duration for each pipeline stage"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Fan-out: Upstreams Queried (p95)", 0, y, 12, 6,
    [t('histogram_quantile(0.95, sum by (le, endpoint) (rate(jmp_fanout_upstreams_bucket[$__rate_interval])))', '{{endpoint}}')],
    "short", "Number of upstreams queried per fan-out request"))
panel_id += 1

panels.append(timeseries(panel_id, "Fan-out: Slowest Upstream Wait (p95)", 12, y, 12, 6,
    [t('histogram_quantile(0.95, sum by (le, endpoint) (rate(jmp_fanout_slowest_seconds_bucket[$__rate_interval])))', '{{endpoint}}')],
    "s", "Time waiting for the slowest upstream in a fan-out"))
panel_id += 1

y += 6

# ===== STREAMING ROW =====
panels.append(row(panel_id, "Streaming (Playback, Images, HLS)", y))
panel_id += 1
y += 1

panels.append(timeseries(panel_id, "Active Streams by Kind", 0, y, 12, 8,
    [t('sum by (kind) (jmp_streams_active{server=~"$server"})', '{{kind}}')],
    "short", "Streaming responses by kind: direct, hls_segment, hls_playlist, transcode, image, download, web, other"))
panel_id += 1

panels.append(timeseries(panel_id, "Active Streams by Server", 12, y, 12, 8,
    [t('sum by (server) (jmp_streams_active{server=~"$server"})', '{{server}}')],
    "short", "Currently streaming responses per upstream server"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Stream Throughput by Server", 0, y, 12, 8,
    [t('sum by (server) (rate(jmp_stream_bytes_total{server=~"$server"}[$__rate_interval]))', '{{server}}')],
    "Bps", "Bytes/sec streamed from upstreams to clients, per server"))
panel_id += 1

panels.append(timeseries(panel_id, "Stream Throughput by Kind", 12, y, 12, 8,
    [t('sum by (kind) (rate(jmp_stream_bytes_total{server=~"$server"}[$__rate_interval]))', '{{kind}}')],
    "Bps", "Bytes/sec streamed, by stream kind"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Stream TTFB p95", 0, y, 12, 8,
    [t('histogram_quantile(0.95, sum by (le, server) (rate(jmp_stream_ttfb_seconds_bucket{server=~"$server"}[$__rate_interval])))', '{{server}}')],
    "s", "Time from request to first body byte (95th percentile)"))
panel_id += 1

panels.append(timeseries(panel_id, "Stream Endings by Outcome", 12, y, 12, 8,
    [t('sum by (outcome) (rate(jmp_stream_end_total{server=~"$server"}[$__rate_interval]))', '{{outcome}}')],
    "short", "How streams ended: complete, client_disconnect, upstream_error"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "HLS Playlist Rewrites", 0, y, 8, 6,
    [t('sum by (server) (rate(jmp_hls_playlist_rewrites_total{server=~"$server"}[$__rate_interval]))', '{{server}}')],
    "reqps", "HLS playlists rewritten to point at proxy"))
panel_id += 1

panels.append(timeseries(panel_id, "HLS Segment Requests", 8, y, 8, 6,
    [t('sum by (server) (rate(jmp_hls_segment_requests_total{server=~"$server"}[$__rate_interval]))', '{{server}}')],
    "reqps", "HLS segment requests proxied"))
panel_id += 1

panels.append(timeseries(panel_id, "Range Requests", 16, y, 8, 6,
    [t('sum by (server) (rate(jmp_range_requests_total{server=~"$server"}[$__rate_interval]))', '{{server}}')],
    "reqps", "Requests with Range header (partial content)"))
panel_id += 1

y += 6

# ===== MERGE AND DEDUPE ROW =====
panels.append(row(panel_id, "Merge and Deduplication", y))
panel_id += 1
y += 1

panels.append(timeseries(panel_id, "Merge: Items In vs Out", 0, y, 12, 8,
    [t('sum by (endpoint) (rate(jmp_merge_items_in_total[$__rate_interval]))', '{{endpoint}} in', 'A'),
     t('sum by (endpoint) (rate(jmp_merge_items_out_total[$__rate_interval]))', '{{endpoint}} out', 'B')],
    "short", "Items from upstreams vs items to clients (after dedupe and paging)"))
panel_id += 1

panels.append(timeseries(panel_id, "Duplicates Dropped by Key", 12, y, 12, 8,
    [t('sum by (key) (rate(jmp_dedupe_dropped_total[$__rate_interval]))', '{{key}}')],
    "short", "Duplicates removed by match key: tmdb, imdb, tvdb, title, index, name"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Dedupe Winner by Server", 0, y, 12, 8,
    [t('sum by (server) (rate(jmp_dedupe_winner_total{server=~"$server"}[$__rate_interval]))', '{{server}}')],
    "short", "Dedupe decisions won by each upstream"))
panel_id += 1

panels.append(timeseries(panel_id, "ID Table Size", 12, y, 12, 8,
    [t('jmp_ids_table_size{server=~"$server"}', '{{server}}')],
    "short", "Entries in the ID mapping table per server"))
panel_id += 1

y += 8

# ===== CACHE ROW =====
panels.append(row(panel_id, "Catalog Cache", y))
panel_id += 1
y += 1

panels.append(timeseries(panel_id, "Cache Requests by Outcome", 0, y, 12, 8,
    [t('sum by (outcome) (rate(jmp_cache_requests_total[$__rate_interval]))', '{{outcome}}')],
    "reqps", "Catalog cache lookups: hit, miss, stale"))
panel_id += 1

panels.append(timeseries(panel_id, "Cache Hit Ratio", 12, y, 12, 8,
    [t('sum(rate(jmp_cache_requests_total{outcome="hit"}[$__rate_interval])) / sum(rate(jmp_cache_requests_total[$__rate_interval]))', 'hit ratio')],
    "percentunit", "Percentage of cache lookups that hit"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Cache Entries", 0, y, 12, 6,
    [t('sum by (cache, server) (jmp_cache_entries{server=~"$server"})', '{{cache}} {{server}}')],
    "short", "Entries in catalog cache per cache and server"))
panel_id += 1

panels.append(timeseries(panel_id, "Cache Size", 12, y, 12, 6,
    [t('sum by (cache, server) (jmp_cache_bytes{server=~"$server"})', '{{cache}} {{server}}')],
    "bytes", "Serialized size of catalog cache"))
panel_id += 1

y += 6

panels.append(timeseries(panel_id, "Cache Refresh Duration p95", 0, y, 12, 6,
    [t('histogram_quantile(0.95, sum by (le, cache, server) (rate(jmp_cache_refresh_duration_seconds_bucket{server=~"$server"}[$__rate_interval])))', '{{cache}} {{server}}')],
    "s", "Catalog refresh call duration (95th percentile)"))
panel_id += 1

panels.append(timeseries(panel_id, "Cache Refresh Failures", 12, y, 12, 6,
    [t('sum by (cache, server) (rate(jmp_cache_refresh_failures_total{server=~"$server"}[$__rate_interval]))', '{{cache}} {{server}}')],
    "reqps", "Failed catalog refresh attempts"))
panel_id += 1

y += 6

# ===== AUTH AND SESSIONS ROW =====
panels.append(row(panel_id, "Authentication and Sessions", y))
panel_id += 1
y += 1

panels.append(timeseries(panel_id, "Proxy Logins by Outcome", 0, y, 12, 8,
    [t('sum by (outcome) (rate(jmp_logins_total[$__rate_interval]))', '{{outcome}}')],
    "reqps", "Login rate: success, unknown_user, bad_password, bad_request"))
panel_id += 1

panels.append(timeseries(panel_id, "Token Validations", 12, y, 12, 8,
    [t('sum by (outcome) (rate(jmp_token_validations_total[$__rate_interval]))', '{{outcome}}')],
    "reqps", "Token validation rate: valid, invalid, missing, anonymous"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Active Sessions", 0, y, 12, 6,
    [t('jmp_sessions_active', 'sessions')],
    "short", "Active proxy client sessions"))
panel_id += 1

panels.append(timeseries(panel_id, "Argon2 Duration p95", 12, y, 12, 6,
    [t('histogram_quantile(0.95, sum by (le, op) (rate(jmp_argon2_duration_seconds_bucket[$__rate_interval])))', '{{op}}')],
    "s", "argon2 hash/verify time (password operations)"))
panel_id += 1

y += 6

# ===== CONFIG AND TLS ROW =====
panels.append(row(panel_id, "Configuration and TLS", y))
panel_id += 1
y += 1

panels.append(timeseries(panel_id, "Config Reloads", 0, y, 8, 8,
    [t('sum by (outcome) (increase(jmp_config_reloads_total[1h]))', '{{outcome}}')],
    "short", "Config reload attempts (last hour): success, error, unchanged"))
panel_id += 1

panels.append(timeseries(panel_id, "Config Generation", 8, y, 8, 8,
    [t('jmp_config_generation', 'generation')],
    "short", "Config generation number (increments on successful reload)"))
panel_id += 1

panels.append(timeseries(panel_id, "Upstream Changes on Reload", 16, y, 8, 8,
    [t('sum by (change) (increase(jmp_config_upstream_changes_total[1h]))', '{{change}}')],
    "short", "Server changes (last hour): added, removed, rebuilt, changed"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Configured Upstreams", 0, y, 6, 6,
    [t('jmp_config_upstreams', 'upstreams')],
    "short", "Number of enabled upstreams"))
panel_id += 1

panels.append(timeseries(panel_id, "Active Servers", 6, y, 6, 6,
    [t('jmp_servers_active', 'active')],
    "short", "Backends currently serving requests"))
panel_id += 1

# TLS cert expiry table
cert_table = {
    "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
    "fieldConfig": {
        "defaults": {
            "custom": {"align": "auto", "cellOptions": {"type": "auto"}, "inspect": False},
            "mappings": [],
            "thresholds": {
                "mode": "absolute",
                "steps": [{"color": "green", "value": None}, {"color": "yellow", "value": 14}, {"color": "red", "value": 7}]
            },
            "unit": "d"
        },
        "overrides": []
    },
    "gridPos": {"h": 6, "w": 12, "x": 12, "y": y},
    "id": panel_id,
    "options": {
        "cellHeight": "sm",
        "footer": {"countRows": False, "fields": "", "reducer": ["sum"], "show": False},
        "showHeader": True
    },
    "pluginVersion": "11.0.0",
    "targets": [{
        "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
        "expr": '(jmp_tls_cert_expiry_timestamp_seconds - time()) / 86400',
        "format": "table",
        "instant": True,
        "refId": "A"
    }],
    "title": "TLS Certificate Expiry (Days Left)",
    "transformations": [{
        "id": "organize",
        "options": {
            "excludeByName": {"Time": True, "instance": True, "job": True},
            "renameByName": {"Value": "days_left", "cert": "Certificate"}
        }
    }],
    "type": "table",
    "description": "Days until TLS certificates expire (green > 14d, yellow 7-14d, red < 7d)"
}
panels.append(cert_table)
panel_id += 1

y += 6

panels.append(timeseries(panel_id, "TLS Reloads", 0, y, 24, 6,
    [t('sum by (target, outcome) (increase(jmp_tls_reloads_total[1h]))', '{{target}} {{outcome}}')],
    "short", "TLS material reloads (last hour): listener, upstream"))
panel_id += 1

y += 6

# ===== PROCESS AND RUNTIME ROW =====
panels.append(row(panel_id, "Process and Runtime", y))
panel_id += 1
y += 1

panels.append(timeseries(panel_id, "CPU Usage", 0, y, 12, 8,
    [t('rate(process_cpu_seconds_total{instance=~"$instance"}[$__rate_interval])', '{{instance}}')],
    "percentunit", "CPU usage (0.5 = 50% of one core)"))
panel_id += 1

panels.append(timeseries(panel_id, "Memory Usage", 12, y, 12, 8,
    [t('process_resident_memory_bytes{instance=~"$instance"}', '{{instance}}')],
    "bytes", "Resident memory (RSS)"))
panel_id += 1

y += 8

panels.append(timeseries(panel_id, "Open File Descriptors", 0, y, 8, 6,
    [t('process_open_fds{instance=~"$instance"}', '{{instance}}')],
    "short", "Open file descriptors"))
panel_id += 1

panels.append(timeseries(panel_id, "Tokio Alive Tasks", 8, y, 8, 6,
    [t('jmp_tokio_alive_tasks{instance=~"$instance"}', '{{instance}}')],
    "short", "Tokio tasks currently alive"))
panel_id += 1

panels.append(timeseries(panel_id, "Tokio Global Queue Depth", 16, y, 8, 6,
    [t('jmp_tokio_global_queue_depth{instance=~"$instance"}', '{{instance}}')],
    "short", "Tasks waiting in Tokio global queue"))
panel_id += 1

y += 6

# Write dashboard
dashboard_base["panels"] = panels

with open("/workspace/deploy/grafana-dashboard.json", "w") as f:
    json.dump(dashboard_base, f, indent=2)

print(f"Dashboard generated with {len(panels)} panels!")
