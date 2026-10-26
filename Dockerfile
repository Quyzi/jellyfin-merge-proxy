# syntax=docker/dockerfile:1
# Multi-stage build on Chainguard images. TLS is rustls + aws-lc-rs (no OpenSSL).

# ---- build ------------------------------------------------------------------
FROM cgr.dev/chainguard/rust:latest AS build
USER root
WORKDIR /src
ARG JMP_GIT_SHA=unknown
ENV JMP_GIT_SHA=${JMP_GIT_SHA}
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY tests ./tests
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
 && cp target/release/jellyfin-merge-proxy /jellyfin-merge-proxy

# ---- runtime ----------------------------------------------------------------
# Distroless-style glibc image: no shell, runs as uid 65532 (nonroot).
FROM cgr.dev/chainguard/glibc-dynamic:latest
COPY --from=build /jellyfin-merge-proxy /usr/local/bin/jellyfin-merge-proxy
COPY config.example.toml /usr/share/jellyfin-merge-proxy/config.example.toml
ENV JMP_CONFIG=/config/config.toml
# /config must contain config.toml; /config/data (data_dir) must be writable by
# uid 65532. Certificates are usually mounted read-only at /config/certs and
# referenced from [tls] / [server.tls] in config.toml.
VOLUME ["/config"]
EXPOSE 8096
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/jellyfin-merge-proxy"]
