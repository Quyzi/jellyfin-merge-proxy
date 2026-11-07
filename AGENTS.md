# Repository Guidelines

## Project Overview
HTTP proxy (Rust) that fronts several upstream Jellyfin servers as one virtual Jellyfin server. Clients log in with a proxy account (`[[user]]`) and see merged/deduped libraries, per-user watch state, parental limits, optional session caps and country allowlists. Features: catalog index, hot reload, TLS (listener + upstream), encrypted secrets, Prometheus metrics + generated Grafana dashboard. Deploy targets: Docker, deb/rpm, tarballs, Unraid.

## Architecture & Data Flow
Workspace: root crate = lib `jmp` (`src/lib.rs`) + bin `jellyfin-merge-proxy` (`src/main.rs`); member `xtask` depends on the lib. `ARCHITECTURE.md` is the authoritative seam/trait table (hand-maintained).

**Boot**: `main.rs` (hand-parsed args; config = `-c` → `$JMP_CONFIG` → `/etc/jmp.toml`) → `config::first_run_help` → `telemetry::init_logging` *before* `Config::load` → `server::start` (validates TLS, builds `AppState::with_options`, spawns main/metrics/redirect listeners, gauge task, `PollingWatcher`) + `reload::spawn_watcher(SighupWatcher)`.

**Request**: `proxy::router` has one `fallback(dispatch)` behind CORS → TraceLayer → `telemetry::http_layer` (only `/socket`, `/embywebsocket`, metrics, debug are real routes). `dispatch` strips `emby`/`mediabrowser` prefix, buffers body (64 MiB), calls `endpoints::Registry` (ordered `dyn Endpoint` list matching classified segments `{u} {root} {view} {id} * **`). Handlers use the `handler!` macro: `async fn(&Shared, Req, &Client) -> anyhow::Result<Flow>`; `Flow::Next(req)` defers to next route. Handler modules: `system users library sessions userdata compat fallback`. Building blocks in `src/proxy/`: `fan` (fan-out), `fetch` (`fetch_dtos`), `forward`, `views`, `list`, `indexed*`, `siblings`, `ws`. `paths.rs` maps legacy `/Users/{id}/…` to `/UserItems/…`.

**Response rewriting** (`src/rewrite.rs`, `RewriteChain`; **order matters**): `JsonIdRewriter` → `HlsPlaylistRewriter` → `LocationHeaderRewriter` → `CanonicalIdRewriter` → `UserDataOverlay` → `ParentalFilter`, each `MeteredRewriter`-wrapped. JSON id rewriting is key-name driven (`*Id`/`*Ids` with 32-hex values); non-item id fields must be added to `SKIP_ID_KEYS`.

**IDs**: proxy id = `sha256("jmp1:"+lowercase server NAME+":"+id)[..16]` hex. Hashes name, not URL. `ServerRegistry` (`servers.rs`) is append-only; removed servers answer 410. Newtypes `ServerId`, `ProxyItemId`, `UpstreamItemId` (`types.rs`) — never raw strings. Canonical id per dedupe group chosen once and persisted (`GroupedIdMapper`).

**Backends** (`src/backend/`): `MediaBackend`/`BackendFactory`, streaming `BackendResponse`, `InFlight` guard (drain), `AccountPool` (per-user upstream accounts via `BackendRequest::as_user`, falling back to shared account; shared account always used for library list, index crawl, sibling discovery). Logins lazy, cached, retried on 401.

**Index** (`src/index/`): `Crawler` → `StoredServerIndex` → `Snapshot::build` → `CanonicalGroups`. Snapshot served only if every live server covered, else live fan-out. `IndexQuery::parse` rejects unknown params (falls back to fan-out) so the index never answers differently from Jellyfin. Only the requested page is hydrated live.

**Userdata / parental**: per-user state keyed by canonical id, write-through to upstream copies, `Seeder` imports upstream once per (user, server); series/season played state derived from episodes. Parental limits are the proxy's own and **fail closed** (unknown limit names, no index yet); blocked → 404.

**Persistence** (`src/store.rs`, `src/store/`): seams `IdStore SiblingStore SessionStore IndexStore UserDataStore` bundled as `Store`; `SqliteStore` (`<data_dir>/jmp.db`, 0600) and `MemoryStore` (fallback if data_dir unwritable). In-memory tables are source of truth, write-through; `HashedIdTable` batches via mpsc to an `id-persist` thread. Store failures on hot path: `warn!` + `jmp_persist_ops_total{outcome="error"}`, never propagated.

