# Architecture

The proxy is built from small traits. Every seam has a production
implementation and can be replaced through `StateOptions` (see `src/lib.rs`)
without touching the request pipeline. Two rules:

* **Static dispatch (generics)** where a component wraps exactly one inner
  component chosen at build time: metric decorators (`MeteredIdMapper<M>`,
  `MeteredCache<C>`, `MeteredDeduplicator<D>`, `MeteredRewriter<R>`,
  `MeteredHasher<H>`, `MeteredAuthenticator<A>`, `Instrumented<T>`) and
  `TomlAuthenticator<V: PasswordVerifier>`.
* **Dynamic dispatch (`dyn`)** for runtime lists and swappable services:
  backends (`servers::BackendSet` of `Arc<dyn MediaBackend>`, held in an
  `ArcSwap`), reload subscribers, the rewriter
  chain, the endpoint registry, and the `Arc<dyn …>` services held in
  `AppState`.

Errors are `thiserror` enums in `src/error.rs` (`ConfigError`,
`PasswordError`, `AuthError`, `TlsError`, `BackendError`, `ReloadError`).
`anyhow` is only used at the edges (handlers, `main`). Ids are newtypes in
`src/types.rs`: `ServerId` (from the append-only `ServerRegistry`, never
reused), `ProxyItemId` (what
clients see) and `UpstreamItemId` (what a server uses).

## Request flow

```
client ─► axum router (telemetry middleware)
        ─► endpoints::Registry::dispatch        ordered dyn Endpoint list
             ├─ authenticate (proxy token → Client) for Access::Authenticated
             └─ handler ─► MergeStrategy / Deduplicator / CatalogCache
                        ─► IdMapper (proxy id ⇄ (ServerId, UpstreamItemId))
                        ─► dyn MediaBackend::send(BackendRequest)
                             └─ UpstreamCredentials (login/re-login)
                             └─ Instrumented<HttpTransport> (reqwest + rustls)
        ◄─ RewriteChain (json_ids → hls → location) ◄─ BackendResponse
```

## Traits and implementations

