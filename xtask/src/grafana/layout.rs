//! [`Layout`]: grid placement and id assignment for dashboard panels.

use super::Panel;
use serde_json::{json, Map, Value};

/// Places panels left to right, wrapping at 24 columns, and assigns ids.
pub struct Layout {
    pub panels: Vec<Value>,
    next_id: u32,
    x: u32,
    y: u32,
    line_h: u32,
}

impl Default for Layout {
    fn default() -> Self {
        Self::new()
    }
}

impl Layout {
    pub fn new() -> Self {
        Layout {
            panels: Vec::new(),
            next_id: 1,
            x: 0,
            y: 0,
            line_h: 0,
        }
    }
    fn newline(&mut self) {
        self.y += self.line_h;
        self.x = 0;
        self.line_h = 0;
    }
    /// Start a (expanded) row.
    pub fn row(&mut self, title: &str) {
        if self.x > 0 {
            self.newline();
        }
        self.panels.push(json!({
            "type": "row", "title": title, "collapsed": false, "panels": [],
            "id": self.next_id, "gridPos": {"h": 1, "w": 24, "x": 0, "y": self.y}
        }));
        self.next_id += 1;
        self.y += 1;
    }
    pub fn add(&mut self, p: Panel) {
        assert!(p.w >= 1 && p.w <= 24, "panel width out of range");
        if self.x + p.w > 24 {
            self.newline();
        }
        let mut v = p.v;
        let obj = v.as_object_mut().expect("panel object");
        // Stable key order: id/gridPos first, then the rest as built.
        let mut out = Map::new();
        out.insert("id".into(), json!(self.next_id));
        out.insert(
            "gridPos".into(),
            json!({"h": p.h, "w": p.w, "x": self.x, "y": self.y}),
        );
        for (k, val) in std::mem::take(obj) {
            out.insert(k, val);
        }
        self.panels.push(Value::Object(out));
        self.next_id += 1;
        self.x += p.w;
        self.line_h = self.line_h.max(p.h);
        if self.x >= 24 {
            self.newline();
        }
    }
}
