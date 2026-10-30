# syntax=docker/dockerfile:1
# Multi-stage build on Chainguard images. TLS is rustls + aws-lc-rs (no OpenSSL).

# ---- build ------------------------------------------------------------------
FROM cgr.dev/chainguard/rust:latest AS build
USER root
WORKDIR /src
ARG JMP_GIT_SHA=unknown
ENV JMP_GIT_SHA=${JMP_GIT_SHA}
COPY Cargo.toml Cargo.lock build.rs config.example.toml ./
COPY src ./src
COPY tests ./tests
# Workspace member (Cargo.toml [workspace]); only built for `cargo xtask`.
COPY xtask ./xtask
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p jellyfin-merge-proxy \
 && cp target/release/jellyfin-merge-proxy /jellyfin-merge-proxy \
 && mkdir -p /jmpdata

# ---- runtime ----------------------------------------------------------------
# Distroless-style glibc image: no shell, runs as uid 65532 (nonroot).
FROM cgr.dev/chainguard/glibc-dynamic:latest
COPY --from=build /jellyfin-merge-proxy /usr/local/bin/jellyfin-merge-proxy
COPY config.example.toml /usr/share/jellyfin-merge-proxy/config.example.toml
COPY --from=build --chown=65532:65532 --chmod=700 /jmpdata /var/lib/jmp
ENV JMP_CONFIG=/etc/jmp.toml
# Runs as any uid (e.g. `--user 99:100` on Unraid); writes to /var/lib/jmp for
# data (ids.tsv, sessions.json, catalog cache, auto-generated secret.key) and
# /etc (config file bind-mount). First start without /etc/jmp.toml writes
# jmp.example.toml next to it (if writable) and exits with instructions. Plain HTTP by
# default: certificates optional, only needed if config enables [tls] or [server.tls].
VOLUME ["/var/lib/jmp"]
EXPOSE 8096
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/jellyfin-merge-proxy"]
