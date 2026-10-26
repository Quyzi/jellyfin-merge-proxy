# Grafana dashboard

Screenshots of the **Jellyfin Merge Proxy** dashboard (`deploy/grafana-dashboard.json`),
captured on a lab setup: the proxy in front of three real Jellyfin servers
(`s1`–`s3`, plus `s4` added and removed by hot reload), with about 20 minutes of
generated traffic covering logins, browsing, Range and HLS streaming, config
reloads, a broken config, one upstream stopped for a few minutes and a TLS
certificate reload. See [DASHBOARD_TESTING.md](../DASHBOARD_TESTING.md) for how
to reproduce it.

## Overview

![Overview row](images/dashboard/01-overview.png)

## Client traffic

![Client traffic row](images/dashboard/02-client-traffic.png)

## Upstream servers

`s4` was removed from the config by a hot reload, so it shows as **REMOVED**
rather than down.

![Upstream servers row](images/dashboard/03-upstream-servers.png)

## Request pipeline

![Request pipeline row](images/dashboard/04-request-pipeline.png)

## Streaming

![Streaming row](images/dashboard/05-streaming.png)

## Merge, dedupe and IDs

![Merge, dedupe and IDs row](images/dashboard/06-merge-dedupe-ids.png)

## Catalog cache

![Catalog cache row](images/dashboard/07-catalog-cache.png)

## Clients and auth

![Clients and auth row](images/dashboard/08-clients-auth.png)

## Configuration and TLS

![Configuration and TLS row](images/dashboard/09-configuration-tls.png)

## Process

![Process row](images/dashboard/10-process.png)

## Full dashboard

![Full dashboard](images/dashboard/full.png)
