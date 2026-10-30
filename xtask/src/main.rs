//! `cargo xtask <command>` (alias in .cargo/config.toml).
//!
//!   dashboard  regenerate deploy/grafana-dashboard.json from src/dashboard/
//!   check      fail if the committed dashboard is stale or invalid, or the
//!              alert rules use unknown metrics/labels (also run by `cargo test`)

mod dashboard;
mod grafana;
mod validate;

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

const DASHBOARD: &str = "deploy/grafana-dashboard.json";
const ALERTS: &str = "deploy/prometheus/alerts.yml";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// The dashboard JSON exactly as committed (pretty, trailing newline).
pub fn render_dashboard() -> String {
    let mut s = serde_json::to_string_pretty(&dashboard::build()).expect("serialize dashboard");
    s.push('\n');
    s
}

fn check() -> Result<()> {
    let root = root();
    let committed = std::fs::read_to_string(root.join(DASHBOARD))
        .with_context(|| format!("read {DASHBOARD}"))?;
    let mut errs = Vec::new();
    if committed != render_dashboard() {
        errs.push(format!("{DASHBOARD} is stale: run `cargo xtask dashboard`"));
    }
    let dash: serde_json::Value = serde_json::from_str(&committed).context("parse dashboard")?;
    errs.extend(validate::check_dashboard(&dash));
    let alerts =
        std::fs::read_to_string(root.join(ALERTS)).with_context(|| format!("read {ALERTS}"))?;
    errs.extend(validate::check_alerts(&alerts));
    if !errs.is_empty() {
        bail!("{} problem(s):\n  {}", errs.len(), errs.join("\n  "));
    }
    let panels = dash["panels"].as_array().map_or(0, Vec::len);
    println!("ok: {DASHBOARD} up to date and valid ({panels} panels incl. rows); {ALERTS} valid");
    Ok(())
}

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("dashboard") => {
            let path = root().join(DASHBOARD);
            std::fs::write(&path, render_dashboard())
                .with_context(|| format!("write {}", path.display()))?;
            println!("wrote {DASHBOARD}");
            check()
        }
        Some("check") | Some("validate") => check(),
        _ => {
            eprintln!("usage: cargo xtask <dashboard|check>");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dashboard_is_current_and_valid() {
        super::check().unwrap();
    }

    #[test]
    fn generated_dashboard_is_valid() {
        let errs = super::validate::check_dashboard(&super::dashboard::build());
        assert!(errs.is_empty(), "{errs:#?}");
    }
}
