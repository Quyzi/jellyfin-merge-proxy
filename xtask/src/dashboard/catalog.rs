//! Merge/dedupe, catalog cache and clients & auth rows.

use super::*;

pub(super) fn merge(l: &mut Layout) {
    l.row("Merge, dedupe & ids");
    l.add(
        Panel::timeseries(
            "Merged lists: items in vs out",
            "Items received from upstreams for merged lists versus items returned to clients (after dedupe and paging), and duplicates removed.",
            &[
                q(format!("sum({})", rate("jmp_merge_items_in_total", I)), "from upstreams"),
                q(format!("sum({})", rate("jmp_merge_items_out_total", I)), "to clients"),
                q(format!("sum({})", rate("jmp_dedupe_dropped_total", I)), "duplicates removed"),
            ],
        )
        .unit("cps")
        .overrides(palette(&[("from upstreams", INFO), ("to clients", GOOD), ("duplicates removed", WARN)]))
        .size(6, 8),
    );
    l.add(
        Panel::timeseries(
            "Duplicates by match key",
            "Which key matched the removed duplicates: tmdb, imdb, tvdb, title, index (episode split across servers), name.",
            &[q(format!("sum by (key) ({})", rate("jmp_dedupe_dropped_total", I)), "{{key}}")],
        )
        .stacked()
        .unit("cps")
        .size(6, 8),
    );
    l.add(
        Panel::bargauge(
            "Dedupe winners (range)",
            "Which upstream's copy was kept when duplicates were merged (lowest priority wins), over the selected range.",
            q(format!("sum by (server) (increase(jmp_dedupe_winner_total{{{IS}}}[$__range]))"), "{{server}}").instant(),
        )
        .unit("short")
        .decimals(0)
        .size(6, 8),
    );
    l.add(
        Panel::timeseries(
            "Id lookups",
            "Proxy id lookups from client requests: hit, view (library view id), miss (unknown id, e.g. stale client cache), invalid (malformed). Table sizes are in the upstream summary.",
            &[q(format!("sum by (outcome) ({})", rate("jmp_ids_decoded_total", I)), "{{outcome}}")],
        )
        .stacked()
        .unit("reqps")
        .overrides(palette(&[("hit", GOOD), ("view", INFO), ("miss", WARN), ("invalid", BAD)]))
        .size(6, 8),
    );
}

