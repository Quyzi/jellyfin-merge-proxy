//! Upstream servers, request pipeline and streaming rows.

use super::*;

pub(super) fn upstreams(l: &mut Layout) {
    l.row("Upstream servers");
    let up_rate = |sel: &str| {
        format!("sum by (server) (rate(jmp_upstream_requests_total{{{sel}}}[$__range]))")
    };
    let all = up_rate(IS);
    let failed = up_rate(&format!(r#"{IS}, status_class=~"5xx|error""#));
    l.add(
        Panel::table(
            "Upstream summary",
            "One row per upstream. Health (UP, DOWN, or REMOVED for a server a reload removed or disabled), in-flight requests and token age are current; rate, p95 latency and failure share cover the selected range (failures = 5xx answers plus transport errors).",
            &[
                q(
                    format!(
                        "max by (server) {} or (max by (server) (jmp_upstream_configured{{{IS}}}) == 0) - 1",
                        upstream_up(IS)
                    ),
                    "",
                )
                .table(),
                q(all.clone(), "").table(),
                q(
                    format!("histogram_quantile(0.95, sum by (server, le) (rate(jmp_upstream_request_duration_seconds_bucket{{{IS}}}[$__range])))"),
                    "",
                )
                .table(),
                q(format!("({failed} or {all} * 0) / {all}"), "").table(),
                q(format!("sum by (server) (jmp_upstream_in_flight{{{IS}}})"), "").table(),
                q(format!("max by (server) (jmp_upstream_token_age_seconds{{{IS}}})"), "").table(),
                q(format!("sum by (server) (jmp_ids_table_size{{{IS}}})"), "").table(),
            ],
            "server",
            &[
                ("Health", "none"),
                ("Requests / s", "reqps"),
                ("p95 latency", "s"),
                ("Failures", "percentunit"),
                ("In flight", "short"),
                ("Token age", "s"),
                ("Mapped ids", "short"),
            ],
        )
        .column(
            "Health",
            json!([
                {"id": "mappings", "value": [map_value(&[("1", "UP", GOOD), ("0", "DOWN", BAD), ("-1", "REMOVED", MUTED)])]},
                {"id": "custom.cellOptions", "value": {"type": "color-background", "mode": "basic"}},
                {"id": "custom.align", "value": "center"},
                {"id": "custom.width", "value": 110}
            ]),
        )
        .column("Requests / s", json!([{"id": "custom.cellOptions", "value": {"type": "gauge", "mode": "gradient", "valueDisplayMode": "text"}}, {"id": "color", "value": {"mode": "continuous-BlPu"}}, {"id": "fieldMinMax", "value": true}]))
        .column("p95 latency", json!([{"id": "custom.cellOptions", "value": {"type": "color-text"}}, {"id": "thresholds", "value": steps(GOOD, &[(0.5, WARN), (2.0, BAD)])}]))
        .column("Failures", json!([{"id": "decimals", "value": 2}, {"id": "custom.cellOptions", "value": {"type": "color-text"}}, {"id": "thresholds", "value": steps(GOOD, &[(0.001, WARN), (0.05, BAD)])}]))
        .column("Mapped ids", json!([{"id": "decimals", "value": 0}]))
        .column("In flight", json!([{"id": "decimals", "value": 0}]))
        .size(24, 6),
    );
    l.add(
        Panel::timeseries(
            "Upstream p95 latency",
            "95th percentile upstream time to response headers, per server.",
            &[q(
                quantile(0.95, "jmp_upstream_request_duration_seconds", IS, "server"),
                "{{server}}",
            )],
        )
        .unit("s")
        .legend_table(&["mean", "max"], "right")
        .size(12, 8),
    );
    l.add(
        Panel::bargauge(
            "Requests per upstream (range)",
            "Requests sent to each upstream in the selected range.",
            q(
                format!(
                    "sum by (server) (increase(jmp_upstream_requests_total{{{IS}}}[$__range]))"
                ),
                "{{server}}",
            )
            .instant(),
        )
        .unit("short")
        .decimals(0)
        .size(6, 8),
    );
    l.add(
        Panel::bargauge(
            "Failed upstream requests (range)",
            "5xx answers plus transport errors per upstream in the selected range.",
            q(
                format!(r#"sum by (server) (increase(jmp_upstream_requests_total{{{IS}, status_class=~"5xx|error"}}[$__range])) or sum by (server) {} * 0"#, upstream_up(IS)),
                "{{server}}",
            )
            .instant(),
        )
        .unit("short")
        .decimals(0)
        .color_mode("thresholds")
        .thresholds(steps(GOOD, &[(1.0, WARN), (100.0, BAD)]))
        .option("displayMode", json!("basic"))
        .size(6, 8),
    );
    l.add(
        Panel::timeseries(
            "Upstream errors & logins",
            "Per interval: connection-level failures to upstreams by kind (connect, dns, tls, timeout, body, other), and the logins the proxy made to upstreams (startup, token expiry, re-auth after a 401). Empty while every upstream is reachable and tokens are valid.",
            &[
                q(format!("sum by (server, kind) (increase(jmp_upstream_errors_total{{{IS}}}[$__rate_interval]))"), "{{server}} {{kind}} error"),
                q(format!("sum by (server, outcome) (increase(jmp_upstream_auth_total{{{IS}}}[$__rate_interval]))"), "{{server}} login {{outcome}}"),
                q(format!("sum by (server) (increase(jmp_upstream_reauth_total{{{IS}}}[$__rate_interval]))"), "{{server}} re-auth"),
            ],
        )
        .bars()
        .stacked()
        .unit("short")
        .decimals(0)
        .overrides(vec![
            json!({"matcher": {"id": "byRegexp", "options": ".* error$"}, "properties": [{"id": "color", "value": {"mode": "fixed", "fixedColor": BAD}}]}),
            json!({"matcher": {"id": "byRegexp", "options": ".* login success$"}, "properties": [{"id": "color", "value": {"mode": "fixed", "fixedColor": INFO}}]}),
            json!({"matcher": {"id": "byRegexp", "options": ".* (login (failure|unreachable)|re-auth)$"}, "properties": [{"id": "color", "value": {"mode": "fixed", "fixedColor": WARN}}]}),
        ])
        .size(24, 6),
    );
}

pub(super) fn pipeline(l: &mut Layout) {
    l.row("Request pipeline");
    l.add(
        Panel::timeseries(
            "Proxy time per request, by stage",
            "Average time each internal pipeline stage adds per client request (auth, id_decode, fanout, rewrite, merge, dedupe, sort_page, serialize, views_build). The stack height is the proxy's own share of a request's latency.",
            &[q(
                format!(
                    "sum by (stage) ({}) / on() group_left() sum({})",
                    rate("jmp_stage_duration_seconds_sum", I),
                    rate("jmp_http_requests_total", I)
                ),
                "{{stage}}",
            )],
        )
        .stacked()
        .unit("s")
        .legend_table(&["mean", "max"], "right")
        .size(12, 8),
    );
    l.add(
        Panel::heatmap(
            "Fan-out: wait for the slowest upstream",
            "How long merged requests waited for their slowest upstream (histogram buckets).",
            q(format!("sum by (le) (increase(jmp_fanout_slowest_seconds_bucket{{{I}}}[$__rate_interval]))"), "{{le}}").heatmap(),
            "s",
        )
        .size(12, 8),
    );
    l.add(
        Panel::timeseries(
            "Fan-out results",
            "Per-upstream results of fan-out calls: success, http_error, timeout, error, skipped.",
            &[q(
                format!(
                    "sum by (outcome) ({})",
                    rate("jmp_fanout_upstream_results_total", IS)
                ),
                "{{outcome}}",
            )],
        )
        .stacked()
        .unit("reqps")
        .overrides(palette(&[
            ("success", GOOD),
            ("skipped", NEUTRAL),
            ("http_error", WARN),
            ("timeout", "yellow"),
            ("error", BAD),
        ]))
        .size(12, 7),
    );
    l.add(
        Panel::timeseries(
            "Partial fan-out responses",
            "Merged responses served with results from only some upstreams (one was down or failed), per interval and endpoint.",
            &[q(format!("sum by (endpoint) (increase(jmp_fanout_partial_total{{{I}}}[$__rate_interval]))"), "{{endpoint}}")],
        )
        .bars()
        .stacked()
        .unit("short")
        .decimals(0)
        .no_value("None")
        .size(12, 7),
    );
}

pub(super) fn streaming(l: &mut Layout) {
    l.row("Streaming");
    l.add(
        Panel::timeseries(
            "Throughput by upstream",
            "Bytes per second streamed to clients, stacked by the upstream that served them.",
            &[q(
                format!("sum by (server) ({})", rate("jmp_stream_bytes_total", IS)),
                "{{server}}",
            )],
        )
        .stacked()
        .unit("Bps")
        .legend_table(&["mean", "max"], "bottom")
        .size(12, 8),
    );
    l.add(
        Panel::timeseries(
            "Streams by kind",
            "Finished streams per second by kind: direct (static file), hls_segment, hls_playlist, transcode, image, download, web, other.",
            &[q(format!("sum by (kind) ({})", rate("jmp_stream_end_total", IS)), "{{kind}}")],
        )
        .stacked()
        .unit("reqps")
        .legend_table(&["mean", "max"], "bottom")
        .size(12, 8),
    );
    l.add(
        Panel::heatmap(
            "Time to first byte",
            "Time from the upstream request to the first body byte, all stream kinds (histogram buckets).",
            q(format!("sum by (le) (increase(jmp_stream_ttfb_seconds_bucket{{{IS}}}[$__rate_interval]))"), "{{le}}").heatmap(),
            "s",
        )
        .size(8, 8),
    );
    l.add(
        Panel::timeseries(
            "Stream outcomes",
            "How streams ended: complete, client_disconnect (player stopped or seeked away), upstream_error.",
            &[q(format!("sum by (outcome) ({})", rate("jmp_stream_end_total", IS)), "{{outcome}}")],
        )
        .stacked()
        .unit("reqps")
        .overrides(palette(&[("complete", GOOD), ("client_disconnect", "yellow"), ("upstream_error", BAD)]))
        .size(8, 8),
    );
    l.add(
        Panel::timeseries(
            "Seeking & HLS",
            "Range requests (seeking / resuming direct play), HLS segment requests and HLS playlists rewritten to point at the proxy.",
            &[
                q(format!("sum({})", rate("jmp_range_requests_total", IS)), "range requests"),
                q(format!("sum({})", rate("jmp_hls_segment_requests_total", IS)), "HLS segments"),
                q(format!("sum({})", rate("jmp_hls_playlist_rewrites_total", IS)), "HLS playlists"),
            ],
        )
        .unit("reqps")
        .overrides(palette(&[("range requests", INFO), ("HLS segments", ACCENT), ("HLS playlists", "yellow")]))
        .size(8, 8),
    );
}
