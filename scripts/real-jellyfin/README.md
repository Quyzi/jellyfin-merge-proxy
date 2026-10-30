# Real Jellyfin test bed

Scripts used to test the proxy against real Jellyfin servers (Jellyfin
12.4.0 at the time of writing). They create test data and containers on the
machine they run on; nothing here is used by the proxy itself.

| Script | Does |
|---|---|
| `make-media.sh [root]` | 8-second H.264/AAC test files (ffmpeg `testsrc2`) for three servers: duplicates, unique titles, a show split across servers 1 and 2, a same-named library of another type, an external `.srt`. |
| `start-servers.sh [dir]` | Generates the media and a private test CA, starts three `jellyfin/jellyfin` containers (host network, ports `BASE_PORT+1..3`, server 3 also HTTPS on `HTTPS_PORT`), and runs `setup-server.sh` on each. |
| `setup-server.sh url admin pass "Name\|type\|path"...` | Completes the startup wizard over the API, adds libraries, waits for the scan, runs a full metadata refresh (TMDB ids), creates the non-admin `jmp/jmppw` user and lowers the resume thresholds so 8 s files can have a resume point. |

Then run the black-box suite from the repository root:

```sh
cargo build --release
JMP_DOWN_CMD="sudo podman stop jf2" JMP_UP_CMD="sudo podman start jf2" \
  scripts/real-jellyfin-test.sh
# the same against the container image, as Unraid's uid 99:100:
JMP_IMAGE=localhost/jellyfin-merge-proxy:test JMP_IMAGE_USER=99:100 \
  JMP_DOWN_CMD="sudo podman stop jf2" JMP_UP_CMD="sudo podman start jf2" \
  scripts/real-jellyfin-test.sh
```

See the header of `scripts/real-jellyfin-test.sh` for all variables
(server URLs, ports, credentials, PKI directory). Metadata (ProviderIds)
comes from TMDB, so the servers need internet access for the dedupe checks.

Scenarios live in `scripts/real-jellyfin/scenario-*.sh`. `scenario-index.sh`
checks the catalog index: queries answered from it, exact totals and paging
at every depth, every `SortBy` key in both orders against each upstream's own
order of the same titles (including keys missing from the DTOs and unrated
items), genre/person/studio filters and lists across servers, `Items/Filters`,
a split series as one series, canonical ids that match search hints and
survive a restart, and fallback for parameters the index does not know.

`scenario-userdata.sh` checks per-user state: two proxy users sharing one
upstream account keep independent played/favourite/like/resume state, changes
are written through to every copy, `IsPlayed`/`IsFavorite`/`Likes` filters,
`Resume` and `SortBy=DatePlayed` come from the index, a show split across two
servers has exactly one NextUp entry (and `SeriesId` NextUp gives the first
episode to a user who has watched nothing), series played state and counts
derive from the episodes, and everything survives a restart.

`scenario-accounts.sh` checks per-user upstream accounts and parental
limits: it creates an upstream user `kid` on s1 and s2 (deleting any
previous one, so it starts without state) and gives the proxy user `kids`
that account plus `max_parental_rating = "PG"`. It checks that the account
passwords are encrypted in the config file, that `kids` imports the kid
account's watch state and writes favourites to it (and not to the shared
account; s3 has no kid account and keeps using the shared one), that the
upstream sees separate sessions, that TV-MA/TV-14 series and episodes are
hidden with exact totals, `404` for the item and its episodes, hidden from
search hints, `Latest` and `NextUp`, and that hot reloading `block_unrated`,
a rating name and a numeric score changes what `kids` sees, with exact
paging.

`scenario-compat.sh` checks client compatibility. `compat_single` puts the
proxy in front of s1 alone and sends about 40 requests the way jellyfin-web,
Android TV and Swiftfin do (library sorts and paging, filters, `Genres`,
`Studios`, `Persons`, `Years`, `Items/Filters(2)`, search, item details and
image tags, `UserData`, played/favourite filters, `Resume`, `NextUp`,
`Latest`, `Items/Counts`) to the proxy and to s1 directly, and compares the
answers: the same items in the same order, the same totals, every key s1
sends with the same JSON type (ids, keys and dates by type only), the same
values otherwise (numbers within 1e-9), the same image kinds, and the same
picture behind an image tag. `compat_merged` runs over all three servers
and checks that names, years, filter values and languages are the union
(once each), that `Items/Counts` and genre counts match the merged library
and that a title on two servers appears once in search.
