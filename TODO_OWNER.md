# Post-commit TODO

Due to cargo registry issues in the Cloud Agent environment (`cpufeatures v0.3.1` requires edition2024),
I was unable to run:
- `cargo xtask check` - to regenerate dashboard and validate
- `cargo run -- --metrics-md` - to update METRICS.md  
- `cargo test` - to run tests
- `cargo fmt` / `cargo clippy` - to check formatting/linting

## Required Actions (owner):

1. **Regenerate dashboard and METRICS.md:**
   ```bash
   cargo xtask check
   ```
   This will:
   - Update `METRICS.md` to include `jmp_upstream_configured`
   - Regenerate `deploy/grafana-dashboard.json` (may need manual updates to use new metric)
   - Validate all metrics and queries

2. **Run tests:**
   ```bash
   cargo test
   ```

3. **Run formatting and linting:**
   ```bash
   cargo fmt
   cargo clippy --all-targets -- -D warnings
   ```

4. **Test the fix:**
   - Start proxy with 3 servers
   - Remove one server from config via hot reload
   - Verify removed server doesn't show as DOWN in dashboard
   - Verify `UpstreamServerDown` alert doesn't fire for removed server
   - Check Prometheus metrics show:
     - `jmp_upstream_configured{server="removed"} 0`
     - Removed server's other metrics (like `jmp_upstream_up`) stop updating

5. **If dashboard needs manual updates:**
   The "Upstreams down" panel and health panels should filter with:
   ```promql
   count((jmp_upstream_up * on(server) jmp_upstream_configured) == 0)
   ```
   Instead of:
   ```promql
   count(jmp_upstream_up == 0)
   ```

## What I Changed:

1. **src/telemetry.rs:**
   - Added `UPSTREAM_CONFIGURED` constant
   - Added metric definition in `METRICS` array

2. **src/servers.rs:**
   - Set `jmp_upstream_configured=1` for all servers in apply loop
   - Set `jmp_upstream_configured=0` when server is removed
   - Removed the line that set `jmp_upstream_up=0` on removal

3. **deploy/prometheus/alerts.yml:**
   - Updated `UpstreamServerDown` to filter: `jmp_upstream_up * on(server) jmp_upstream_configured`
   - Updated `NoHealthyUpstreams` to only count configured servers
   - Removed note about restart requirement
