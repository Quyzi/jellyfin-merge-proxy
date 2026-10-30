//! Small builders for Grafana dashboard JSON (schemaVersion 39+ panels).
//!
//! Panels are plain `serde_json::Value`s; [`Layout`] assigns ids and grid
//! positions so the dashboard definition reads top to bottom like the page.

use serde_json::{json, Map, Value};

mod layout;

pub use layout::Layout;

/// Datasource reference used by every panel and query.
pub fn ds() -> Value {
    json!({"type": "prometheus", "uid": "${datasource}"})
}

/// A Prometheus query.
#[derive(Clone)]
pub struct Query {
    expr: String,
    legend: String,
    instant: bool,
    format: Option<&'static str>,
    hide: bool,
}

/// Range query with a legend template.
pub fn q(expr: impl Into<String>, legend: &str) -> Query {
    Query {
        expr: expr.into(),
        legend: legend.into(),
        instant: false,
        format: None,
        hide: false,
    }
}

impl Query {
    /// Instant query (stat/bar gauge "current value", tables).
    pub fn instant(mut self) -> Self {
        self.instant = true;
        self
    }
    /// Instant query in table format (one row per series, labels as columns).
    pub fn table(mut self) -> Self {
        self.instant = true;
        self.format = Some("table");
        self
    }
    /// Histogram buckets for the heatmap panel.
    pub fn heatmap(mut self) -> Self {
        self.format = Some("heatmap");
        self
    }
    fn to_json(&self, idx: usize) -> Value {
        let mut t = json!({
            "datasource": ds(),
            "editorMode": "code",
            "expr": self.expr,
            "legendFormat": if self.legend.is_empty() { "__auto".to_string() } else { self.legend.clone() },
            "range": !self.instant,
            "instant": self.instant,
            "refId": ref_id(idx),
        });
        if let Some(f) = self.format {
            t["format"] = json!(f);
        }
        if self.hide {
            t["hide"] = json!(true);
        }
        t
    }
}

fn ref_id(i: usize) -> String {
    ((b'A' + i as u8) as char).to_string()
}

/// Threshold steps: base color first, then (value, color) pairs ascending.
pub fn steps(base: &str, rest: &[(f64, &str)]) -> Value {
    let mut v = vec![json!({"color": base, "value": null})];
    v.extend(rest.iter().map(|(n, c)| json!({"color": c, "value": n})));
    json!({"mode": "absolute", "steps": v})
}

/// Field override pinning a series (by display name) to a color.
pub fn color_by_name(name: &str, color: &str) -> Value {
    json!({
        "matcher": {"id": "byName", "options": name},
        "properties": [{"id": "color", "value": {"mode": "fixed", "fixedColor": color}}]
    })
}

/// Overrides for a fixed name -> color palette.
pub fn palette(pairs: &[(&str, &str)]) -> Vec<Value> {
    pairs.iter().map(|(n, c)| color_by_name(n, c)).collect()
}

/// Value mapping: exact value -> display text + color.
pub fn map_value(pairs: &[(&str, &str, &str)]) -> Value {
    let mut opts = Map::new();
    for (i, (val, text, color)) in pairs.iter().enumerate() {
        opts.insert(
            val.to_string(),
            json!({"text": text, "color": color, "index": i}),
        );
    }
    json!({"type": "value", "options": opts})
}

/// Common panel skeleton.
pub struct Panel {
    pub v: Value,
    pub w: u32,
    pub h: u32,
}

impl Panel {
    fn base(kind: &str, title: &str, desc: &str, queries: &[Query]) -> Value {
        json!({
            "type": kind,
            "title": title,
            "description": desc,
            "datasource": ds(),
            "targets": queries.iter().enumerate().map(|(i, q)| q.to_json(i)).collect::<Vec<_>>(),
            "fieldConfig": {"defaults": {}, "overrides": []},
            "options": {},
        })
    }
    fn new(v: Value) -> Self {
        Panel { v, w: 12, h: 8 }
    }
    /// Grid size (24 columns wide; height in 30px rows).
    pub fn size(mut self, w: u32, h: u32) -> Self {
        self.w = w;
        self.h = h;
        self
    }
    pub fn unit(mut self, unit: &str) -> Self {
        self.v["fieldConfig"]["defaults"]["unit"] = json!(unit);
        self
    }
    pub fn decimals(mut self, d: u32) -> Self {
        self.v["fieldConfig"]["defaults"]["decimals"] = json!(d);
        self
    }
    pub fn thresholds(mut self, t: Value) -> Self {
        self.v["fieldConfig"]["defaults"]["thresholds"] = t;
        self
    }
    pub fn mappings(mut self, m: Vec<Value>) -> Self {
        self.v["fieldConfig"]["defaults"]["mappings"] = json!(m);
        self
    }
    pub fn no_value(mut self, text: &str) -> Self {
        self.v["fieldConfig"]["defaults"]["noValue"] = json!(text);
        self
    }
    /// One fixed color for every series / the value.
    pub fn fixed_color(mut self, c: &str) -> Self {
        self.v["fieldConfig"]["defaults"]["color"] = json!({"mode": "fixed", "fixedColor": c});
        self
    }
    pub fn color_mode(mut self, mode: &str) -> Self {
        self.v["fieldConfig"]["defaults"]["color"]["mode"] = json!(mode);
        self
    }
    pub fn overrides(mut self, o: Vec<Value>) -> Self {
        let arr = self.v["fieldConfig"]["overrides"]
            .as_array_mut()
            .expect("overrides");
        arr.extend(o);
        self
    }
    pub fn option(mut self, key: &str, val: Value) -> Self {
        self.v["options"][key] = val;
        self
    }
    pub fn custom(mut self, key: &str, val: Value) -> Self {
        self.v["fieldConfig"]["defaults"]["custom"][key] = val;
        self
    }

