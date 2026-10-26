use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(|s| s.as_str()).unwrap_or("help");

    match command {
        "dashboard" => generate_dashboard(),
        "validate" => validate_dashboard(),
        _ => {
            println!("Usage: cargo xtask <command>");
            println!("Commands:");
            println!("  dashboard  Generate deploy/grafana-dashboard.json");
            println!("  validate   Validate dashboard references existing metrics");
            Ok(())
        }
    }
}

fn generate_dashboard() -> Result<()> {
    println!("Generating Grafana dashboard...");
    
    let dashboard = build_dashboard();
    let json = serde_json::to_string_pretty(&dashboard)?;
    
    fs::write("deploy/grafana-dashboard.json", json)
        .context("Failed to write dashboard JSON")?;
    
    println!("✓ Generated deploy/grafana-dashboard.json");
    Ok(())
}

fn validate_dashboard() -> Result<()> {
    println!("Validating dashboard...");
    
    // Read known metrics from telemetry module
    let telemetry_source = fs::read_to_string("src/telemetry.rs")
        .context("Failed to read src/telemetry.rs")?;
    let known_metrics = extract_metric_names(&telemetry_source);
    
    println!("  Found {} known metrics in telemetry.rs", known_metrics.len());
    
    // Read dashboard
    let dashboard_json = fs::read_to_string("deploy/grafana-dashboard.json")
        .context("Failed to read dashboard")?;
    
    // Extract all metric references from dashboard
    let referenced_metrics = extract_dashboard_metrics(&dashboard_json);
    println!("  Found {} metric references in dashboard", referenced_metrics.len());
    
    // Check for unknown metrics
    let mut unknown = Vec::new();
    for metric in &referenced_metrics {
        if !is_valid_metric(metric, &known_metrics) {
            unknown.push(metric);
        }
    }
    
    if !unknown.is_empty() {
        anyhow::bail!(
            "Dashboard references {} unknown metrics: {:?}",
            unknown.len(),
            unknown
        );
    }
    
    // Read alert rules
    let alerts_path = Path::new("deploy/prometheus/alerts.yml");
    if alerts_path.exists() {
        let alerts_yml = fs::read_to_string(alerts_path)?;
        let alert_metrics = extract_dashboard_metrics(&alerts_yml);
        println!("  Found {} metric references in alerts", alert_metrics.len());
        
        let mut unknown_alerts = Vec::new();
        for metric in &alert_metrics {
            if !is_valid_metric(metric, &known_metrics) {
                unknown_alerts.push(metric);
            }
        }
        
        if !unknown_alerts.is_empty() {
            anyhow::bail!(
                "Alerts reference {} unknown metrics: {:?}",
                unknown_alerts.len(),
                unknown_alerts
            );
        }
    }
    
    println!("✓ All dashboard and alert metrics are valid");
    Ok(())
}

fn is_valid_metric(metric: &str, known_metrics: &HashSet<String>) -> bool {
    // Allow process_ metrics from metrics-process crate
    if metric.starts_with("process_") {
        return true;
    }
    
    // Allow 'up' which is a standard Prometheus metric
    if metric == "up" {
        return true;
    }
    
    // Check if it's a known metric directly
    if known_metrics.contains(metric) {
        return true;
    }
    
    // Handle histogram suffixes (_bucket, _sum, _count)
    for suffix in ["_bucket", "_sum", "_count"] {
        if let Some(base) = metric.strip_suffix(suffix) {
            if known_metrics.contains(base) {
                return true;
            }
        }
    }
    
    false
}

fn extract_metric_names(source: &str) -> HashSet<String> {
    let mut metrics = HashSet::new();
    
    // Extract from pub const NAME: &str = "metric_name"; patterns
    for line in source.lines() {
        if let Some(stripped) = line.trim().strip_prefix("pub const ") {
            if let Some(rest) = stripped.split_once(':') {
                if rest.1.contains("&str") {
                    if let Some(value_part) = rest.1.split('"').nth(1) {
                        metrics.insert(value_part.to_string());
                    }
                }
            }
        }
    }
    
    metrics
}

fn extract_dashboard_metrics(text: &str) -> HashSet<String> {
    let mut metrics = HashSet::new();
    
    // Find all jmp_ prefixed metric names
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if word.starts_with("jmp_") {
            // Remove any PromQL functions or brackets
            let clean = word.split('{').next().unwrap_or(word);
            metrics.insert(clean.to_string());
        }
    }
    
    metrics
}