pub(super) fn cache(l: &mut Layout) {
    l.row("Catalog cache");
    l.add(
        Panel::stat(
            "Cache hit ratio",
            "Share of catalog cache lookups answered from cache. Red under 50 %, orange under 80 %.",
            q(
                format!(
                    r#"sum({}) / sum({})"#,
                    rate("jmp_cache_requests_total", &format!(r#"{I}, outcome="hit""#)),
                    rate("jmp_cache_requests_total", I)
                ),
                "",
            ),
        )
        .unit("percentunit")
        .decimals(1)
        .no_value("Cache idle")
        .thresholds(steps(BAD, &[(0.5, WARN), (0.8, GOOD)]))
        .size(4, 8),
    );
    l.add(
        Panel::timeseries(
            "Cache lookups",
            "Catalog cache lookups per second: hit, miss, stale (expired or forced refresh).",
            &[q(
                format!("sum by (outcome) ({})", rate("jmp_cache_requests_total", I)),
                "{{outcome}}",
            )],
        )
        .stacked()
        .unit("reqps")
        .overrides(palette(&[
            ("hit", GOOD),
            ("miss", WARN),
            ("stale", "yellow"),
        ]))
        .size(10, 8),
    );
    l.add(
        Panel::timeseries(
            "Catalog refresh time (p95)",
            "95th percentile duration of catalog refresh calls to each upstream.",
            &[q(
                quantile(0.95, "jmp_cache_refresh_duration_seconds", IS, "server"),
                "{{server}}",
            )],
        )
        .unit("s")
        .size(10, 8),
    );
    l.add(
        Panel::table(
            "Cache by upstream",
            "Catalog cache per upstream: entries held, approximate size, and time since the last successful refresh (orange after 10 min, red after 1 h).",
            &[
                q(format!("sum by (server) (jmp_cache_entries{{{IS}}})"), "").table(),
                q(format!("sum by (server) (jmp_cache_bytes{{{IS}}})"), "").table(),
                q(format!("time() - max by (server) (jmp_cache_last_refresh_timestamp_seconds{{{IS}}})"), "").table(),
            ],
            "server",
            &[("Entries", "short"), ("Size", "bytes"), ("Since refresh", "s")],
        )
        .column("Entries", json!([{"id": "decimals", "value": 0}]))
        .column("Size", json!([{"id": "custom.cellOptions", "value": {"type": "gauge", "mode": "gradient", "valueDisplayMode": "text"}}, {"id": "color", "value": {"mode": "continuous-BlPu"}}]))
        .column("Since refresh", json!([{"id": "custom.cellOptions", "value": {"type": "color-text"}}, {"id": "decimals", "value": 0}, {"id": "thresholds", "value": steps(GOOD, &[(600.0, WARN), (3600.0, BAD)])}]))
        .size(12, 6),
    );
    l.add(
        Panel::timeseries(
            "Refresh failures & evictions",
            "Bars: failed catalog refreshes per upstream per interval (left axis). Line: cache entries evicted per second (right axis); steady evictions are normal TTL churn.",
            &[
                q(format!("sum by (server) (increase(jmp_cache_refresh_failures_total{{{IS}}}[$__rate_interval]))"), "{{server}} failed"),
                q(format!("sum({})", rate("jmp_cache_evictions_total", I)), "evictions / s"),
            ],
        )
        .bars()
        .unit("short")
        .decimals(0)
        .overrides(vec![json!({"matcher": {"id": "byName", "options": "evictions / s"}, "properties": [
            {"id": "color", "value": {"mode": "fixed", "fixedColor": NEUTRAL}},
            {"id": "custom.drawStyle", "value": "line"},
            {"id": "custom.fillOpacity", "value": 0},
            {"id": "custom.axisPlacement", "value": "right"},
            {"id": "unit", "value": "cps"},
            {"id": "decimals", "value": 1}
        ]})])
        .fixed_color(BAD)
        .size(12, 6),
    );
}

pub(super) fn auth(l: &mut Layout) {
    l.row("Clients & auth");
    l.add(
        Panel::timeseries(
            "Proxy logins",
            "Client logins to the proxy per interval: success, bad_password, unknown_user, bad_request. Bursts of failures may mean password guessing.",
            &[q(format!("sum by (outcome) (increase(jmp_logins_total{{{I}}}[$__rate_interval]))"), "{{outcome}}")],
        )
        .bars()
        .stacked()
        .unit("short")
        .decimals(0)
        .overrides(palette(&[("success", GOOD), ("bad_password", WARN), ("unknown_user", BAD), ("bad_request", ACCENT)]))
        .size(8, 7),
    );
    l.add(
        Panel::timeseries(
            "Client token checks",
            "Access-token checks on client requests: valid, invalid (unknown/expired token), missing, anonymous (public routes).",
            &[q(format!("sum by (outcome) ({})", rate("jmp_token_validations_total", I)), "{{outcome}}")],
        )
        .stacked()
        .unit("reqps")
        .overrides(palette(&[("valid", GOOD), ("anonymous", NEUTRAL), ("missing", "yellow"), ("invalid", BAD)]))
        .size(8, 7),
    );
    l.add(
        Panel::timeseries(
            "Password hashing (argon2 p95)",
            "95th percentile argon2 time per operation (hash at config load, verify at login). High values slow logins.",
            &[q(quantile(0.95, "jmp_argon2_duration_seconds", I, "op"), "{{op}}")],
        )
        .unit("s")
        .size(8, 7),
    );
}
