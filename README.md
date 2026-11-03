# jellyfin-merge-proxy

One Jellyfin server made of many.

You run Jellyfin. Your friends run Jellyfin. Each server has its own movies
and shows, with plenty of overlap, and the TV apps only talk to one server at
a time. Switching servers on a remote is a dozen clicks, and you still end up
with three copies of the same movie and no idea which episode you were on.

jellyfin-merge-proxy (JMP) sits in front of all of those servers and presents
them to clients as a single, ordinary Jellyfin server. Libraries with the same
name merge into one. A movie that lives on three servers shows up once. A show
whose first season is on your server and second season on a friend's browses
as one show. Watch state, favourites, playlists and "Next Up" belong to your
JMP account and follow you across every server. Playback streams straight from
whichever server holds the file.

Any stock Jellyfin client works: Android TV, Fire TV, the web client,
Swiftfin, Jellyfin Media Player. Point it at the proxy and log in.

It is a single Rust binary (tokio, axum, rustls, SQLite) with no OpenSSL,
hot-reloadable config, Prometheus metrics and a ready-made Grafana dashboard.

![Grafana dashboard overview](docs/images/dashboard/01-overview.png)

## Contents

- [How it works](#how-it-works)
- [Quick start](#quick-start)
- [Installing](#installing)
- [Connecting a client](#connecting-a-client)
- [Configuration](#configuration)
- [Accounts, parental limits and per-user state](#accounts-parental-limits-and-per-user-state)
- [What the proxy answers itself](#what-the-proxy-answers-itself)
- [Secrets](#secrets)
- [TLS](#tls)
- [Hot reload](#hot-reload)
- [Observability](#observability)
- [API surface](#api-surface)
- [Limitations](#limitations)
- [Development](#development)

## How it works

```
 Jellyfin client ──► jellyfin-merge-proxy ──┬──► "home"   (Jellyfin)
   (JMP login)        one server id         ├──► "friend" (Jellyfin)
                      merged catalog        └──► ...
```

**Accounts.** Clients log in with a `[[user]]` from the proxy config, never
with an upstream account. JMP issues its own tokens. Each upstream is reached
through one configured account (username/password or API key), or through a
per-user account if you configure one. Upstream logins are lazy, cached and
retried on 401; a server that is down simply drops out until it comes back.

**IDs.** Every id an upstream returns is rewritten, all the way through the
JSON, to a proxy id: the first 32 hex characters of
`sha256("jmp1:" + server name + ":" + upstream id)`. The mapping lives in
`data_dir/jmp.db`, so any proxy id routes back to its server and original id,
and ids in paths, query strings and request bodies are translated back on the
way out. Ids hash the server's *name*, so changing a server's URL keeps every
id valid.

**The catalog index.** JMP crawls each server's library into SQLite and
builds one merged catalog. Copies of the same movie or series are grouped by
Tmdb, Imdb or Tvdb id (else name plus year), seasons and episodes of a grouped
show by number, and genres, studios and people by name. Each group gets one
canonical id that is chosen once and persisted, and it always resolves to the
best live copy. Library queries (`Items`, seasons, episodes, genres, studios,
people, filters, counts) are answered from the index with Jellyfin's own
filter and sort semantics, exact totals and exact paging at any depth. Only
the page being returned is fetched live, in the fields the client asked for.
The index is recrawled every ten minutes by default and is warm immediately
after a restart.

**Fan-out.** Whatever the index cannot answer (an unfamiliar query parameter,
`Latest`, search hints, Live TV, recommendations) goes to every relevant
server at once. JMP merges, de-duplicates, sorts and pages the results the
way Jellyfin would.

**Playback.** `PlaybackInfo`, direct streams, HLS playlists and segments,
subtitles and images go to the server that owns the item. Bodies stream
through unbuffered, `Range` passes through, HLS playlists are rewritten so
segments come back through the proxy, and upstream tokens never reach the
client. Transcoding happens on the owning server.

**Your own state.** Played, play count, resume position, favourites, likes
and last played are kept per JMP user, keyed by the canonical item. Two JMP
users sharing an upstream account still have independent state, and an
episode has one state no matter which copy you played. Changes are written
through to every server holding a copy, and whatever state your upstream
accounts already have is imported once.

## Quick start

```sh
cp config.example.toml jmp.toml
$EDITOR jmp.toml                 # add [[user]] and [[server]] entries
mkdir -p data
sudo chown 65532:65532 jmp.toml data   # the image runs as uid 65532 by default
docker compose up -d
docker compose logs -f
```

A minimal config:

```toml
[proxy]
listen = "0.0.0.0:8096"
name = "Everything"

[[user]]
name = "chris"
password = "change me"      # hashed with argon2id on first load

[[server]]
name = "home"
server = "http://192.168.1.10:8096"
user = "chris"
password = "upstream password"   # encrypted in place on first load

[[server]]
name = "friend"
server = "https://jellyfin.friend.example"
api_key = "..."
user = "chris"

[merge]
dedupe = true
```

Create `jmp.toml` before starting the container. If the file does not exist,
Docker bind-mounts a directory in its place and the proxy will tell you so.
Keep the file writable (no `:ro`): on first load JMP replaces plaintext
passwords with hashes and encrypted values. The data volume holds the
database and `secret.key`, the key those encrypted values depend on. Back it
up.

## Installing

**Container images** for amd64 and arm64 are on GHCR:
`ghcr.io/quyzi/jellyfin-merge-proxy` with tags `latest`, `X.Y.Z` and `X.Y`.
The image is built on Chainguard (no shell, non-root, any uid works) and
reads `/etc/jmp.toml`, writes `/var/lib/jmp` and listens on 8096. There is no
`HEALTHCHECK`; probe `GET /System/Ping` or `/metrics`. `docker-compose.yml`
also brings up Prometheus and Grafana, see [Observability](#observability).

**Debian/Ubuntu and RHEL/Fedora packages** come with every release. They
install `/usr/bin/jellyfin-merge-proxy`, create a `jmp` system user and
`/var/lib/jmp`, put the example config at `/etc/jmp.toml` (never overwritten
on upgrade) and ship a hardened systemd unit.

```sh
sudo dpkg -i jellyfin-merge-proxy_<version>_amd64.deb      # or rpm -i ...
sudo $EDITOR /etc/jmp.toml
sudo systemctl enable --now jellyfin-merge-proxy
journalctl -u jellyfin-merge-proxy -f
```

**Tarballs** are published for x86_64 and aarch64, glibc and static musl.

**Unraid** has a Community Applications template in
[`unraid/`](unraid/README.md): bridge network, port 8096, config and data
under `/mnt/user/appdata/jellyfin-merge-proxy`, running as `99:100`.

**From source:**

```sh
cargo build --release
./target/release/jellyfin-merge-proxy -c ./jmp.toml
```

The config path is the first argument (`-c`, `--config` or positional), else
`$JMP_CONFIG`, else `/etc/jmp.toml`. Other flags are `--help`, `--version`
and `--metrics-md`. `SIGHUP` reloads the config; `SIGINT` and `SIGTERM` shut
down gracefully, giving in-flight requests up to 10 seconds. A config error
exits with code 2, which the systemd unit does not restart.

## Connecting a client

Run the proxy where the TV can reach it, say `http://192.168.1.5:8096`. In
the app choose *Add server*, enter that address, and log in with a JMP
`[[user]]`. The server shows up under `[proxy].name`. There are no public
users and Quick Connect is reported as disabled, so the app shows a plain
username and password form.

LAN auto-discovery is not implemented, so type the address. Keep `[proxy].id`
stable, because clients remember servers by id. For HTTPS to a TV, use a
publicly trusted certificate; TV apps rarely let you add a CA.

Tokens are accepted in every form Jellyfin clients send them:
`Authorization: MediaBrowser ..., Token="..."`, `X-Emby-Authorization`,
`X-Emby-Token`, `X-MediaBrowser-Token`, `?api_key=` and `?ApiKey=`.

## Configuration

[`config.example.toml`](config.example.toml) is the fully commented
reference. The sections at a glance:

| Section | Keys |
|---|---|
| `[proxy]` | `listen`, `name`, `id` (32 hex), `reported_version`, `data_dir` (`/var/lib/jmp`), `unknown_routes` (`primary` or `empty`), `upstream_timeout_secs`, `reload_interval_secs` (3), `drain_timeout_secs` (300), `rewrite_secrets` (true) |
| `[[user]]` | `name`, `password` (plaintext, `{ plain = ".." }`, `{ argon2 = ".." }` or a bare `$argon2id$...`), `max_parental_rating` (a name like `"PG"` or a score), `block_unrated` |
| `[[user.account]]` | `server`, then `user` + `password` or `api_key` (+ `user_id`): this user's own account on that server |
| `[[server]]` | `name`, `server` (base URL, sub-paths fine), `user` + `password` or `api_key` (+ `user` or `user_id`), `priority` (lower wins when deduping), `enabled`, optional `[server.tls]` |
| `[merge]` | `merge_libraries_by_name`, `dedupe`, `dedupe_types`, `label_colliding_libraries`, `fanout_limit` |
| `[index]` | `enabled`, `refresh_interval_secs` (600), `page_size` (500), `prefer` (`priority` or `resolution`) |
| `[userdata]` | `enabled`, `write_through`, `seed_from_upstream` (all true), `min_resume_pct` (5), `max_resume_pct` (90), `min_resume_duration_secs` (300) |
| `[cache]` | `enabled`, `ttl_secs`, `refresh_interval_secs`, `max_entries`, `serve_stale_on_error`, `persist_path` |
| `[database]` | `path` (`<data_dir>/jmp.db`), `journal_mode` (`wal`), `synchronous` (`normal`), `busy_timeout_ms` (5000) |
| `[tls]`, `[upstream_tls]` | see [TLS](#tls) |
| `[logging]` | `filter` (`info`), `format` (`pretty`, `compact`, `json`), `ansi` |
| `[metrics]` | `enabled`, `path`, `listen`, `require_auth`, `process_metrics` |
| `[debug]` | `enabled` (false), `path`, `users` |

The first enabled server is the **primary**. It serves `/web` and receives
routes JMP does not recognise when `unknown_routes = "primary"`. With an
`api_key`, also give `user_id` or `user` so JMP knows whose library and watch
state to use. `[[upstream]]` and `[[upstreams]]` are accepted as aliases for
`[[server]]`.

## Accounts, parental limits and per-user state

By default every JMP user reaches a server through that server's shared
account. A `[[user.account]]` gives one JMP user their own account there:

```toml
[[user]]
name = "kids"
password = "..."
max_parental_rating = "PG"
block_unrated = false

[[user.account]]
server = "home"
user = "kids"
password = "..."      # encrypted in place, like [[server]] secrets
```

The user's own account is used for everything that touches their content:
item details, playback, streams, images, fan-out lists, page hydration,
write-through and the one-time state import. Each account gets its own
upstream device id, so it appears as that user in the server's dashboard. The
library list, the index crawl and series sibling discovery stay on the shared
account, which means indexed results do not follow an account's library
restrictions on the server. Use `max_parental_rating` on the JMP user for that.

**Parental limits** are JMP's own and follow Jellyfin's rule: an item is
shown when its rating score is at most the limit, and unrated videos, series
and collections are hidden only with `block_unrated`. Episodes and seasons
inherit their series' rating. Rating names are resolved through the servers'
rating tables, collected by the crawl. Limits **fail closed**: an unknown
rating name allows only score 0, and before the first index build a
restricted user sees no individual items and no rated videos in lists.
Blocked items are left out of lists with exact totals, return 404 when asked
for directly, and are kept out of NextUp, Resume, search, playlists and
remote play commands. Images stay reachable, as in Jellyfin.

**Play state** follows Jellyfin's `UpdatePlayState` rules: resume
thresholds from `[userdata]`, played at the end, audiobooks by minutes, types
that cannot be resumed or played handled like Jellyfin, and failed stops
ignored. A series or season is played when all its real episodes are.
Virtual ("missing") episodes never count.

## What the proxy answers itself

Some things a single Jellyfin server would know about its own users have no
sensible upstream to forward to, so JMP owns them and keeps them in `jmp.db`.

**User configuration** (`POST /Users/Configuration`): subtitle and audio
preferences, `DisplayMissingEpisodes`, `OrderedViews`, `MyMediaExcludes`,
auto-play of the next episode and the rest, replaced as a whole like Jellyfin
does. The library order and hidden libraries apply to `UserViews`.

**Password changes** (`POST /Users/Password`) verify the current password,
write a new argon2 hash into the `[[user]]` entry of the config file, reload,
and sign the user out everywhere else. This needs a config file JMP can
write. Password resets are refused.

**Display preferences** are stored per user, preferences id and client name,
with Jellyfin's id derivation, defaults and `CustomPrefs` rules, so the home
screen layout you pick on the TV stays picked.

**Playlists** you create through the proxy are yours: per JMP user, with
Jellyfin's sharing model (shares with or without edit rights, public or
private), and they can mix items from any number of servers. They appear in
a "Playlists" library next to the playlists that already exist on upstream
servers. Those upstream playlists keep working as before, forwarded to the
server that has them.

**Sessions and the WebSocket.** JMP keeps its own session registry. Clients
see each other in `GET /Sessions`, can report capabilities, and can remote
control each other: play, pause, seek, send messages and commands. Over
`/socket` the proxy sends keep-alives, the `Sessions` feed, and its own
`UserDataChanged` and `LibraryChanged` events, so a TV's home rows refresh
when you mark something played on your phone or when the next crawl finds
new episodes. Logging in again on the same device replaces the old session,
like Jellyfin.

## Secrets

**Proxy passwords** are hashed with argon2id (random salt) on load and on
every reload, verified on a blocking thread, and cached so unchanged entries
are not re-hashed. Plaintext buffers are zeroized and every secret-bearing
type redacts itself in debug output. Existing argon2 hashes are validated and
used as they are. To pre-hash one:
`echo -n 'secret' | argon2 "$(openssl rand -hex 8)" -id -e`.

**Upstream passwords and API keys** (in `[[server]]` and `[[user.account]]`)
are encrypted with XChaCha20-Poly1305 and written back as `enc:v1:<base64>`.
The key comes from `JMP_SECRET_KEY` (64 hex characters) or from
`<data_dir>/secret.key`, which is generated with mode 0600 if missing. Lose
the key and you re-enter every upstream secret. A value that fails to decrypt
skips that server and counts in `jmp_secret_decrypt_failures_total`.

With `rewrite_secrets = true` (the default) both rewrites happen on load. If
the config file is read-only, JMP logs a warning and keeps running with the
secrets in memory, which means they stay plaintext on disk. Set
`rewrite_secrets = false` to turn the rewrite off on purpose.

Tokens, passwords and keys never appear in logs, metric labels or the debug
endpoint.

## TLS

rustls with the aws-lc-rs provider. Plain HTTP is the default.

**Inbound** (`[tls]`): `cert_path` and `key_path` (PKCS#8, RSA or EC PEM,
checked against each other), `min_version` (`1.2` or `1.3`), `client_auth`
(`none`, `optional`, `required`) with `client_ca_path` for mTLS, `http2`
(ALPN h2 plus http/1.1), and `redirect_http_listen` for a plain listener that
answers with a 308 to HTTPS. Certificate files are watched and swapped
without dropping the listener; a bad new certificate is rejected and the old
one keeps serving.

**Outbound** (`[upstream_tls]` for all servers, `[server.tls]` per server,
merged field by field): `ca_path` added to the system roots, client
certificates for mTLS, `min_version`, `server_name` to verify against a
different name, and `insecure_skip_verify`, which logs a loud warning every
time it is used. Each server gets its own HTTP client, rebuilt on reload.

Certificate expiry dates are exported as
`jmp_tls_cert_expiry_timestamp_seconds`.

## Hot reload

The config and every file it references are polled every
`reload_interval_secs` (or reloaded on `SIGHUP`). A reload validates the
whole new config first, including building every TLS config and every new
backend. If anything fails, it logs the error and keeps the running config.

Servers are diffed by name. An added server logs in in the background and its
libraries show up on the next request. A removed or disabled server vanishes
from views and lists immediately, finishes the requests it is serving (for
up to `drain_timeout_secs`), and is then logged out; its proxy ids answer
`410 Gone` until it is added back under the same name. A changed URL rebuilds
the backend under the same ids. Other per-server changes apply in place, and
the new server set is swapped in atomically, so no request sees half a
reload. Users, per-user accounts, parental limits, merge settings, the log
filter, `[userdata]` and most of `[index]` apply live as well.

These still need a restart and are only warned about: `[proxy] listen`,
`data_dir`, `name` and `id`, turning `[tls]` on or off or changing its
redirect port, `[metrics]` and `[debug]` listen, path and enabled,
`[cache] enabled` and `persist_path`, `[index] page_size`, and
`[logging] format` and `ansi`.

## Observability

**Logs** use `tracing` with a hot-reloadable filter (`RUST_LOG` overrides
`[logging].filter` and then disables runtime changes). Every request runs in
a span with method, route template, status, latency and an `X-Request-Id`
that is taken from the client or generated, and echoed back.

**Metrics** are served at `/metrics`, or on their own port with
`[metrics] listen`, optionally behind a proxy token. About 90 metrics cover
HTTP, every pipeline stage, fan-out outcomes per server, dedupe decisions,
the index, the cache, the upstream clients (latency, status, error kind,
logins, re-auth), auth, streaming (bytes, time to first byte, how streams
end), config and TLS reloads, and the tokio runtime. Labels are bounded:
route templates and server names, never ids or tokens. The full list is in
[METRICS.md](METRICS.md), generated from `src/telemetry.rs`.

**Grafana and Prometheus** come with `docker compose up` (copy `.env.example`
to `.env` and set `GRAFANA_ADMIN_PASSWORD`). The provisioned dashboard has 55
panels covering upstream health over time, latency heatmaps, per-server
gauges, pipeline stages, streaming, dedupe, cache, auth and certificate
expiry, with reload annotations. Alert rules in `deploy/prometheus/alerts.yml`
cover a missing scrape, upstreams down, 5xx ratio, slow upstreams, expiring
certificates, failed reloads, stream disconnects and cache hit ratio.
Screenshots are in [docs/dashboard.md](docs/dashboard.md).

**Debug endpoint.** With `[debug] enabled = true`, `GET /jmp/debug` (proxy
token required, optional user allow-list) returns version, uptime, config
generation and last reload result, per-server status and token age, id table
sizes, cache state, redacted sessions and active streams.

## API surface

Answered or merged by the proxy:

| Endpoint | Behaviour |
|---|---|
| `System/Info[/Public]`, `System/Ping`, `Branding/*` | proxy identity, empty branding |
| `Users/AuthenticateByName`, `Users/Me`, `Users`, `Users/{id}` | JMP users; `Users/Public` is empty |
| `Users/Configuration`, `Users/Password`, `DisplayPreferences/{id}` | proxy-owned, persisted |
| `UserViews` | merged libraries in the user's order, live TV only with `includeExternalContent`, hidden libraries only with `includeHidden`, a Playlists library when you have JMP playlists |
| `Items`, `Users/{u}/Items`, `Shows/{id}/Seasons`, `Shows/{id}/Episodes` | from the index (exact totals, paging and sorts), fan-out while it is cold |
| `Genres`, `Studios`, `Persons` and their `{name}` routes | names merged across servers; a name's own page goes to a server that has it |
| `Items/Latest`, `Search/Hints`, `Years`, `Artists`, `MusicGenres` | fan-out and merge; `Latest` defaults to 20 items |
| `UserItems/Resume`, `Shows/NextUp` | from the index and per-user state, one next episode per show |
| `Items/Filters`, `Items/Filters2`, `Items/Counts` | from the index, with live values the index does not keep |
| `UserPlayedItems`, `UserFavoriteItems`, `UserItems/{id}/Rating`, `UserItems/{id}/UserData` | per-user state, written through to every copy |
| `Movies/Recommendations`, `Items/Suggestions` | fan-out and merge |
| `LiveTv/Channels`, `Programs`, `Recordings`, `Timers`, `SeriesTimers`, `Info`, `GuideInfo` | fan-out and merge, read only |
| `Playlists/*` | JMP playlists; upstream playlist ids are forwarded |
| `Sessions`, `Sessions/Capabilities`, `Sessions/{id}/*`, `/socket` | JMP session registry, remote control, push events |
| `Videos/ActiveEncodings`, `Sessions/Playing/Ping` | sent to the server that issued the play session |
| `QuickConnect/*` | disabled, answered like Jellyfin with Quick Connect off |
| `SyncPlay/*`, `Plugins`, `Packages`, `ScheduledTasks`, Live TV management, user policy | 403, as for a non-administrator |

Everything else that carries a proxy id is forwarded to the server that owns
it: item details, images, `PlaybackInfo`, video and audio streams and HLS,
subtitles, playback reports (also recorded in your state), and legacy
`/Users/{u}/...` paths. With every server down, list endpoints answer 503
rather than pretending your library is empty. Requests JMP does not recognise
go to the primary server or get an empty answer, per `unknown_routes`.

## Limitations

- **Real clients are lightly tested.** A real-server suite checks the
  requests jellyfin-web, Android TV and Swiftfin make against Jellyfin 12.4,
  and the proxy has been browsed by hand, but real TV apps have seen little
  use.
- **Shared upstream identity.** Without a `[[user.account]]`, a JMP user acts
  as the server's shared account upstream: what other native clients see,
  and what a new JMP user imports, is that account's state. The library list
  and the index always come from the shared account.
- **The index takes a crawl to catch up.** New items appear after the next
  crawl (ten minutes by default) and are visible only through fan-out
  endpoints until then. Music, playlists and collections are not indexed, so
  they get no aggregated folder counts. `Latest` is fan-out and hides played
  items by the upstream account's state.
- **WebSocket events are JMP's own.** Upstream servers' pushes are not
  relayed, and remote control reaches only your own sessions.
- **No** SyncPlay, Live TV management, admin dashboard, user management,
  Quick Connect or LAN discovery. `/web` is the primary server's web client
  served through the proxy; it loads but has not been used much.
- **Search folding** of accents covers common Latin letters and may differ
  from Jellyfin for rarer ones.
- `server_name` changes which name the upstream certificate is verified
  against, but SNI is still the URL host. Inbound TLS handshake failures are
  not metered.
- The container image has been run with podman, including as uid 99:100, but
  not with Docker Engine, and the Unraid template has not been installed on a
  real Unraid box.

## Development

```sh
cargo build
cargo test --workspace                         # unit, mock-backend and e2e tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -q -- --metrics-md > METRICS.md      # after changing telemetry::METRICS
cargo xtask dashboard                          # regenerate the Grafana dashboard
```

`cargo test` fails when `METRICS.md` is stale, and `cargo test --workspace`
also fails when the dashboard JSON or alert rules drift from the metrics. No
`.rs` file may exceed 420 lines.

The code is a set of small traits (backends, id mapping, authenticators,
deduplicators, rewriters, stores, reloadable components, TLS provider,
endpoint registry) wired by hand in `AppState::with_options`, with metrics
added through decorators. [ARCHITECTURE.md](ARCHITECTURE.md) has the seam
table and the request flow.

**Against real Jellyfin.** `scripts/real-jellyfin/start-servers.sh` starts
three Jellyfin containers with generated test media (duplicates, a show split
across two servers, an HTTPS server with a private CA), and
`scripts/real-jellyfin-test.sh` drives the proxy like a client through login,
browsing, direct and HLS playback, subtitles, user data, the index, per-user
accounts, parental limits, client compatibility and eleven config
scenarios. It needs podman or docker, ffmpeg and internet access, and is not
part of `cargo test`. Bugs it finds get regression tests in
`tests/real_jellyfin_regressions.rs`. See
[`scripts/real-jellyfin/README.md`](scripts/real-jellyfin/README.md).

**Releasing.** Bump the version in `Cargo.toml`, commit, and push a
`release/vX.Y.Z` tag. The release workflow checks the tag against the
version, runs fmt, clippy and tests, builds the tarballs, `.deb` and `.rpm`
packages for both architectures, pushes signed multi-arch images with SBOM
and provenance to GHCR, and publishes a GitHub release. Versions containing
`-rc`, `-alpha` or `-beta` are marked as prereleases.

Dependabot proposes weekly updates for Cargo, GitHub Actions and the base
images. Patch, minor and digest updates merge automatically once CI passes,
provided the repository allows auto-merge and requires the `fmt`, `clippy`,
`test`, `audit` and `docker` checks on `master`.
