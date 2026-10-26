# Dashboard Testing Guide

This guide explains how to test the Grafana dashboard with jellyfin-merge-proxy.

## Quick Start

```bash
# Start the full observability stack
docker-compose up -d

# Wait ~30 seconds for services to start and first metrics scrape
sleep 30

# Access the services
open http://localhost:3000  # Grafana (admin/admin)
open http://localhost:9090  # Prometheus
```

## Services

- **jellyfin-merge-proxy** `:8096` - Main proxy with `/metrics` endpoint
- **Prometheus** `:9090` - Metrics collection and alerting
- **Grafana** `:3000` - Visualization (default credentials: `admin`/`admin`)

## Dashboard Validation

### 1. Check Provisioning

```bash
# Verify datasource
curl -s -u admin:admin http://localhost:3000/api/datasources | jq '.[] | select(.name=="Prometheus")'

# Verify dashboard
curl -s -u admin:admin http://localhost:3000/api/search?query=Jellyfin | jq '.'
```

### 2. Verify Metrics Flow

```bash
# Check proxy metrics endpoint
curl -s http://localhost:8096/metrics | grep jmp_

# Check Prometheus scraping
curl -s http://localhost:9090/api/v1/targets | jq '.data.activeTargets[] | select(.labels.job=="jellyfin-merge-proxy")'

# Query a metric in Prometheus
curl -s 'http://localhost:9090/api/v1/query?query=jmp_uptime_seconds' | jq '.data.result'
```

### 3. Open the Dashboard

1. Navigate to http://localhost:3000
2. Log in with `admin` / `admin`
3. Go to **Dashboards** → **Jellyfin Merge Proxy**
4. Verify all panels load without errors

### 4. Test Template Variables

At the top of the dashboard, use the dropdowns to filter:
- **Datasource**: Select Prometheus
- **Server**: Select specific upstreams or "All"
- **Route**: Select specific route templates or "All"  
- **Instance**: Select specific proxy instances or "All"

### 5. Verify Panel Data

Check that each row shows data:

- **Overview**: 8 stat panels with sparklines should show current values
- **Traffic**: Request rates, error rates, latency percentiles should have data
- **Upstream Servers**: Health timeline should show 1 (up) for each server
- **Request Pipeline**: Stage duration graphs should show data
- **Streaming**: Active streams should show 0 or more
- **Merge & Dedupe**: ID table size should show entries per server
- **Cache**: Hit ratio, entries, size should have data
- **Auth**: Sessions, logins should show data
- **Config & TLS**: Generation number, cert expiry table should show data
- **Process**: CPU, memory, Tokio metrics should show data

## Generate Traffic

To see more interesting data, generate some traffic:

```bash
# Login
curl -X POST http://localhost:8096/Users/AuthenticateByName \
  -H 'Content-Type: application/json' \
  -d '{"Username":"youruser","Pw":"yourpass"}'

# Get views
curl -s http://localhost:8096/UserViews?api_key=YOUR_TOKEN

# Browse items
curl -s "http://localhost:8096/Items?api_key=YOUR_TOKEN"

# Or use a real Jellyfin client
# Configure it to point to http://localhost:8096
```

## Test Alerting

Check Prometheus alerts:

```bash
# View alerting rules
curl -s http://localhost:9090/api/v1/rules | jq '.data.groups[] | select(.name=="jellyfin-merge-proxy")'

# View active alerts
curl -s http://localhost:9090/api/v1/alerts | jq '.data.alerts[] | select(.labels.alertname)'
```

To trigger alerts:
- **UpstreamServerDown**: Stop an upstream server
- **HighErrorRate**: Generate 5xx errors  
- **ConfigReloadFailed**: Edit config with invalid TOML

## Validate Dashboard JSON

```bash
# Check JSON structure
python3 scripts/validate-dashboard.py

# Verify panel count and types
jq '.panels | length' deploy/grafana-dashboard.json
jq '.panels[] | .type' deploy/grafana-dashboard.json | sort | uniq -c
```

## Screenshots

To capture dashboard screenshots for the PR:

### Option 1: Manual Screenshots

1. Open each row in Grafana
2. Take screenshots showing:
   - Overview row with all stat panels
   - Traffic row with request graphs
   - Upstream Servers with health timeline
   - Any interesting panels with data

### Option 2: Grafana Image Renderer

```bash
# Install Grafana image renderer
docker run -d --name=renderer \
  --network=container:jmp-grafana \
  grafana/grafana-image-renderer:latest

# Enable in Grafana (set in docker-compose.yml):
# GF_RENDERING_SERVER_URL=http://renderer:8081/render
# GF_RENDERING_CALLBACK_URL=http://grafana:3000/

# Then use API to render panels
curl -u admin:admin \
  'http://localhost:3000/render/d-solo/jellyfin-merge-proxy/jellyfin-merge-proxy?orgId=1&panelId=1&width=1000&height=500' \
  -o overview-stats.png
```

### Option 3: Playwright/Puppeteer

Use a headless browser to capture screenshots programmatically:

```javascript
// screenshot.js
const { chromium } = require('playwright');

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage();
  
  // Login
  await page.goto('http://localhost:3000/login');
  await page.fill('input[name="user"]', 'admin');
  await page.fill('input[name="password"]', 'admin');
  await page.click('button[type="submit"]');
  
  // Open dashboard
  await page.goto('http://localhost:3000/d/jellyfin-merge-proxy/jellyfin-merge-proxy');
  await page.waitForTimeout(5000); // Wait for data to load
  
  // Expand rows and screenshot
  await page.screenshot({ path: 'dashboard-overview.png', fullPage: true });
  
  await browser.close();
})();
```

## Troubleshooting

### No data in panels

```bash
# Check proxy is exposing metrics
curl http://localhost:8096/metrics | head

# Check Prometheus is scraping
curl 'http://localhost:9090/api/v1/query?query=up{job="jellyfin-merge-proxy"}'

# Check for scrape errors
curl 'http://localhost:9090/api/v1/targets' | jq '.data.activeTargets[] | select(.lastError != "")'
```

### Dashboard not appearing

```bash
# Check Grafana logs
docker-compose logs grafana

# Re-provision dashboard
docker-compose restart grafana

# Verify file is mounted
docker exec jmp-grafana ls -la /etc/grafana/provisioning/dashboards/
```

### Alerts not firing

```bash
# Check alert rule syntax
promtool check rules deploy/prometheus/alerts.yml

# Check Prometheus config
docker exec jmp-prometheus cat /etc/prometheus/prometheus.yml

# View rule evaluation
curl 'http://localhost:9090/api/v1/rules'
```

## Clean Up

```bash
# Stop services
docker-compose down

# Remove volumes (clears metrics history)
docker-compose down -v
```