| Trait | Module | Implementations | Purpose |
|---|---|---|---|
| `MediaBackend` (: `Reloadable`) | `backend` | `JellyfinBackend`, `MockBackend` | One upstream media server: login state, health, `send(BackendRequest) -> BackendResponse` (buffered or streaming), `in_flight()` and `retire()` for draining after removal. Per-user accounts: `has_account`, `login_as(user)`, `get_json_as`, and `BackendRequest::as_user` (defaults use the shared account). |
| `ContentPolicy` | `parental` | `RatingPolicy` (Jellyfin's MaxParentalRating / BlockUnratedItems rule) | Whether a rating score may be shown for an item type; resolved per proxy user by `ParentalService`. |
| `BackendFactory` | `backend` | `JellyfinFactory`, `MockFactory` | Builds a backend per `[[server]]`. Swap in `StateOptions::backend_factory`. |
| `UpstreamCredentials` | `backend::credentials` | `PasswordCredentials` (`user` + `password`), `ApiKeyCredentials` (`api_key` + `user`/`user_id`) | How a backend obtains a token / user id. |
| `AuthChannel` | `backend::credentials` | `JellyfinBackend`, `DeviceChannel` (the backend under a per-user account's device id) | The minimal request surface credentials need, so they are testable without HTTP. |
| `Transport` | `backend::transport` | `HttpTransport`, `Instrumented<T>` | Raw HTTP to an upstream; the decorator emits `jmp_upstream_*` metrics. |
| `PasswordHasher` / `PasswordVerifier` | `password` | `Argon2id`, `MeteredHasher<H>` | Proxy-user password hashing (argon2id) and verification. |
| `ProxyAuthenticator` | `users` | `TomlAuthenticator<V>`, `MeteredAuthenticator<A>` | Proxy logins against `[[user]]` (also `Reloadable`). |
| `IdMapper` | `ids` | `HashedIdTable` (persisted through an `IdStore`), `PersistentViews<M>` (merged views stored and reloaded), `MeteredIdMapper<M>`, `GroupedIdMapper<M>` (canonical ids: best live copy, per-server resolution) | Stable proxy ids `sha256("jmp1:"+server+":"+id)`, merged views. |
| `IdStore` / `SiblingStore` / `SessionStore` / `IndexStore` / `UserDataStore` / `LocalStore` (`Store`) | `store` | `SqliteStore`, `MemoryStore` | Durable state: id mappings, series sibling links, sessions, crawled catalog per server, canonical ids, merged views, per-user item state and seed marks, and what the proxy owns itself (user settings, display preferences, playlists, see `local`). Loaded at startup into the in-memory tables, which write new entries through. |
| `Crawler` | `index::crawl` | `ItemsCrawler` | Reads one server's whole catalog into index `Entry`s (paged `Items?ParentId=<library>&Recursive=true`) plus its parental rating scores. |
| `GroupingPolicy` | `index::grouping` | `ProviderGrouping` | Which copies are the same item (provider ids, name + year; seasons/episodes of grouped series by number). |
| `CopyRanker` | `index::rank` | `PriorityRanker`, `ResolutionRanker` | Best copy of a group (`[index] prefer`). |
| `GroupLookup` | `index::groups` | `CanonicalGroups` (lock-free, published per rebuild) | Canonical id and members of a group, on the request path. |
| `WatchState` | `userdata` | `UserDataTable` (in memory, written through to a `UserDataStore`), `MeteredWatchState<W>` | Per proxy user, per canonical item state (played, play count, position, favourite, likes, rating, last played). |
| `MergeStrategy` | `merge` | `MergeByNameAndType`, `KeepSeparate` | Groups libraries across servers (`[merge].merge_libraries_by_name`). |
| `Deduplicator` | `merge` | `NoDedupe`, `ProviderIdDeduplicator`, `NameDeduplicator`, `MeteredDeduplicator<D>` | Removes duplicate items across servers in merged lists. |
| `CatalogCache` | `cache` | `NoCache`, `MemoryCatalogCache` (TTL + max entries), `DiskCatalogCache` (memory + JSON file), `MeteredCache<C>` | Caches per-server library views (`[cache]`). |
| `ConfigSource` | `reload` | `FileConfigSource`, `StaticConfigSource` | Where the config comes from. |
| `ConfigWatcher` | `reload` | `PollingWatcher` (mtime + size of config + TLS files, default every 3 s), `SighupWatcher` | What triggers a reload; run via `reload::spawn_watcher`. |
| `Reloadable` | `reload` | `BackendManager` (the server set; each `MediaBackend` applies its own in-place changes), `MeteredAuthenticator` (users), `SessionPruner`, `LogFilter`, `ListenerTls`, `CatalogCacheReload`, `UserDataService`, `ParentalService`, `RestartNotices` | Two-phase hot reload through `ReloadHub`: every subscriber validates, then every subscriber applies. A failed validation keeps the old config. |
| `TlsProvider` | `tls` | `RustlsProvider` | Listener and upstream rustls configs, certificate expiry. |
| `ResponseRewriter` | `rewrite`, `index::canonical` | `JsonIdRewriter`, `HlsPlaylistRewriter`, `LocationHeaderRewriter`, `CanonicalIdRewriter` (member ids → canonical ids, except `MediaSources`), `UserDataOverlay` (every `UserData` → the proxy user's), `ParentalFilter` (drops items over the proxy user's limit from list responses), `PlaySessionRecorder` (`PlaybackInfo` responses → which server owns the `PlaySessionId`), `MeteredRewriter<R>` | Ordered `RewriteChain` applied to upstream responses (ids, playlist URLs, redirects). |
| `Endpoint` | `endpoints` | `Route` (pattern + `Access` + handler fn) | Ordered `Registry`; modules `system`, `users`, `library`, `sessions`, `remote`, `userdata`, `compat`, `fallback`. A handler may return `Flow::Next` to defer to the next match. |
| `SecretCipher` | `secrets` | `XChaCha20Cipher` | Encrypts and decrypts upstream passwords and API keys with authenticated encryption (XChaCha20-Poly1305 AEAD). Encrypted values are prefixed `enc:v1:<base64>`. |
| `KeyStore` | `secrets` | `DefaultKeyStore` | Manages the encryption key: reads from `JMP_SECRET_KEY` env var, or `/var/lib/jmp/secret.key` file, or auto-generates (with 0600 permissions) on first run. |

## Catalog index (`src/index/`)

```
IndexService (background task, every [index] refresh_interval_secs / on new server)
  ├─ Crawler per live server ─► StoredServerIndex ─► IndexStore (index_servers, index_entries)
  └─ rebuild when (servers, priorities, crawl times, dedupe, prefer) change:
       Snapshot::build(GroupingPolicy, CopyRanker, IdMapper::encode, persisted canonical ids)
         ├─ items: canonical groups, best copy first, parents, libraries, rating scores
         ├─ names: genres/studios/people merged by normalised name
         └─ GroupMap ─► CanonicalGroups (GroupLookup) + canonical_ids rows
request: IndexQuery::parse (Err = unsupported ⇒ live fan-out) ─► run (filters, SortCtx)
         ─► page ─► hydrate: Users/{uid}/Items?Ids=… per owning server ─► rewrite ─► canonical Id
```

* Page DTOs are fetched by `proxy::fetch::fetch_dtos` (`Items?Ids=` per
  server, on the requesting user's account, through the rewriter chain).
  Items use it via `indexed::hydrate`; `Genres`/`Studios`/`Persons`
  (`indexed_names`) use it for one live member of each merged name, so the
  name DTOs are Jellyfin's own (images, `UserData`), then add Jellyfin's
  per-type counts (`IncludeItemTypes` sent; counted over the client's scope
  with every type; episodes inherit their series' studios; seasons are not
  counted) and honour `EnableUserData` (genres default off, studios/people
  on) and `EnableTotalRecordCount=false` (total 0).
* `Items/Filters2` takes genres/tags from the index and unions the keys the
  index does not keep (audio/subtitle languages) from a live fan-out.
  `Items/Counts` (`indexed_counts`) takes per-type counts from the index
  (copies counted once) and lowers the summed `ItemCount` by the merged
  copies. `Years` is a fan-out list merged by name.

* A canonical id is one member's proxy id, chosen once (`assign_canonical`)
  and persisted, so it stays stable when rankings or copies change.
  `GroupedIdMapper::decode(canonical)` is the best live copy; any other
  member id decodes to its own copy (or a live one while its server is down);
  `resolve_for(id, server)` finds the group's copy on a server, so
  `ParentId`/`SeriesId`/`GenreIds`/`PersonIds` values translate per upstream.
* A snapshot is served only if every live server is covered; otherwise
  queries fall back to fan-out (`jmp_index_queries_total{outcome="cold"}`).
* `IndexQuery::parse` refuses any parameter it does not understand, so the
  index never answers a query differently from Jellyfin; it falls back
  instead (`outcome="unsupported"`, the reason is logged at debug).
* Schema: migration `0002_index.sql` (`user_version` 2) adds
  `index_servers`, `index_entries`, `canonical_ids`, `merged_views`.

## Proxy-owned state (`src/local/`, `src/live/`)

`AppState::local` holds what the proxy answers itself instead of forwarding:

* `settings` (`UserConfiguration` per proxy user, replaced whole like
  `UserManager.UpdateConfigurationAsync`), `prefs` (`DisplayPreferences` per
  user, preferences guid and client, Jellyfin's id derivation and `CustomPrefs`
  rules) and `playlists` (owner, shares, open access, entries as canonical ids).
  All three load from `LocalStore` at start and write through; schema:
  migration `0004_local.sql` (`user_settings`, `display_prefs`, `playlists`).
* `password` changes a proxy user's password: verify, write an argon2 hash into
  the config file (`config_rewrite::set_user_password`), reload, revoke the
  user's other sessions. `policy` builds the `UserPolicy` (parental limits are
  reflected in it).
* `live` (`LiveSessions`) is the in-memory session registry behind
  `Sessions`, `/socket` and remote control between one user's clients;
  `live::notify` turns `UserDataService` and `IndexService` broadcasts into
  `UserDataChanged` / `LibraryChanged` pushes. `play_sessions` remembers which
  upstream issued each `PlaySessionId`.

## Per-user state (`src/userdata/`)

```
client ─► UserPlayedItems / UserFavoriteItems / UserItems/{id}/Rating|UserData
          Sessions/Playing{,/Progress,/Stopped}
        ─► UserDataService (key = canonical id via GroupLookup; rules::* = Jellyfin's
           play-state transitions with [userdata] resume thresholds)
             ├─ WatchState (MeteredWatchState<UserDataTable>) ─► UserDataStore (user_data)
             └─ write-through: the original request forwarded to every live copy's server
reads:  UserDataOverlay (response rewriter) ─► view::dto (series/season: aggregate of episodes)
        IndexQuery::run(user = UserDataService::fact) ─► IsPlayed/IsFavorite/IsResumable/Likes,
                                                          SortBy=DatePlayed/PlayCount, Resume
        nextup::next_up(Snapshot, live, state)      ─► Shows/NextUp (one entry per canonical series)
Seeder (every 10 s): once per (proxy user, server) import Filters=IsPlayed|IsResumable|IsFavorite|
        Likes|Dislikes from the upstream account (the user's own one if configured); copies merged; never overwrites proxy state
```

* State is keyed by the canonical id, so the copies of a movie share one
  state and the episodes of a show split across servers are one list.
* A series or season has no stored played state: it is played when every
  episode is, with `UnplayedItemCount` and `PlayedPercentage`; marking it
  played/unplayed marks its episodes (as Jellyfin does).
* NextUp follows Jellyfin's `TVSeriesManager`: per series, the first
  unplayed regular episode after the furthest played one; series ordered by
  last played; with `SeriesId` and nothing played, the first episode.
* Schema: migration `0003_userdata.sql` (`user_version` 3) adds
  `user_data(user_id, item_id, state JSON)` and `user_data_seeded`.

## Per-user accounts and parental limits (`src/backend/accounts.rs`, `src/parental/`)

```
[[user.account]] ─► AccountPool (per JellyfinBackend, key = proxy user id)
                      Account: UpstreamCredentials + cached login + own device id
request (Client.user_id) ─► BackendRequest::as_user / login_as(Some(user))
        ─► account token if the user has one on that server, else the shared login
           (401 re-logs in only that account)
shared account only: library list (views), index crawl, sibling discovery
Seeder: user with an account imports from it (mark "server#account"), others share
        one fetch of the shared account

[[user]] max_parental_rating / block_unrated ─► ParentalService (ArcSwap of limits)
        policy(user, Snapshot) ─► RatingPolicy { max (name via Snapshot.ratings), block_unrated }
        index queries (run_query), NextUp ─► ParentalService::retain (exact totals)
        fan-out pages ─► retain_dtos (page total lowered)
        forwarded lists ─► ParentalFilter rewriter (after UserDataOverlay)
        item paths, Shows/{id}/Seasons|Episodes ─► 404 when the indexed item is blocked
```

* An item's score is its own rating's, else its series'; names are
  resolved through the union of the servers' `Localization/ParentalRatings`
  (lowercased). Unknown limit names, or no index yet, fail closed (score 0);
  without rating tables, rated videos in fan-out pages are hidden and
  `blocks` refuses every item. A snapshot is only published once at least
  one server has been crawled; after that `blocks` also refuses ids the
  snapshot lacks when they decode to a live server the index does not cover
  yet (`IndexService::note_coverage`, refreshed every index tick), and `Config::validate` rejects limits when
  `[index] enabled = false` (the index would never exist).
* Account secrets are decrypted with the `[[server]]` cipher and rewritten
  as `enc:v1:` by `config_rewrite::rewrite_user_accounts`. Tokens are only
  sent as headers; `AccountLogin`'s `Debug` redacts them; logs name the
  proxy user and upstream user id only.
* Reload: a changed or removed account is dropped from the pool and its
  password session logged out; unchanged accounts keep their login.
  `ParentalService` swaps limits atomically.

## Secret management (`src/secrets.rs`, `src/config_rewrite.rs`)

The proxy protects secrets at rest:

* **User passwords** (`[[user]]`) are hashed with argon2id on first load and
  written back to the config file as PHC strings (`$argon2id$...`). Plaintext
  passwords are never kept in memory or on disk after the first load.
* **Upstream secrets** (`[[server]] password` and `api_key`) are encrypted
  with XChaCha20-Poly1305 AEAD and written back as `enc:v1:<base64>`. The
  proxy decrypts them in memory when needed. Plaintext secrets are accepted
  as input (that's how users enter them) but are replaced on disk after the
  first load.

The encryption key is managed by `KeyStore` (trait) / `DefaultKeyStore`
(default impl):

1. Reads `JMP_SECRET_KEY` env var (64 hex characters), or
2. Reads `/var/lib/jmp/secret.key`, or
3. Auto-generates a 256-bit key and saves it to `/var/lib/jmp/secret.key`
   with 0600 permissions.

The key file is logged clearly on generation with a warning to back it up:
losing it means re-entering all upstream secrets.

`config_rewrite::rewrite_secrets(path, cache, cipher)` modifies the TOML
file in place using `toml_edit` (preserves comments, formatting, and order):

* Hashes plaintext user passwords → argon2id PHC
* Encrypts plaintext upstream secrets → `enc:v1:<base64>`
* Skips already-hashed/encrypted values (idempotent)
* Writes atomically via temp file + rename; falls back to in-place write
  (truncate + write + fsync) when rename fails with `EXDEV`/`EBUSY` (bind-
  mounted single file in Docker/Unraid)
* Emits `jmp_secrets_rewritten_total` (by kind), `jmp_secret_rewrite_failures_total`,
  and `jmp_secret_decrypt_failures_total` metrics

The rewrite is controlled by `[proxy] rewrite_secrets = true` (default) and
runs automatically during `Config::load` after validation. A wrong or missing
key for existing `enc:` values logs a clear error per server and skips that
server (no crash; the rest of the config loads).

Secret types redact in `Debug` and `Display`, and are zeroized on drop
(`zeroize` crate).

## Extending

* **Another backend type** (e.g. a different media server): implement
  `MediaBackend` + `Reloadable`, write a `BackendFactory`, pass it in
  `StateOptions::backend_factory`. `tests/mock_backend.rs` runs the whole
  proxy on `MockFactory` this way.
* **A new route**: `registry.add_first(Route::auth("name", handler!(f)).at("path/**").get())`
  and pass the registry in `StateOptions::endpoints` (also shown in
  `tests/mock_backend.rs`).
* **A new response transformation**: implement `ResponseRewriter`, then
  `RewriteChain::default_chain().with(MeteredRewriter(MyRewriter))`.
* **Another login source** (LDAP, OIDC, ...): implement `ProxyAuthenticator`
  (and `Reloadable` if it reads the config).
* **Another cache store**: implement `CatalogCache`; wrap it in
  `MeteredCache` to keep the `jmp_cache_*` metrics.
* **A different secret encryption scheme**: implement `SecretCipher` and
  `KeyStore`, pass them in `StateOptions`. The `enc:v1:` prefix allows for
  future versioning.

## Server set (`src/servers.rs`)

* `ServerRegistry`: append-only `name ⇄ ServerId` table (case-insensitive).
  Server names found in the stored id mappings but no longer configured are registered too, so
  their ids still decode to *their* `ServerId`.
* `BackendSet`: the enabled backends in config order, stored in
  `AppState` as `Arc<ArcSwap<BackendSet>>`. Handlers take one snapshot
  (`st.backends()`) or look up one backend (`st.backend(id)`, an owned
  `Arc`), so a reload never changes the set under a running request and a
  removed backend stays alive until its last request/stream is dropped.
  `st.server_state(id)` tells `Active`, `Removed` (→ 410 Gone) and
  `Unknown` apart.
* `BackendManager` (`Reloadable` "backends"): diffs `[[server]]` by name.
  `validate` builds every new/changed backend through the `BackendFactory`
  (so a bad URL or TLS file rejects the reload); `apply` keeps unchanged
  servers (calling their own `Reloadable::apply`), adds new ones, rebuilds
  ones whose `server` URL changed (same `ServerId`), purges the per-server
  catalog cache, stores the new set, logs new backends in and spawns
  `servers::drain` for retired ones. `drain` waits for
  `MediaBackend::in_flight() == 0` (a guard counted until the response body is
  dropped) or `drain_timeout_secs`, then calls `MediaBackend::retire()` (ends
  a password login's upstream session; later stragglers get `Down`).

## Hot reload order

`ReloadHub` subscribers, in order: backends (`BackendManager`) → users →
sessions (drops sessions of removed users) → log filter → listener TLS →
catalog cache (re-tune; full purge only when `[merge]` changed) → per-user
state (`[userdata]`) → parental limits → restart
notices (warns about changes such as `listen` that need a restart).