**Hot reload** (`src/reload.rs`, `reload/`): `ReloadHub::run` is two-phase (all `validate`, then all `apply`); validate failure keeps old config. Subscriber order fixed in `AppState::with_options`: BackendManager → auth → SessionPruner → LogFilter → listener TLS → catalog cache → userdata → parental → RestartNotices. New `Reloadable` = implement `reload_name/validate/apply` and register there. Settings needing restart (`listen`, `data_dir`, TLS on/off, `[metrics]`/`[debug]` addr, `[index] page_size`, …) are only warned about; list in README.

## Key Directories
- `src/` — lib; `backend/ endpoints/ proxy/ index/ userdata/ parental/ store/ reload/ config/ rewrite/ ids/ cache/ merge/ telemetry/`
- `tests/` — integration targets (`e2e/`, `client_compat/`, flat `*.rs`), `tests/fixtures/`
- `xtask/` — `cargo xtask` Grafana dashboard generator/validator
- `scripts/` — live black-box suites vs real Jellyfin containers (`real-jellyfin-test.sh`, `real-jellyfin/`), dashboard smoke (`test-dashboard.sh`, `dashboard-lab/`)
- `deploy/` — Prometheus config + `alerts.yml`, Grafana provisioning, **generated** `grafana-dashboard.json`
- `unraid/`, `debian/`, `jellyfin-merge-proxy.service`, `Dockerfile`, `docker-compose.yml` — packaging
- `config/` — local gitignored config, may hold real credentials; never commit or copy contents

## Development Commands
```sh
cargo build --release
cargo run -- -c config.toml            # or JMP_CONFIG=...
cargo fmt --all -- --check             # CI
cargo clippy --all-targets -- -D warnings   # CI
cargo test --all-targets               # CI (root package only)
cargo test --workspace                 # also xtask (dashboard staleness test)
cargo xtask dashboard                  # regenerate deploy/grafana-dashboard.json (then runs check)
cargo xtask check                      # validate dashboard + deploy/prometheus/alerts.yml vs METRICS
cargo run -q -- --metrics-md > METRICS.md   # regenerate metrics docs
promtool check rules deploy/prometheus/alerts.yml
scripts/real-jellyfin-test.sh [scenario...]  # needs podman/docker, ffmpeg, internet; not part of cargo test
```
Release: bump `Cargo.toml` version, commit, push tag `release/vX.Y.Z` (`.github/workflows/release.yml`).

