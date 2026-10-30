//! Checks that the dashboard and the Prometheus alert rules only use metrics
//! and labels the proxy really exports (from `jmp::telemetry::METRICS`), and
//! that the dashboard layout is sane.

use jmp::telemetry::{Kind, METRICS};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

/// process_* series exported by the metrics-process crate (process_metrics = true).
const PROCESS_METRICS: &[&str] = &[
    "process_cpu_seconds_total",
    "process_resident_memory_bytes",
    "process_virtual_memory_bytes",
    "process_virtual_memory_max_bytes",
    "process_open_fds",
    "process_max_fds",
    "process_start_time_seconds",
    "process_threads",
];
/// Labels every series has (added by Prometheus) or that histograms carry.
const GENERIC_LABELS: &[&str] = &["instance", "job", "le"];
/// Labels created inside queries with label_replace.
const SYNTHETIC_LABELS: &[&str] = &["class"];

/// metric series name -> its labels.
pub fn catalogue() -> HashMap<String, BTreeSet<String>> {
    let mut m = HashMap::new();
    for def in METRICS {
        let labels: BTreeSet<String> = def
            .labels
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        m.insert(def.name.to_string(), labels.clone());
        if def.kind == Kind::Histogram {
            for suffix in ["_bucket", "_sum", "_count"] {
                m.insert(format!("{}{suffix}", def.name), labels.clone());
            }
        }
    }
    for p in PROCESS_METRICS {
        m.insert(p.to_string(), BTreeSet::new());
    }
    m
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == ':'
}

/// Metric names (jmp_* / process_*) referenced by a PromQL expression.
pub fn metrics_in(expr: &str) -> BTreeSet<String> {
    expr.split(|c: char| !is_ident(c))
        .filter(|w| w.starts_with("jmp_") || w.starts_with("process_"))
        .map(str::to_string)
        .collect()
}