    // ---- time series -------------------------------------------------------

    /// Line chart with a soft gradient fill and a table legend (mean/max/last).
    pub fn timeseries(title: &str, desc: &str, queries: &[Query]) -> Self {
        let mut v = Self::base("timeseries", title, desc, queries);
        v["fieldConfig"]["defaults"] = json!({
            "color": {"mode": "palette-classic"},
            "custom": {
                "drawStyle": "line",
                "lineInterpolation": "smooth",
                "lineWidth": 2,
                "fillOpacity": 18,
                "gradientMode": "opacity",
                "showPoints": "never",
                "spanNulls": false,
                "axisBorderShow": false,
                "axisSoftMin": 0,
                "stacking": {"mode": "none", "group": "A"},
                "thresholdsStyle": {"mode": "off"}
            },
            "min": 0
        });
        v["options"] = json!({
            "legend": {"showLegend": true, "displayMode": "list", "placement": "bottom", "calcs": []},
            "tooltip": {"mode": "multi", "sort": "desc"}
        });
        Panel::new(v)
    }
    /// Stack the series (areas, or bars with [`Panel::bars`]).
    pub fn stacked(self) -> Self {
        self.custom("stacking", json!({"mode": "normal", "group": "A"}))
            .custom("fillOpacity", json!(55))
            .custom("gradientMode", json!("none"))
            .custom("lineWidth", json!(1))
    }
    /// Draw as bars (event counts per interval).
    pub fn bars(self) -> Self {
        self.custom("drawStyle", json!("bars"))
            .custom("fillOpacity", json!(80))
            .custom("gradientMode", json!("none"))
            .custom("lineWidth", json!(1))
            // event counts: keep a 0..1 axis instead of 0..100 when nothing happened
            .custom("axisSoftMax", json!(1))
    }
    /// Legend as a table with the given calcs (e.g. mean, max, lastNotNull).
    pub fn legend_table(self, calcs: &[&str], placement: &str) -> Self {
        let sort = calcs.first().copied().unwrap_or("mean");
        self.option(
            "legend",
            json!({"showLegend": true, "displayMode": "table", "placement": placement,
                   "calcs": calcs, "sortBy": legend_calc_name(sort), "sortDesc": true}),
        )
    }

    // ---- stat ---------------------------------------------------------------