## Code Conventions & Common Patterns
- **Formatting/lint**: default rustfmt/clippy, no config files; CI fails on warnings. Edition 2021, `recursion_limit = 512`.
- **Size cap**: no `.rs` file (src, tests, xtask) may exceed **420 lines** (`tests/file_size_limit.rs`). Split into `foo.rs` + `foo/`; parent does `pub use sub::*`, children start `use super::*;`.
- **Errors**: `thiserror` enums in `src/error.rs` (`ConfigError PasswordError AuthError TlsError BackendError ReloadError`; `StoreError` in `store.rs`) for trait seams; `anyhow` only at edges (handlers, `main`, `server::start`, reload). `AuthError` is deliberately coarse ("invalid username or password"). Log anyhow with `{e:#}`; structured fields (`upstream = %name`). Handler 5xx logs route template, never raw path.
- **Dispatch**: generics for single-inner decorators (`Metered*<T>`, `Instrumented<T>`, `TomlAuthenticator<V>`); `dyn` for runtime lists/swappable services. New metric-bearing components wrap in a `Metered…` decorator rather than emitting inline.
- **DI**: hand-wired in `AppState::with_options`; `StateOptions` has `Option<Arc<dyn …>>` per seam defaulting to production impl (`backend_factory`, `endpoints`, TLS provider, catalog, config source, `password_cache`). No DI framework.
- **State/concurrency**: config = `RwLock<Arc<Config>>` via `st.cfg()`; backends = `ArcSwap<BackendSet>` (take one snapshot per request: `st.backends()`/`st.backend(id)`); index snapshot/groups/parental limits via `ArcSwap`. `std::sync::{Mutex,RwLock}` with `.lock().unwrap()` — never hold across `.await`. `tokio::sync::Mutex<()>` only as async serializer (`reload_lock`, `views_refresh`). Background tasks hold `Weak<AppState>`. CPU-heavy work (argon2, config load) in `spawn_blocking`. SQLite is a single `Mutex<Connection>`.
- **Config** (`src/config/*`): serde `Raw*` types with `deny_unknown_fields` → `Config` → `validate()`; `Config: PartialEq` powers reload "unchanged" detection. `Config::parse` (pure, tests) vs `Config::load` (loads key + **rewrites the file** to encrypt secrets, despite `FileConfigSource` docs). Aliases `servers/upstream/upstreams`, `users` exist; Rust field is `upstreams`.
- **Secrets**: `[[user]] password` = argon2id one-way hash (`password.rs`, accepts `"plain"`, `{plain}`, `{argon2}`, bare `$argon2…`). `[[server]]`/`[[user.account]]` password & `api_key` = reversible XChaCha20-Poly1305 `enc:v1:<base64>` (`secrets.rs`). Key: `JMP_SECRET_KEY` (64 hex) → `<data_dir>/secret.key` (loader: `data_dir.join("secret.key")`, default `/var/lib/jmp/secret.key`; the hardcoded `/var/lib/jmp` in `secrets.rs` doc comment/`describe()` and `ARCHITECTURE.md` is only the default) → auto-generated 0600. Losing the key file loses stored upstream secrets. `Secret`/`Password` redact Debug and zeroize; `.expose()` only at point of use. Never log tokens (`RedactedSession` for debug endpoint).
- **Secret rewrite** (`config/load.rs:32`, `config_rewrite.rs`): gated by `[proxy] rewrite_secrets` (default `true`; `false` disables it). When on, `Config::load` writes back both `[[user]]` argon2 hashes and upstream `enc:v1:` values. A read-only config is a **warning, not an error**: load.rs logs "secrets remain plaintext on disk" and the proxy keeps running with in-memory secrets. So "plaintext never stays on disk" (README:407, `config.example.toml:56`) is false on the read-only path; don't rely on it.
- **Metrics** (`src/telemetry.rs`): add const in `telemetry::names` (`jmp_*`; counters `_total`, histograms `_seconds`/`_bytes`) **and** a `m!(NAME, Kind, "labels", "help")` row in `telemetry::METRICS`; then regenerate `METRICS.md`, and `cargo xtask dashboard` if dashboard/alerts use it. Labels must be bounded (route templates, server config names, small enums) — never ids/paths/tokens. Pre-seed counters in `init_metrics` if `increase()` must see them. `telemetry::Stage::start("name")` = RAII timer.
- **Logging**: `tracing` with reloadable `EnvFilter`; `RUST_LOG` overrides `[logging]` and disables runtime filter changes.
- **Migrations**: `src/store/migrations/000N_*.sql`, append-only, contiguous, one transaction each, tracked via `PRAGMA user_version`; never edit a released one; add a `MIGRATIONS` entry.
- **Naming**: `Metered*` decorators, `Reload*`/`*Reloadable` hot-reload types, `*Store` persistence seams, `*Service` stateful orchestrators. Module `//!` docs describe seams — keep current. Don't add to the back-compat `pub mod upstream` alias in `lib.rs`.
- **Clean cutover**: when changing a seam, update all callers, `ARCHITECTURE.md`, README (Configuration, Hot reload restart list, API surface, Limitations).

## Important Files
- `src/main.rs`, `src/lib.rs` (`AppState`, `StateOptions`, `build_app`), `src/server.rs`, `src/proxy.rs`, `src/rewrite.rs`, `src/telemetry.rs`
- `config.example.toml` (schema reference), `config.minimal.toml` (packaged, all comments)
- `ARCHITECTURE.md`, `README.md`, `METRICS.md` (generated), `DASHBOARD_TESTING.md`
- `build.rs` (embeds `JMP_GIT_SHA`: env → `git rev-parse --short=12 HEAD` → `unknown`), `.cargo/config.toml` (`xtask` alias)
- `xtask/src/dashboard/*.rs`, `xtask/src/grafana*.rs` — dashboard sources (never hand-edit `deploy/grafana-dashboard.json`)

