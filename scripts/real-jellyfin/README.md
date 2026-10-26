# Real Jellyfin test bed

Scripts used to test the proxy against real Jellyfin servers (Jellyfin
12.3.0 at the time of writing). They create test data and containers on the
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