// Dashboard builder - simplified version focusing on core panels
fn build_dashboard() -> Value {
    json!({
        "__inputs": [{
            "name": "DS_PROMETHEUS",
            "label": "Prometheus",
            "type": "datasource",
            "pluginId": "prometheus"
        }],
        "annotations": {
            "list": [
                {
                    "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
                    "enable": true,
                    "expr": "changes(jmp_config_generation[1m]) > 0",
                    "iconColor": "green",
                    "name": "Config Reloads",
                    "titleFormat": "Config Reload"
                },
                {
                    "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
                    "enable": true,
                    "expr": "changes(jmp_upstream_up{server=~\"$server\"}[2m]) > 0",
                    "iconColor": "red",
                    "name": "Server Up/Down",
                    "titleFormat": "Server State Change"
                }
            ]
        },
        "editable": true,
        "fiscalYearStartMonth": 0,
        "graphTooltip": 1,
        "id": null,
        "links": [{
            "asDropdown": false,
            "icon": "doc",
            "includeVars": false,
            "keepTime": false,
            "tags": [],
            "targetBlank": true,
            "title": "Metrics Documentation",
            "tooltip": "View complete metrics reference",
            "type": "link",
            "url": "https://github.com/yourusername/jellyfin-merge-proxy/blob/main/METRICS.md"
        }],
        "panels": build_panels(),
        "refresh": "30s",
        "schemaVersion": 39,
        "tags": ["jellyfin", "jmp", "proxy", "streaming"],
        "templating": {
            "list": [
                {
                    "current": {"selected": false, "text": "Prometheus", "value": "Prometheus"},
                    "hide": 0,
                    "includeAll": false,
                    "label": "Datasource",
                    "multi": false,
                    "name": "DS_PROMETHEUS",
                    "options": [],
                    "query": "prometheus",
                    "refresh": 1,
                    "skipUrlSync": false,
                    "type": "datasource"
                },
                {
                    "current": {"selected": false, "text": "All", "value": "$__all"},
                    "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
                    "definition": "label_values(jmp_upstream_up,server)",
                    "hide": 0,
                    "includeAll": true,
                    "label": "Server",
                    "multi": true,
                    "name": "server",
                    "options": [],
                    "query": {"query": "label_values(jmp_upstream_up,server)", "refId": "StandardVariableQuery"},
                    "refresh": 2,
                    "sort": 1,
                    "type": "query"
                },
                {
                    "current": {"selected": false, "text": "All", "value": "$__all"},
                    "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
                    "definition": "label_values(jmp_http_requests_total,route)",
                    "hide": 0,
                    "includeAll": true,
                    "label": "Route",
                    "multi": true,
                    "name": "route",
                    "options": [],
                    "query": {"query": "label_values(jmp_http_requests_total,route)", "refId": "StandardVariableQuery"},
                    "refresh": 2,
                    "sort": 1,
                    "type": "query"
                },
                {
                    "current": {"selected": false, "text": "All", "value": "$__all"},
                    "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
                    "definition": "label_values(jmp_http_requests_total,instance)",
                    "hide": 0,
                    "includeAll": true,
                    "label": "Instance",
                    "multi": true,
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
    })
}

fn build_panels() -> Vec<Value> {
    let mut panels = Vec::new();
    let mut panel_id = 1;
    let mut y = 0;
    
    // Overview row
    panels.push(row(panel_id, "Overview", y));
    panel_id += 1;
    y += 1;
    
    // Key stats
    panels.push(stat(panel_id, "Requests/s", 0, y, 3, 4, "reqps",
        "sum(rate(jmp_http_requests_total{instance=~\"$instance\"}[$__rate_interval]))",
        "Total HTTP requests per second"));
    panel_id += 1;
    
    panels.push(stat(panel_id, "Error Rate", 3, y, 3, 4, "percentunit",
        "sum(rate(jmp_http_requests_total{instance=~\"$instance\",status=~\"5..\"}[$__rate_interval])) / sum(rate(jmp_http_requests_total{instance=~\"$instance\"}[$__rate_interval]))",
        "5xx error percentage"));
    panel_id += 1;
    
    panels.push(stat(panel_id, "p95 Latency", 6, y, 3, 4, "s",
        "histogram_quantile(0.95, sum(rate(jmp_http_request_duration_seconds_bucket{instance=~\"$instance\"}[$__rate_interval])) by (le))",
        "95th percentile request duration"));
    panel_id += 1;
    
    panels.push(stat(panel_id, "Servers Up", 9, y, 3, 4, "short",
        "count(jmp_upstream_up{server=~\"$server\"} == 1) or vector(0)",
        "Healthy upstream servers"));
    panel_id += 1;
    
    panels.push(stat(panel_id, "Active Streams", 12, y, 3, 4, "short",
        "sum(jmp_streams_active{server=~\"$server\",instance=~\"$instance\"})",
        "Currently streaming responses"));
    panel_id += 1;
    
    panels.push(stat(panel_id, "Cache Hit Ratio", 15, y, 3, 4, "percentunit",
        "sum(rate(jmp_cache_requests_total{outcome=\"hit\",instance=~\"$instance\"}[$__rate_interval])) / sum(rate(jmp_cache_requests_total{instance=~\"$instance\"}[$__rate_interval]))",
        "Catalog cache hit percentage"));
    panel_id += 1;
    
    panels.push(stat(panel_id, "Uptime", 18, y, 6, 4, "s",
        "jmp_uptime_seconds{instance=~\"$instance\"}",
        "Seconds since process start"));
    panel_id += 1;
    
    y += 4;
    
    // More panels would be added here in the full implementation...
    // This is a simplified version to get the xtask working
    
    panels
}

fn row(id: i32, title: &str, y: i32) -> Value {
    json!({
        "collapsed": false,
        "gridPos": {"h": 1, "w": 24, "x": 0, "y": y},
        "id": id,
        "panels": [],
        "title": title,
        "type": "row"
    })
}

fn stat(id: i32, title: &str, x: i32, y: i32, w: i32, h: i32, unit: &str, expr: &str, desc: &str) -> Value {
    json!({
        "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
        "fieldConfig": {
            "defaults": {
                "color": {"mode": "value"},
                "mappings": [],
                "thresholds": {"mode": "absolute", "steps": [{"color": "green", "value": null}]},
                "unit": unit
            },
            "overrides": []
        },
        "gridPos": {"h": h, "w": w, "x": x, "y": y},
        "id": id,
        "options": {
            "colorMode": "value",
            "graphMode": "area",
            "justifyMode": "auto",
            "orientation": "auto",
            "reduceOptions": {"values": false, "calcs": ["lastNotNull"], "fields": ""},
            "textMode": "auto"
        },
        "pluginVersion": "11.0.0",
        "targets": [{
            "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
            "expr": expr,
            "refId": "A"
        }],
        "title": title,
        "type": "stat",
        "description": desc
    })
}
