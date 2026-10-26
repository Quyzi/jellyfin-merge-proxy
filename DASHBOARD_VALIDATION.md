# Dashboard Visual Validation Guide

Since the Cloud Agent environment doesn't have Docker available, visual validation needs to be done locally. Here's how to test the dashboard comprehensively.

## Prerequisites

```bash
git clone <repo>
cd jellyfin-merge-proxy
git checkout cursor/grafana-dashboard-a62d
```

## Setup

1. **Configure environment**:
```bash
cp .env.example .env
# Edit .env and set GRAFANA_ADMIN_PASSWORD
```

2. **Configure proxy** (minimal config for testing):
```bash
cat > config.toml << 'EOF'
[proxy]
listen = "0.0.0.0:8096"
name = "Test Proxy"
data_dir = "/tmp/jmp"

[[user]]
name = "test"
password = "test"

[[server]]
name = "mock1"
server = "http://localhost:8097"
user = "admin"
password = "admin"
EOF
```

## Visual Validation Steps

### 1. Start the Stack

```bash
docker compose up -d
```

Wait ~30 seconds for:
- Proxy to start and expose metrics
- Prometheus to scrape first batch
- Grafana to provision dashboard

### 2. Verify Services

```bash
# Check proxy metrics
curl http://localhost:8096/metrics | grep jmp_

# Check Prometheus targets
curl http://localhost:9090/api/v1/targets

# Check Grafana dashboard provisioned
curl -s -u admin:changeme http://localhost:3000/api/search | jq
```

### 3. Access Grafana

Open http://localhost:3000
- Login with credentials from `.env` (default: admin/changeme)
- Navigate to Dashboards → Jellyfin Merge Proxy
- Check all panels load without "No data" or query errors

### 4. Generate Traffic

Generate realistic traffic to populate all panels:

```bash
# Login
TOKEN=$(curl -sX POST http://localhost:8096/Users/AuthenticateByName \
  -H 'Content-Type: application/json' \
  -d '{"Username":"test","Pw":"test"}' | jq -r .AccessToken)

# Browse endpoints
curl -s "http://localhost:8096/UserViews?api_key=$TOKEN"
curl -s "http://localhost:8096/Items?api_key=$TOKEN"
curl -s "http://localhost:8096/System/Info?api_key=$TOKEN"

# Trigger errors
curl -s "http://localhost:8096/Items/invalidid?api_key=$TOKEN"
curl -s "http://localhost:8096/nonexistent?api_key=$TOKEN"
```

### 5. Test Config Reload

```bash
# Edit config
echo "# comment" >> config.toml

# Send SIGHUP
docker compose kill -s HUP jellyfin-merge-proxy

# Check annotation appears in Grafana (Config Reload marker)
```

### 6. Capture Screenshots

#### Option A: Manual Screenshots

1. Open each collapsible row
2. Take full-width screenshots showing:
   - Overview (all stat panels with data)
   - Traffic (requests, errors, latency graphs)
   - Upstream Servers (health, latency, errors)
   - Request Pipeline (stage timing)
   - Streaming (if any streams active)
   - Merge & Dedupe (ID table, dedupe counts)
   - Cache (hit ratio, entries)
   - Auth (logins, sessions)
   - Config & TLS (reload events, cert expiry)
   - Process (CPU, memory, Tokio)

3. Save to `artifacts/screenshots/`

#### Option B: Playwright Script

```javascript
// screenshot.js
const { chromium } = require('playwright');

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage();
  page.setViewportSize({ width: 1920, height: 1080 });
  
  // Login
  await page.goto('http://localhost:3000/login');
  await page.fill('input[name="user"]', 'admin');
  await page.fill('input[name="password"]', 'changeme');
  await page.click('button[aria-label="Login button"]');
  await page.waitForURL('**/dashboards');
  
  // Navigate to dashboard
  await page.goto('http://localhost:3000/d/jellyfin-merge-proxy');
  await page.waitForTimeout(5000); // Let panels load
  
  // Expand all rows and screenshot
  const rows = await page.locator('.dashboard-row').all();
  for (let i = 0; i < rows.length; i++) {
    await rows[i].click(); // Expand
    await page.waitForTimeout(1000);
  }
  
  await page.screenshot({ 
    path: 'artifacts/screenshots/dashboard-full.png', 
    fullPage: true 
  });
  
  // Individual row screenshots
  const rowTitles = [
    'Overview', 'Traffic', 'Upstream Servers', 'Request Pipeline',
    'Streaming', 'Merge and Deduplication', 'Catalog Cache',
    'Authentication and Sessions', 'Configuration and TLS', 
    'Process and Runtime'
  ];
  
  for (const title of rowTitles) {
    const row = page.locator(`text="${title}"`).first();
    await row.scrollIntoViewIfNeeded();
    await page.waitForTimeout(500);
    
    const filename = title.toLowerCase().replace(/\s+/g, '-');
    await row.screenshot({ 
      path: `artifacts/screenshots/row-${filename}.png`
    });
  }
  
  await browser.close();
})();
```

