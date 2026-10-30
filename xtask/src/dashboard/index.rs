//! Catalog index row: crawl freshness, canonical contents and how library
//! queries were answered.

use super::*;

pub(super) fn index(l: &mut Layout) {
    l.row("Catalog index");
    l.add(
        Panel::stat(
            "Answered from the index",
            "Share of library queries (Items, Seasons, Episodes, Genres/Studios/Persons, Filters) answered from the index with exact totals and sorting. The rest went to live fan-out because the index was cold or the query used a parameter it cannot express.",
            q(
                format!(
                    "sum({}) / sum({})",
                    rate("jmp_index_queries_total", &format!(r#"{I}, outcome="index""#)),
                    rate("jmp_index_queries_total", I)
                ),
                "",
            ),
        )
        .unit("percentunit")
        .decimals(1)
        .no_value("No library queries")
        .thresholds(steps(WARN, &[(0.8, GOOD)]))
        .size(4, 8),
    );
    l.add(
        Panel::timeseries(
            "Library queries by outcome",
            "index: answered from the index; cold: a live server has never been crawled (live fan-out); unsupported: the query needs something the index does not hold (live fan-out).",
            &[q(
                format!("sum by (outcome) ({})", rate("jmp_index_queries_total", I)),
                "{{outcome}}",
            )],
        )
        .stacked()
        .unit("reqps")
        .overrides(palette(&[("index", GOOD), ("cold", WARN), ("unsupported", INFO)]))
        .size(10, 8),
    );
    l.add(
        Panel::timeseries(
            "Canonical catalog",
            "Items after grouping copies, items with copies on several servers, and genres/studios/people merged by name.",
            &[q(format!("max by (kind) (jmp_index_canonical{{{I}}})"), "{{kind}}")],
        )
        .unit("short")
        .decimals(0)
        .overrides(palette(&[("items", INFO), ("merged", GOOD), ("names", ACCENT)]))
        .size(10, 8),
    );
    l.add(
        Panel::table(
            "Index by upstream",
            "Items indexed per upstream and time since its last successful crawl (orange after 30 min, red after 2 h; the default refresh is 10 min).",
            &[
                q(format!("sum by (server) (jmp_index_items{{{IS}}})"), "").table(),
                q(format!("time() - max by (server) (jmp_index_last_sync_timestamp_seconds{{{IS}}})"), "").table(),
            ],
            "server",
            &[("Items", "short"), ("Since crawl", "s")],
        )
        .column("Items", json!([{"id": "decimals", "value": 0}]))
        .column("Since crawl", json!([{"id": "custom.cellOptions", "value": {"type": "color-text"}}, {"id": "decimals", "value": 0}, {"id": "thresholds", "value": steps(GOOD, &[(1800.0, WARN), (7200.0, BAD)])}]))
        .size(8, 7),
    );
    l.add(
        Panel::timeseries(
            "Crawls",
            "Index crawls per upstream per interval: success, error, skipped (server down; its last crawl is kept).",
            &[q(
                format!("sum by (server, outcome) (increase(jmp_index_syncs_total{{{IS}}}[$__rate_interval]))"),
                "{{server}} {{outcome}}",
            )],
        )
        .bars()
        .stacked()
        .unit("short")
        .decimals(0)
        .size(8, 7),
    );
    l.add(
        Panel::timeseries(
            "Crawl and rebuild time (p95)",
            "95th percentile duration of one upstream's crawl, and of rebuilding the merged snapshot (grouping, ranking, canonical ids).",
            &[
                q(quantile(0.95, "jmp_index_sync_duration_seconds", IS, "server"), "crawl {{server}}"),
                q(quantile(0.95, "jmp_index_build_duration_seconds", I, ""), "rebuild"),
            ],
        )
        .unit("s")
        .size(8, 7),
    );
}