## Runtime/Tooling Preferences
- Cargo only; recent stable Rust (no `rust-toolchain`, no MSRV; axum 0.8, reqwest 0.13, rusqlite 0.41 `bundled` → needs C compiler). rustls with `aws_lc_rs`; no OpenSSL.
- tokio multi-thread runtime built manually in `main.rs`.
- Dev profile compiles `argon2`/`blake2` at `opt-level = 3` — keep, or tests get very slow. Release: thin LTO, strip.
- Env: `JMP_CONFIG`, `JMP_SECRET_KEY`, `RUST_LOG`, build-time `JMP_GIT_SHA`.
- Config file should stay writable (else secrets stay plaintext on disk, see Secret rewrite): Docker bind mount `./jmp.toml:/etc/jmp.toml` never `:ro`; systemd unit has `ReadWritePaths=/etc/jmp.toml /var/lib/jmp`, heavy hardening (check new syscalls/write paths), exit code 2 = config error (no restart).
- Dockerfile `COPY` is an explicit list (incl. `xtask`); add new build-required top-level files there. `.gitignore` globally ignores `*.ts`, `*.key`, `*.pem`, `/config/` — fixtures with those extensions won't commit.
- Pin GitHub Actions to exact tags (existing style).

## Testing & QA
- Plain `#[test]`/`#[tokio::test]`; dev-deps `tempfile`, `rcgen`. No `#[ignore]`, no feature flags, nothing in `cargo test` needs Docker/real Jellyfin/env.
- Unit tests: inline `mod tests` or sibling `tests.rs`/`*_tests.rs` (`#[cfg(test)] mod tests;`). Shared helpers are `pub(crate)` (e.g. `store::tests::conformance(&dyn Store)` run against every backend).
- Run: `cargo test --test client_compat`, `cargo test --test e2e tls_listener`, `cargo test --lib index::query`.
- **`MockBackend`/`MockFactory`** (`src/backend/mock.rs`, public in lib): `on_json(Some(Method::GET), "path", json)`, `on`, `on_query`, `.calls()`, `.set_down()`, `.hold()`, `.at_url()`. Inject via `StateOptions { backend_factory: Some(...), ..Default::default() }` → `AppState::with_options` → `jmp::build_app`.
- **Harness pattern** (copy per file; no shared `tests/common`): TOML with `listen="127.0.0.1:0"`, `reload_interval_secs=0`, tempdir `data_dir`, `[logging] filter="warn"`; `Config::parse`; bind `127.0.0.1:0`; log in via `POST /Users/AuthenticateByName` with `Authorization: MediaBrowser Client=…`. Call `jmp::telemetry::init_metrics()` in tests touching metrics. `client_compat` waits for `state.index.latest().is_some()`.
- Use `tests/e2e/` (real axum mock Jellyfins + `rcgen` PKI) only when TLS/real HTTP matters; `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`.
- Test names = behavioral snake_case sentences with a `///` stating what broke; `//!` header per suite. `tests/real_jellyfin_regressions.rs` runs on mocks and pins bugs found by the live `scripts/real-jellyfin-test.sh` suite — add new live-found bugs there.
- `tests/fixtures/secrets_v1.json` = legacy argon2 0.5.3/chacha20poly1305 0.10.1 bytes; **never regenerate** (`secret_compat.rs` must still verify/decrypt them).
- Stale-file gates: plain `cargo test` fails if `METRICS.md` is stale (`telemetry::tests::metrics_md_in_sync`, root crate). The dashboard/alerts checks (`xtask::dashboard_is_current_and_valid`) run **only** under `cargo test --workspace` or `cargo xtask check`: dashboard JSON must match the generator, alerts/dashboards may only use known metrics/labels, every panel needs description/unit/`${datasource}`/`$instance`/`$__rate_interval`, and `jmp_upstream_up` queries must join `jmp_upstream_configured == 1`.
- **CI gap**: `ci.yml`/`release.yml` run `cargo test --all-targets` and `cargo clippy --all-targets` without `--workspace`, and nothing runs `cargo xtask check`, so dashboard/alert drift and `xtask` lint are never gated in CI. Fix would be adding `cargo xtask check`, `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` to the workflows. Run those locally first or CI goes red. Don't use `default-members` (likely makes bare `cargo run` ambiguous). Left unchanged.
- No coverage tooling; CI also runs `cargo audit`, a Docker build, and actionlint. Before finishing: fmt, clippy `-D warnings`, `cargo test --workspace`.
- Doc drift to be aware of: README:15 top note ("only tested with mocks in `tests/e2e.rs`") is stale (that path doesn't exist; target is `tests/e2e/main.rs`; the proxy has been tested against real Jellyfin) and its Development test-count line is stale; `xtask/src/main.rs` doc comment mislabels dashboard path as `src/dashboard/`.
