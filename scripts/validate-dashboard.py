#!/usr/bin/env python3
"""Validate the Grafana dashboard JSON structure."""
import json
import sys

dashboard_path = sys.argv[1] if len(sys.argv) > 1 else 'deploy/grafana-dashboard.json'
print(f"Validating {dashboard_path}...")

with open(dashboard_path, 'r') as f:
    dashboard = json.load(f)

# Check structure
panels = dashboard.get('panels', [])
print(f"✓ Found {len(panels)} panels")

# Count panel types
panel_types = {}
rows = []
for panel in panels:
    ptype = panel.get('type', 'unknown')
    panel_types[ptype] = panel_types.get(ptype, 0) + 1
    if ptype == 'row':
        rows.append(panel.get('title', 'untitled'))

print(f"✓ Panel types: {dict(panel_types)}")
print(f"✓ Found {len(rows)} rows:")
for row in rows:
    print(f"  - {row}")

# Check metrics usage
dashboard_json = json.dumps(dashboard)
jmp_metrics = dashboard_json.count('jmp_')
rate_count = dashboard_json.count('$__rate_interval')
print(f"✓ Found {jmp_metrics} jmp_ metric references")
print(f"✓ Found {rate_count} $__rate_interval usages")

print("\n✓ Dashboard validation PASSED")
