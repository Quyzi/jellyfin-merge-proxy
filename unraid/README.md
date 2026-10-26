# Unraid

`jellyfin-merge-proxy.xml` is a Community Applications (CA) template
(`<Container version="2">`).

> **Placeholders.** The template points at `ghcr.io/OWNER/jellyfin-merge-proxy:latest`
> and `github.com/OWNER/...` for Registry/Project/Support. Replace `OWNER` with
> whoever publishes the image before installing or submitting the template.
> No image is published by this repository yet; until one is, build it
> yourself (`docker build -t jellyfin-merge-proxy .`) and set *Repository* to
> that tag.

## Install

Either:

* **templates-user:** copy the XML to the flash drive and add the container
  from it:
  ```sh
  cp jellyfin-merge-proxy.xml /boot/config/plugins/dockerMan/templates-user/my-jellyfin-merge-proxy.xml
  ```
  Then *Docker → Add Container → Template:* `my-jellyfin-merge-proxy`.
* **URL:** download it straight into the same folder from the Unraid terminal:
  ```sh
  wget -O /boot/config/plugins/dockerMan/templates-user/my-jellyfin-merge-proxy.xml \
    https://raw.githubusercontent.com/OWNER/jellyfin-merge-proxy/main/unraid/jellyfin-merge-proxy.xml
  ```
  (or add the repository URL under *Docker → Template repositories*, which
  makes Unraid fetch the templates it contains).

What the template sets:

| Setting | Value |
|---|---|
| Network | `bridge` |
| Port | `8096` → container `8096` (what TV apps connect to) |
| Metrics port | optional, advanced; only if `[metrics] listen = "0.0.0.0:9090"` |
| `/config` | `/mnt/user/appdata/jellyfin-merge-proxy` (config.toml + `data/`) |
| `/config/certs` | **optional**, advanced, read-only, `Required="false"`; leave it empty |
| `JMP_CONFIG` | `/config/config.toml` (advanced) |
| `RUST_LOG` | empty (advanced); e.g. `info,jmp=debug` |
| Extra parameters | `--user 99:100` (Unraid's `nobody:users`) |

The container runs as `99:100` and only writes under `/config`. The image
works with any uid; there is no PUID/PGID variable.

**Certificates are always optional.** Plain HTTP is the default, inbound and
to the upstream servers. Only map `/config/certs` if your `config.toml` enables
`[tls]` (HTTPS for clients) or a `[server.tls]`/`[upstream_tls]` `ca_path` or
client certificate. HTTPS upstreams with publicly trusted certificates need
no files at all.

## First run

1. Make sure the appdata folder is writable by uid 99. If Docker created it
   (owned by root), fix it once from the Unraid terminal:
   ```sh
   mkdir -p /mnt/user/appdata/jellyfin-merge-proxy
   chown -R 99:100 /mnt/user/appdata/jellyfin-merge-proxy
   ```
2. Start the container. Without a `config.toml` it writes
   `/mnt/user/appdata/jellyfin-merge-proxy/config.example.toml`, logs what to
   do and stops (exit code 2). If the folder is not writable the log says so,
   with the uid/gid to `chown` to.
3. Copy the example and edit it:
   ```sh
   cd /mnt/user/appdata/jellyfin-merge-proxy
   cp config.example.toml config.toml
   nano config.toml
   ```
   * `[proxy]`: keep `listen = "0.0.0.0:8096"`; set `name` (shown in apps).
   * one `[[user]]` per person/TV: the login used in the apps (plaintext or
     an argon2id hash).
   * one `[[server]]` per Jellyfin server: `server = "http://192.168.1.10:8096"`
     plus `user`/`password` (or `api_key`) of an account on that server.
     Use the server's LAN IP, not `localhost` (that is the container itself).
4. Start the container again. The log shows one "logged in to upstream" line
   per server. Later edits to `config.toml` are picked up live (no restart),
   except `listen` and `data_dir`.

## TV / client setup

In the Jellyfin app choose *Add server* and enter
`http://<unraid-ip>:8096` (the host port from the template). Log in with a
`[[user]]` from `config.toml`, not an account of an upstream server. The
WebUI link opens the Jellyfin web client through the proxy (`/web/`).

## Validate the template

```sh
xmllint --noout unraid/jellyfin-merge-proxy.xml
```