/// Label names used in matchers `{a="..", b=~".."}` and in `by (..)` / `on (..)` / `without (..)`.
pub fn labels_in(expr: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let b = expr.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            // matcher block: label names are the identifiers before =, =~, !=, !~
            let end = expr[i..].find('}').map(|e| i + e).unwrap_or(b.len());
            let mut in_str = false;
            let mut cur = String::new();
            for c in expr[i + 1..end].chars() {
                if c == '"' {
                    in_str = !in_str;
                    continue;
                }
                if in_str {
                    continue;
                }
                if is_ident(c) {
                    cur.push(c);
                } else {
                    if (c == '=' || c == '!') && !cur.is_empty() {
                        out.insert(cur.clone());
                    }
                    if c != ' ' {
                        cur.clear();
                    }
                }
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    for kw in ["by", "on", "without", "group_left", "group_right"] {
        let mut rest = expr;
        while let Some(pos) = rest.find(kw) {
            let before_ok = pos == 0 || !is_ident(rest[..pos].chars().last().unwrap_or(' '));
            let after = rest[pos + kw.len()..].trim_start();
            if before_ok && after.starts_with('(') {
                if let Some(close) = after.find(')') {
                    for l in after[1..close].split(',') {
                        let l = l.trim();
                        if !l.is_empty() {
                            out.insert(l.to_string());
                        }
                    }
                }
            }
            rest = &rest[pos + kw.len()..];
        }
    }
    out
}

/// `{{label}}` placeholders in a legend format.
pub fn legend_labels(legend: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = legend;
    while let Some(s) = rest.find("{{") {
        let after = &rest[s + 2..];
        if let Some(e) = after.find("}}") {
            out.insert(after[..e].trim().to_string());
            rest = &after[e + 2..];
        } else {
            break;
        }
    }
    out
}

/// Problems with one PromQL expression (+ optional legend), empty when fine.
pub fn check_expr(
    cat: &HashMap<String, BTreeSet<String>>,
    expr: &str,
    legend: &str,
) -> Vec<String> {
    let mut errs = Vec::new();
    let metrics = metrics_in(expr);
    let mut known_labels: HashSet<&str> = GENERIC_LABELS
        .iter()
        .chain(SYNTHETIC_LABELS)
        .copied()
        .collect();
    for m in &metrics {
        match cat.get(m) {
            Some(ls) => known_labels.extend(ls.iter().map(String::as_str)),
            None => errs.push(format!("unknown metric `{m}`")),
        }
    }
    // A [[server]] removed or disabled by a reload keeps its last jmp_upstream_up
    // sample (metrics cannot be unregistered); only the configured join hides it.
    if metrics.iter().any(|m| m == "jmp_upstream_up")
        && !expr.contains("and on(instance, server) jmp_upstream_configured")
    {
        errs.push(
            "`jmp_upstream_up` must be limited to configured servers: \
             `and on(instance, server) jmp_upstream_configured{...} == 1`"
                .into(),
        );
    }
    let mut labels = labels_in(expr);
    labels.extend(legend_labels(legend));
    for l in labels {
        if !known_labels.contains(l.as_str()) {
            errs.push(format!("label `{l}` is not exported by {metrics:?}"));
        }
    }
    errs
}

fn panels_flat(dash: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    for p in dash["panels"].as_array().into_iter().flatten() {
        out.push(p);
        for c in p["panels"].as_array().into_iter().flatten() {
            out.push(c);
        }
    }
    out
}

/// Every problem found in the dashboard JSON.
pub fn check_dashboard(dash: &Value) -> Vec<String> {
    let cat = catalogue();
    let mut errs = Vec::new();
    let mut ids = HashSet::new();
    let mut cells: HashMap<(u64, u64), String> = HashMap::new();
    for p in panels_flat(dash) {
        let title = p["title"].as_str().unwrap_or("?").to_string();
        let kind = p["type"].as_str().unwrap_or("");
        let id = p["id"].as_u64().unwrap_or(0);
        if !ids.insert(id) {
            errs.push(format!("{title}: duplicate panel id {id}"));
        }
        let g = &p["gridPos"];
        let (x, y, w, h) = (
            g["x"].as_u64().unwrap_or(99),
            g["y"].as_u64().unwrap_or(0),
            g["w"].as_u64().unwrap_or(0),
            g["h"].as_u64().unwrap_or(0),
        );
        if w == 0 || h == 0 || x + w > 24 {
            errs.push(format!("{title}: bad gridPos {g}"));
        }
        for cx in x..(x + w).min(24) {
            for cy in y..y + h {
                if let Some(other) = cells.insert((cx, cy), title.clone()) {
                    errs.push(format!("{title}: overlaps {other} at ({cx},{cy})"));
                    break;
                }
            }
        }
        if kind == "row" {
            continue;
        }
        if p["description"].as_str().is_none_or(str::is_empty) {
            errs.push(format!("{title}: missing description"));
        }
        if p["datasource"]["uid"] != "${datasource}" {
            errs.push(format!("{title}: datasource must be ${{datasource}}"));
        }
        let defaults = &p["fieldConfig"]["defaults"];
        if defaults["color"]["mode"] == "fixed" && !defaults["color"]["fixedColor"].is_string() {
            errs.push(format!("{title}: fixed color mode without fixedColor"));
        }
        let has_unit = defaults["unit"].is_string() || p["options"]["yAxis"]["unit"].is_string();
        let unit_needed = matches!(kind, "timeseries" | "bargauge" | "heatmap")
            || (kind == "stat"
                && p["options"]["textMode"] != "name"
                && defaults["mappings"].as_array().is_none_or(Vec::is_empty)
                && defaults["color"]["mode"] != "fixed");
        if unit_needed && !has_unit {
            errs.push(format!("{title}: no unit"));
        }
        let targets = p["targets"].as_array().cloned().unwrap_or_default();
        if targets.is_empty() {
            errs.push(format!("{title}: no queries"));
        }
        for t in targets {
            let expr = t["expr"].as_str().unwrap_or("");
            let legend = t["legendFormat"].as_str().unwrap_or("");
            for e in check_expr(&cat, expr, legend) {
                errs.push(format!("{title}: {e}"));
            }
            if expr.contains('[')
                && !(expr.contains("[$__rate_interval]") || expr.contains("[$__range]"))
            {
                errs.push(format!(
                    "{title}: use $__rate_interval or $__range windows: {expr}"
                ));
            }
            if !expr.contains("$instance") {
                errs.push(format!("{title}: query ignores $instance: {expr}"));
            }
        }
    }
    for a in dash["annotations"]["list"].as_array().into_iter().flatten() {
        if let Some(expr) = a["expr"].as_str() {
            for e in check_expr(&cat, expr, a["titleFormat"].as_str().unwrap_or("")) {
                errs.push(format!("annotation {}: {e}", a["name"]));
            }
        }
    }
    errs
}

/// PromQL expressions of a Prometheus rules file (`expr:` scalars and `|` blocks).
pub fn alert_exprs(yaml: &str) -> Vec<String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(pos) = line.find("expr:") {
            let indent = line.len() - line.trim_start().len();
            let val = line[pos + 5..].trim();
            if val == "|" || val == ">" || val == "|-" || val == ">-" {
                let mut block = Vec::new();
                i += 1;
                while i < lines.len()
                    && (lines[i].trim().is_empty()
                        || lines[i].len() - lines[i].trim_start().len() > indent)
                {
                    block.push(lines[i].trim());
                    i += 1;
                }
                out.push(block.join(" ").trim().to_string());
                continue;
            }
            out.push(val.trim_matches('\'').trim_matches('"').to_string());
        }
        i += 1;
    }
    out
}

