//! Overview and client traffic rows.

use super::*;

pub(super) fn overview(l: &mut Layout) {
    l.row("Overview");
    let total = format!("sum({})", rate("jmp_http_requests_total", I));
    l.add(
        Panel::stat(
            "Requests / s",
            "Client HTTP requests per second handled by the proxy (all routes).",
            q(total.clone(), ""),
        )
        .unit("reqps")
        .fixed_color(INFO)
        .color_mode("fixed"),
    );
    l.add(
        Panel::stat(
            "5xx ratio",
            "Share of client responses with a 5xx status. 0 when nothing failed. Orange from 1 %, red from 5 %.",
            q(
                format!("(sum({}) or vector(0)) / {total}", rate("jmp_http_requests_total", &format!(r#"{I}, status=~"5..""#))),
                "",
            ),
        )
        .unit("percentunit")
        .decimals(2)
        .thresholds(steps(GOOD, &[(0.01, WARN), (0.05, BAD)])),
    );
    l.add(
        Panel::stat(
            "p95 latency",
            "95th percentile time until response headers, all routes (for streams this excludes the body transfer). Orange from 500 ms, red from 2 s.",
            q(quantile(0.95, "jmp_http_request_duration_seconds", I, ""), ""),
        )
        .unit("s")
        .thresholds(steps(GOOD, &[(0.5, WARN), (2.0, BAD)])),
    );
    l.add(
        Panel::stat(
            "Upstreams down",
            "Configured upstream servers currently in back-off (failed health checks). Servers removed or disabled by a reload are not counted. Red when any is down.",
            q(
                format!("count({} == 0) or vector(0)", upstream_up(IS)),
                "",
            ),
        )
        .thresholds(steps(GOOD, &[(1.0, BAD)]))
        .mappings(vec![map_value(&[("0", "All up", GOOD)])]),
    );
    l.add(
        Panel::stat(
            "Active streams",
            "Response bodies being streamed right now (video, HLS, images, downloads).",
            q(format!("sum(jmp_streams_active{{{IS}}}) or vector(0)"), ""),
        )
        .fixed_color(ACCENT)
        .color_mode("fixed"),
    );
    l.add(
        Panel::stat(
            "Stream throughput",
            "Bytes per second streamed from upstreams to clients.",
            q(
                format!("sum({}) or vector(0)", rate("jmp_stream_bytes_total", IS)),
                "",
            ),
        )
        .unit("Bps")
        .fixed_color(ACCENT)
        .color_mode("fixed"),
    );
    l.add(
        Panel::stat(
            "Client sessions",
            "Proxy login sessions (issued access tokens).",
            q(format!("sum(jmp_sessions_active{{{I}}})"), ""),
        )
        .fixed_color(INFO)
        .color_mode("fixed"),
    );
    l.add(
        Panel::stat(
            "Next cert expiry",
            "Days until the first loaded TLS certificate (listener or upstream client/CA) expires. Orange under 14 days, red under 7. Empty when TLS is not used.",
            q(format!("(min(jmp_tls_cert_expiry_timestamp_seconds{{{I}}}) - time()) / 86400"), ""),
        )
        .unit("suffix: days")
        .decimals(0)
        .no_value("No TLS")
        .no_graph()
        .thresholds(steps(BAD, &[(7.0, WARN), (14.0, GOOD)])),
    );

    l.add(
        Panel::state_timeline(
            "Upstream health",
            "Health of each configured upstream over time: green = healthy, red = in back-off after failed checks. A server removed or disabled by a reload stops at the reload (no bar), it is not shown as down.",
            &[q(format!("max by (server) {}", upstream_up(IS)), "{{server}}")],
        )
        .mappings(vec![map_value(&[("1", "Up", GOOD), ("0", "Down", BAD)])])
        .thresholds(steps(BAD, &[(0.5, GOOD)]))
        .size(12, 7),
    );
    l.add(
        Panel::timeseries(
            "Responses by status class",
            "Client responses per second, stacked by HTTP status class.",
            &[q(
                format!(
                    r#"sum by (class) (label_replace({}, "class", "${{1}}xx", "status", "([0-9]).."))"#,
                    rate("jmp_http_requests_total", I)
                ),
                "{{class}}",
            )],
        )
        .stacked()
        .unit("reqps")
        .overrides(status_palette())
        .size(12, 7),
    );
}

pub(super) fn traffic(l: &mut Layout) {
    l.row("Client traffic");
    l.add(
        Panel::timeseries(
            "Top routes",
            "Request rate of the 8 busiest route templates over the selected range.",
            &[q(
                format!(
                    "sum by (route) ({}) and on(route) topk(8, sum by (route) (increase(jmp_http_requests_total{{{I}}}[$__range] @ end())))",
                    rate("jmp_http_requests_total", I)
                ),
                "{{route}}",
            )],
        )
        .unit("reqps")
        .legend_table(&["mean", "max"], "right")
        .size(12, 9),
    );
    l.add(
        Panel::heatmap(
            "Latency distribution",
            "Client request latency (time to response headers) as a heatmap of histogram buckets: darker = more requests.",
            q(format!("sum by (le) (increase(jmp_http_request_duration_seconds_bucket{{{I}}}[$__rate_interval]))"), "{{le}}").heatmap(),
            "s",
        )
        .size(12, 9),
    );
    l.add(
        Panel::timeseries(
            "Latency percentiles",
            "p50 / p90 / p99 client latency (time to response headers).",
            &[
                q(
                    quantile(0.5, "jmp_http_request_duration_seconds", I, ""),
                    "p50",
                ),
                q(
                    quantile(0.9, "jmp_http_request_duration_seconds", I, ""),
                    "p90",
                ),
                q(
                    quantile(0.99, "jmp_http_request_duration_seconds", I, ""),
                    "p99",
                ),
            ],
        )
        .unit("s")
        .overrides(palette(&[("p50", GOOD), ("p90", "yellow"), ("p99", BAD)]))
        .size(8, 8),
    );
    l.add(
        Panel::timeseries(
            "Proxy errors by kind",
            "Requests the proxy itself failed, by error kind (unauthorized, not_found, upstream_unavailable, upstream_error, bad_request, payload_too_large).",
            &[q(format!("sum by (kind) ({})", rate("jmp_http_errors_total", I)), "{{kind}}")],
        )
        .stacked()
        .unit("reqps")
        .overrides(palette(&[
            ("not_found", "yellow"),
            ("unauthorized", WARN),
            ("upstream_unavailable", BAD),
            ("upstream_error", "dark-red"),
            ("bad_request", ACCENT),
            ("payload_too_large", INFO),
        ]))
        .size(8, 8),
    );
    l.add(
        Panel::timeseries(
            "Concurrency",
            "Client requests waiting for response headers, and requests pending on each upstream.",
            &[
                q(
                    format!("sum(jmp_http_requests_in_flight{{{I}}})"),
                    "client requests",
                ),
                q(
                    format!("sum by (server) (jmp_upstream_in_flight{{{IS}}})"),
                    "upstream {{server}}",
                ),
            ],
        )
        .unit("short")
        .decimals(0)
        .overrides(vec![color_by_name("client requests", NEUTRAL)])
        .size(8, 8),
    );
    let route_rate =
        |sel: &str| format!("sum by (route) (rate(jmp_http_requests_total{{{sel}}}[$__range]))");
    let all = route_rate(I);
    let ratio = |status: &str| {
        let bad = route_rate(&format!(r#"{I}, status=~"{status}""#));
        format!("({bad} or {all} * 0) / {all}")
    };
    l.add(
        Panel::table(
            "Routes (selected range)",
            "Per route template over the selected time range: average request rate, p95 latency and the share of 4xx / 5xx responses.",
            &[
                q(all.clone(), "").table(),
                q(
                    format!("histogram_quantile(0.95, sum by (route, le) (rate(jmp_http_request_duration_seconds_bucket{{{I}}}[$__range])))"),
                    "",
                )
                .table(),
                q(ratio("4.."), "").table(),
                q(ratio("5.."), "").table(),
            ],
            "route",
            &[("Requests / s", "reqps"), ("p95 latency", "s"), ("4xx", "percentunit"), ("5xx", "percentunit")],
        )
        .column("Requests / s", json!([{"id": "custom.cellOptions", "value": {"type": "gauge", "mode": "gradient", "valueDisplayMode": "text"}}, {"id": "color", "value": {"mode": "continuous-BlPu"}}, {"id": "fieldMinMax", "value": true}]))
        .column("p95 latency", json!([{"id": "custom.cellOptions", "value": {"type": "color-text"}}, {"id": "thresholds", "value": steps(GOOD, &[(0.5, WARN), (2.0, BAD)])}]))
        .column("4xx", json!([{"id": "custom.cellOptions", "value": {"type": "color-text"}}, {"id": "thresholds", "value": steps(NEUTRAL, &[(0.1, WARN)])}]))
        .column("5xx", json!([{"id": "custom.cellOptions", "value": {"type": "color-text"}}, {"id": "thresholds", "value": steps(GOOD, &[(0.001, WARN), (0.05, BAD)])}]))
        .sort_by("Requests / s", true)
        .size(24, 9),
    );
}