    /// Big number with a background sparkline, colored by thresholds.
    pub fn stat(title: &str, desc: &str, query: Query) -> Self {
        let mut v = Self::base("stat", title, desc, &[query]);
        v["fieldConfig"]["defaults"] = json!({
            "color": {"mode": "thresholds"},
            "thresholds": steps("green", &[]),
            "mappings": []
        });
        v["options"] = json!({
            "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": false},
            "colorMode": "background_solid",
            "graphMode": "area",
            "justifyMode": "center",
            "textMode": "value",
            "wideLayout": true,
            "showPercentChange": false,
            "orientation": "auto"
        });
        Panel::new(v).size(3, 4)
    }
    /// Stat with colored value text instead of a solid background.
    pub fn text_colored(self) -> Self {
        self.option("colorMode", json!("value"))
    }
    pub fn no_graph(self) -> Self {
        self.option("graphMode", json!("none"))
    }

    // ---- other panel kinds -------------------------------------------------

    /// Up/down bands per series.
    pub fn state_timeline(title: &str, desc: &str, queries: &[Query]) -> Self {
        let mut v = Self::base("state-timeline", title, desc, queries);
        v["fieldConfig"]["defaults"] = json!({
            "color": {"mode": "thresholds"},
            "thresholds": steps("green", &[]),
            "custom": {"fillOpacity": 85, "lineWidth": 0, "spanNulls": false, "insertNulls": false, "hideFrom": {"legend": false, "tooltip": false, "viz": false}}
        });
        v["options"] = json!({
            "mergeValues": true,
            "showValue": "never",
            "alignValue": "center",
            "rowHeight": 0.8,
            "legend": {"showLegend": false, "displayMode": "list", "placement": "bottom"},
            "tooltip": {"mode": "single", "sort": "none"}
        });
        Panel::new(v)
    }

    /// Prometheus histogram buckets as a heatmap (`query` must use `.heatmap()`).
    pub fn heatmap(title: &str, desc: &str, query: Query, unit: &str) -> Self {
        let mut v = Self::base("heatmap", title, desc, &[query]);
        v["fieldConfig"]["defaults"] = json!({
            "custom": {"hideFrom": {"legend": false, "tooltip": false, "viz": false}, "scaleDistribution": {"type": "linear"}}
        });
        v["options"] = json!({
            "calculate": false,
            "cellGap": 1,
            "cellValues": {"unit": "short"},
            "color": {"mode": "scheme", "scheme": "Oranges", "steps": 64, "exponent": 0.5, "fill": "dark-orange", "reverse": false, "scale": "exponential"},
            "exemplars": {"color": "rgba(255,0,255,0.7)"},
            "filterValues": {"le": 1e-9},
            "legend": {"show": true},
            "rowsFrame": {"layout": "auto"},
            "tooltip": {"mode": "single", "showColorScale": true, "yHistogram": true},
            "yAxis": {"axisPlacement": "left", "reverse": false, "unit": unit, "decimals": 0}
        });
        v["maxDataPoints"] = json!(60);
        Panel::new(v)
    }

    /// Horizontal gradient bars comparing series (e.g. per server).
    pub fn bargauge(title: &str, desc: &str, query: Query) -> Self {
        let mut v = Self::base("bargauge", title, desc, &[query]);
        v["fieldConfig"]["defaults"] = json!({
            "color": {"mode": "continuous-BlPu"},
            "thresholds": steps("green", &[]),
            "min": 0
        });
        v["options"] = json!({
            "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": false},
            "orientation": "horizontal",
            "displayMode": "gradient",
            "valueMode": "color",
            "namePlacement": "top",
            "showUnfilled": true,
            "sizing": "auto",
            "minVizHeight": 16,
            "minVizWidth": 8,
            "maxVizHeight": 36
        });
        Panel::new(v)
    }

    /// Table built from instant table-format queries joined on `join_on`.
    pub fn table(
        title: &str,
        desc: &str,
        queries: &[Query],
        join_on: &str,
        columns: &[(&str, &str)],
    ) -> Self {
        let mut v = Self::base("table", title, desc, queries);
        v["fieldConfig"]["defaults"] = json!({
            "custom": {"align": "auto", "cellOptions": {"type": "auto"}, "inspect": false, "filterable": false},
            "thresholds": steps("green", &[]),
            "color": {"mode": "thresholds"}
        });
        v["options"] = json!({"showHeader": true, "cellHeight": "sm", "footer": {"show": false, "reducer": ["sum"], "fields": ""}});
        // Value #A.. -> column names, drop Time, keep the join label first.
        let mut rename = Map::new();
        let mut index = Map::new();
        index.insert(join_on.into(), json!(0));
        for (i, (col, _)) in columns.iter().enumerate() {
            rename.insert(format!("Value #{}", ref_id(i)), json!(col));
            index.insert(format!("Value #{}", ref_id(i)), json!(i + 1));
        }
        rename.insert(join_on.into(), json!(capitalize(join_on)));
        v["transformations"] = json!([
            {"id": "joinByField", "options": {"byField": join_on, "mode": "outer"}},
            {"id": "organize", "options": {"excludeByName": {"Time": true}, "indexByName": index, "renameByName": rename}}
        ]);
        let mut p = Panel::new(v);
        for (col, unit) in columns {
            p = p.overrides(vec![json!({"matcher": {"id": "byName", "options": col}, "properties": [{"id": "unit", "value": unit}]})]);
        }
        p
    }
    /// Per-column override properties for tables.
    pub fn column(self, col: &str, props: Value) -> Self {
        self.overrides(vec![
            json!({"matcher": {"id": "byName", "options": col}, "properties": props}),
        ])
    }
    pub fn sort_by(self, col: &str, desc: bool) -> Self {
        self.option("sortBy", json!([{"displayName": col, "desc": desc}]))
    }
}

fn legend_calc_name(calc: &str) -> &'static str {
    match calc {
        "mean" => "Mean",
        "max" => "Max",
        "lastNotNull" => "Last *",
        "sum" => "Total",
        _ => "Mean",
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}