pub fn check_alerts(yaml: &str) -> Vec<String> {
    let cat = catalogue();
    let mut errs = Vec::new();
    let exprs = alert_exprs(yaml);
    if exprs.is_empty() {
        errs.push("no alert expressions found".into());
    }
    for e in exprs {
        for p in check_expr(&cat, &e, "") {
            errs.push(format!("alert `{e}`: {p}"));
        }
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_labels() {
        let l = labels_in(
            r#"sum by (server, kind) (rate(jmp_upstream_errors_total{instance=~"$instance", server!="a,b"}[5m])) / on() group_left() x"#,
        );
        assert_eq!(
            l.into_iter().collect::<Vec<_>>(),
            vec!["instance", "kind", "server"]
        );
        assert!(legend_labels("{{server}} · {{ kind }}").contains("kind"));
    }

    #[test]
    fn flags_unknown_metric_and_label() {
        let cat = catalogue();
        assert!(check_expr(&cat, "rate(jmp_http_requests_total[1m])", "{{route}}").is_empty());
        assert!(!check_expr(&cat, "rate(jmp_nope_total[1m])", "").is_empty());
        assert!(!check_expr(&cat, "sum by (server) (jmp_http_requests_total)", "").is_empty());
        assert!(check_expr(
            &cat,
            "histogram_quantile(0.9, sum by (le) (rate(jmp_stage_duration_seconds_bucket[1m])))",
            ""
        )
        .is_empty());
        // _bucket only exists for histograms
        assert!(!check_expr(&cat, "jmp_http_requests_total_bucket", "").is_empty());
    }

    #[test]
    fn upstream_up_must_skip_removed_servers() {
        let cat = catalogue();
        assert_eq!(check_expr(&cat, "count(jmp_upstream_up == 0)", "").len(), 1);
        assert_eq!(
            check_expr(&cat, "max by (instance, server) (jmp_upstream_up * on(server) jmp_upstream_configured) == 0", "").len(),
            1,
            "up * configured reads a removed server as down"
        );
        assert!(check_expr(
            &cat,
            "count((jmp_upstream_up and on(instance, server) jmp_upstream_configured == 1) == 0)",
            ""
        )
        .is_empty());
    }

    #[test]
    fn reads_alert_blocks() {
        let y = "rules:\n  - alert: A\n    expr: |\n      sum(rate(jmp_http_requests_total[5m]))\n      > 1\n    for: 1m\n  - alert: B\n    expr: jmp_upstream_up == 0\n";
        assert_eq!(
            alert_exprs(y),
            vec![
                "sum(rate(jmp_http_requests_total[5m])) > 1",
                "jmp_upstream_up == 0"
            ]
        );
    }
}