Run:
```bash
npm install -D playwright
node screenshot.js
```

#### Option C: Grafana Image Renderer

```bash
# Add renderer service to docker-compose.yml
docker run -d --name=renderer \
  --network=container:jmp-grafana \
  grafana/grafana-image-renderer:latest

# Render via API
for panel_id in $(seq 1 71); do
  curl -u admin:changeme \
    "http://localhost:3000/render/d-solo/jellyfin-merge-proxy/jellyfin-merge-proxy?orgId=1&panelId=$panel_id&width=1200&height=600" \
    -o "artifacts/screenshots/panel-$panel_id.png"
done
```

## Validation Checklist

For each row, verify:

### Overview
- [ ] All 7 stat panels show values (not null/NaN)
- [ ] Sparklines render correctly
- [ ] Colors match thresholds (green/yellow/red)
- [ ] Uptime shows reasonable value
- [ ] Build info shows version and git SHA

### Traffic
- [ ] Request rate shows data
- [ ] Status code breakdown visible
- [ ] Error rate calculated correctly
- [ ] Latency percentiles (p50, p95, p99) render
- [ ] In-flight requests shows 0 or small number

### Upstream Servers
- [ ] Health timeline shows 1 (up) for each server
- [ ] Latency graphs show data
- [ ] Error breakdown by kind renders
- [ ] Fan-out results show outcomes
- [ ] Token age shows seconds

### Request Pipeline
- [ ] Stage duration stacked chart shows breakdown
- [ ] p95 per stage renders
- [ ] Fan-out metrics show upstream counts

### Streaming
- [ ] Active streams (may be 0 without clients)
- [ ] Throughput graphs render
- [ ] TTFB p95 shows when streams occur
- [ ] Stream endings by outcome

### Merge & Dedupe
- [ ] Items in/out shows values
- [ ] Duplicates dropped (may be 0)
- [ ] ID table size shows entries per server

### Cache
- [ ] Hit/miss/stale breakdown
- [ ] Hit ratio percentage
- [ ] Entries and size show values
- [ ] Refresh duration and failures

### Auth
- [ ] Login rate by outcome
- [ ] Token validations
- [ ] Active sessions count
- [ ] Argon2 timing (when logins occur)

### Config & TLS
- [ ] Reload events (check annotations)
- [ ] Config generation number
- [ ] Upstream counts
- [ ] Cert expiry table (if TLS enabled)

### Process
- [ ] CPU usage renders
- [ ] Memory (RSS) shows bytes
- [ ] File descriptors count
- [ ] Tokio tasks and queue depth

## Common Issues & Fixes

### "No data" in panels
- Check Prometheus is scraping: `curl localhost:9090/api/v1/targets`
- Verify proxy exposes metrics: `curl localhost:8096/metrics`
- Check time range (top-right) includes when proxy was running

### Query errors
- Check metric names match telemetry.rs
- Verify template variables are populated
- Run `cargo run -p xtask -- validate`

### Annotations not appearing
- Check config reload or server state change occurred
- Annotations only show when condition is met (changes detected)

### Wrong units
- Verify unit field matches metric type (s for seconds, bytes, reqps, etc.)
- Check threshold values are sensible for the metric

## Save Results

Once validated:
```bash
# Save screenshots
git add artifacts/screenshots/
git commit -m "test(dashboard): add visual validation screenshots"

# Document what was tested in PR
```

## Cleanup

```bash
docker compose down
# Keep volumes: docker compose down (preserves Prometheus data)
# Clean all: docker compose down -v
```
