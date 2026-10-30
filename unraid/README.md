# Unraid

`jellyfin-merge-proxy.xml` is a Community Applications (CA) template
(`<Container version="2">`).

> **Image.** The template points at `ghcr.io/quyzi/jellyfin-merge-proxy:latest`,
> which the release workflow publishes for each version tag. Until a release
> exists, build the image yourself (`docker build -t jellyfin-merge-proxy .`)
> and set *Repository* to that tag.

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
    https://raw.githubusercontent.com/Quyzi/jellyfin-merge-proxy/master/unraid/jellyfin-merge-proxy.xml
  ```
  (or add the repository URL under *Docker → Template repositories*, which
  makes Unraid fetch the templates it contains).

What the template sets:

| Setting | Value |
|---|---|
| Network | `bridge` |
| Port | `8096` → container `8096` (what TV apps connect to) |
| Metrics port | optional, advanced; only if `[metrics] listen = "0.0.0.0:9090"` |
| `/etc/jmp.toml` | `/mnt/user/appdata/jellyfin-merge-proxy/jmp.toml` (config file, read-write) |
| `/var/lib/jmp` | `/mnt/user/appdata/jellyfin-merge-proxy/data` (jmp.db, catalog cache, secret.key) |
| `/etc/jmp/certs` | **optional**, advanced, read-only, `Required="false"`; leave it empty |
| `JMP_CONFIG` | `/etc/jmp.toml` (advanced) |
| `JMP_SECRET_KEY` | empty (advanced); optional 64-hex-character encryption key |
| `RUST_LOG` | empty (advanced); e.g. `info,jmp=debug` |
| Extra parameters | `--user 99:100` (Unraid's `nobody:users`) |

The container runs as `99:100` and only writes to `/etc/jmp.toml` (it
rewrites plaintext passwords there as argon2id hashes and `enc:v1:` secrets)
and `/var/lib/jmp`. The image
works with any uid; there is no PUID/PGID variable.

**Certificates are always optional.** Plain HTTP is the default, inbound and
to the upstream servers. Only map `/etc/jmp/certs` if your `jmp.toml` enables
`[tls]` (HTTPS for clients) or a `[server.tls]`/`[upstream_tls]` `ca_path` or
client certificate. HTTPS upstreams with publicly trusted certificates need
no files at all.

## First run

1. Create the appdata folder and the config file **before** the first start.
   `/etc/jmp.toml` is a single-file bind mount: if the host file does not
   exist, Docker creates an empty *directory* in its place. From the Unraid
   terminal:
   ```sh
   mkdir -p /mnt/user/appdata/jellyfin-merge-proxy/data
   cd /mnt/user/appdata/jellyfin-merge-proxy
   wget -O jmp.toml \
     https://raw.githubusercontent.com/Quyzi/jellyfin-merge-proxy/master/config.example.toml
   chown -R 99:100 /mnt/user/appdata/jellyfin-merge-proxy
   ```
2. Edit `jmp.toml` (`nano jmp.toml`):
   * `[proxy]`: keep `listen = "0.0.0.0:8096"`; set `name` (shown in apps).
   * one `[[user]]` per person/TV: the login used in the apps (plaintext or
     an argon2id hash).
   * one `[[server]]` per Jellyfin server: `server = "http://192.168.1.10:8096"`
     plus `user`/`password` (or `api_key`) of an account on that server.
     Use the server's LAN IP, not `localhost` (that is the container itself).
3. Start the container. The log shows one "logged in to upstream" line per
   server. On start the proxy rewrites plaintext `[[user]]` passwords as
   argon2id hashes and upstream passwords/API keys as `enc:v1:` secrets
   (encrypted with `data/secret.key`; back it up). If a folder is not
   writable the log says so, with the uid/gid to `chown` to. Later edits to
   `jmp.toml` are picked up live (no restart), except `listen` and
   `data_dir`.

## TV / client setup

In the Jellyfin app choose *Add server* and enter
`http://<unraid-ip>:8096` (the host port from the template). Log in with a
`[[user]]` from `jmp.toml`, not an account of an upstream server. The
WebUI link opens the Jellyfin web client through the proxy (`/web/`).

## Validate the template

```sh
xmllint --noout unraid/jellyfin-merge-proxy.xml
```
