//! Configuration & TLS and process rows.

use super::*;

pub(super) fn config_tls(l: &mut Layout) {
    l.row("Configuration & TLS");
    l.add(
        Panel::stat(
            "Version",
            "Proxy version and git commit (jmp_build_info).",
            q(
                format!("max by (version, git_sha) (jmp_build_info{{{I}}})"),
                "{{version}} · {{git_sha}}",
            )
            .instant(),
        )
        .option("textMode", json!("name"))
        .fixed_color(NEUTRAL)
        .color_mode("fixed")
        .text_colored()
        .no_graph()
        .size(6, 4),
    );
    l.add(
        Panel::stat(
            "Uptime",
            "Time since the proxy process started.",
            q(format!("max(jmp_uptime_seconds{{{I}}})"), ""),
        )
        .unit("dtdurations")
        .fixed_color(NEUTRAL)
        .color_mode("fixed")
        .text_colored()
        .no_graph()
        .size(6, 4),
    );
    l.add(
        Panel::stat(
            "Config applied",
            "When the current config was (re)loaded.",
            q(
                format!("max(jmp_config_last_reload_timestamp_seconds{{{I}}}) * 1000"),
                "",
            ),
        )
        .unit("dateTimeFromNow")
        .fixed_color(NEUTRAL)
        .color_mode("fixed")
        .text_colored()
        .no_graph()
        .size(6, 4),
    );
    l.add(
        Panel::stat(
            "Upstreams configured",
            "Enabled [[server]] entries in the config.",
            q(format!("max(jmp_config_upstreams{{{I}}})"), ""),
        )
        .fixed_color(NEUTRAL)
        .color_mode("fixed")
        .text_colored()
        .size(6, 4),
    );
    l.add(
        Panel::timeseries(
            "Config reloads",
            "Config reload attempts per interval: success, unchanged (file touched, same content), error (invalid file: the previous config stays active).",
            &[q(format!("sum by (outcome) (increase(jmp_config_reloads_total{{{I}}}[$__rate_interval]))"), "{{outcome}}")],
        )
        .bars()
        .stacked()
        .unit("short")
        .decimals(0)
        .overrides(palette(&[("success", GOOD), ("unchanged", NEUTRAL), ("error", BAD)]))
        .size(8, 7),
    );
    l.add(
        Panel::timeseries(
            "Upstream changes on reload",
            "[[server]] changes applied by hot reloads: added, removed, rebuilt (URL changed), changed (TLS/credentials/timeout in place).",
            &[q(format!("sum by (change) (increase(jmp_config_upstream_changes_total{{{I}}}[$__rate_interval]))"), "{{change}}")],
        )
        .bars()
        .stacked()
        .unit("short")
        .decimals(0)
        .overrides(palette(&[("added", GOOD), ("removed", BAD), ("rebuilt", WARN), ("changed", INFO)]))
        .size(8, 7),
    );
    l.add(
        Panel::timeseries(
            "Backends",
            "Backends serving requests, removed/rebuilt backends still draining in-flight streams, and requests for ids of removed servers (410 Gone).",
            &[
                q(format!("max(jmp_servers_active{{{I}}})"), "active"),
                q(format!("max(jmp_servers_draining{{{I}}})"), "draining"),
                q(format!("sum(increase(jmp_removed_server_requests_total{{{I}}}[$__rate_interval]))"), "410 Gone"),
            ],
        )
        .unit("short")
        .decimals(0)
        .overrides(palette(&[("active", GOOD), ("draining", WARN), ("410 Gone", BAD)]))
        .size(8, 7),
    );
    l.add(
        Panel::table(
            "TLS certificates",
            "Every certificate the proxy has loaded (listener, upstream:<name> client certificates) with its expiry. Days left: orange under 14, red under 7.",
            &[
                q(format!("max by (cert) (jmp_tls_cert_expiry_timestamp_seconds{{{I}}}) * 1000"), "").table(),
                q(format!("(max by (cert) (jmp_tls_cert_expiry_timestamp_seconds{{{I}}}) - time()) / 86400"), "").table(),
            ],
            "cert",
            &[("Expires", "dateTimeAsIso"), ("Days left", "suffix: d")],
        )
        .column(
            "Days left",
            json!([
                {"id": "custom.cellOptions", "value": {"type": "gauge", "mode": "basic", "valueDisplayMode": "text"}},
                {"id": "decimals", "value": 0},
                {"id": "min", "value": 0}, {"id": "max", "value": 90},
                {"id": "thresholds", "value": steps(BAD, &[(7.0, WARN), (14.0, GOOD)])}
            ]),
        )
        .column("Cert", json!([{"id": "custom.width", "value": 220}]))
        .sort_by("Days left", false)
        .no_value("No TLS certificates loaded")
        .size(12, 6),
    );
    l.add(
        Panel::timeseries(
            "TLS reloads",
            "Certificate/key/CA reloads per interval by target (listener, upstream) and outcome. Errors keep the previous certificate.",
            &[q(format!("sum by (target, outcome) (increase(jmp_tls_reloads_total{{{I}}}[$__rate_interval]))"), "{{target}} {{outcome}}")],
        )
        .bars()
        .stacked()
        .unit("short")
        .decimals(0)
        .overrides(palette(&[("listener success", GOOD), ("upstream success", INFO), ("listener error", BAD), ("upstream error", "dark-red")]))
        .size(12, 6),
    );
}

pub(super) fn process(l: &mut Layout) {
    l.row("Process");
    l.add(
        Panel::timeseries(
            "CPU",
            "CPU time used by the proxy process (1 = one full core).",
            &[q(
                format!("sum({})", rate("process_cpu_seconds_total", I)),
                "cpu",
            )],
        )
        .unit("percentunit")
        .fixed_color(INFO)
        .color_mode("fixed")
        .size(6, 7),
    );
    l.add(
        Panel::timeseries(
            "Memory",
            "Resident memory of the proxy process.",
            &[q(
                format!("sum(process_resident_memory_bytes{{{I}}})"),
                "resident",
            )],
        )
        .unit("bytes")
        .fixed_color(ACCENT)
        .color_mode("fixed")
        .size(6, 7),
    );
    l.add(
        Panel::timeseries(
            "Open file descriptors",
            "Open file descriptors (sockets, files) of the proxy process.",
            &[q(format!("sum(process_open_fds{{{I}}})"), "open fds")],
        )
        .unit("short")
        .fixed_color("yellow")
        .color_mode("fixed")
        .size(6, 7),
    );
    l.add(
        Panel::timeseries(
            "Tokio runtime",
            "Alive async tasks and tasks waiting in the global queue (a growing queue means the runtime is saturated).",
            &[
                q(format!("sum(jmp_tokio_alive_tasks{{{I}}})"), "alive tasks"),
                q(format!("sum(jmp_tokio_global_queue_depth{{{I}}})"), "global queue"),
            ],
        )
        .unit("short")
        .overrides(palette(&[("alive tasks", GOOD), ("global queue", BAD)]))
        .size(6, 7),
    );
}
