#!/usr/bin/env bash
# Start three real Jellyfin servers in containers (host network) on
# $BASE_PORT+1..3 (default 8101-8103), set them up through the API and, if
# openssl is available, give server 3 HTTPS on 8923 with a private CA.
# Needs: podman or docker (JMP_RUNTIME), curl, jq, ffmpeg (for make-media.sh).
# usage: start-servers.sh [work-dir]       (default /srv/jf)
# env:   BASE_PORT (8100), HTTPS_PORT (8923), JF_IMAGE (jellyfin/jellyfin:latest),
#        NAME_PREFIX (jf), JMP_RUNTIME ("sudo podman")
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
W=${1:-/srv/jf}
BASE_PORT=${BASE_PORT:-8100}; HTTPS_PORT=${HTTPS_PORT:-8923}
IMAGE=${JF_IMAGE:-docker.io/jellyfin/jellyfin:latest}
P=${NAME_PREFIX:-jf}; RT=${JMP_RUNTIME:-sudo podman}
mkdir -p "$W"
"$HERE/make-media.sh" "$W/media" >/dev/null

# Test PKI: CA + server cert for localhost/127.0.0.1 (jf3) + proxy listener cert.
PKI=$W/pki
if command -v openssl >/dev/null && [ ! -f "$PKI/ca.pem" ]; then
  mkdir -p "$PKI"
  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=JMP test CA" \
    -keyout "$PKI/ca.key" -out "$PKI/ca.pem" 2>/dev/null
  for n in jf3 proxy; do
    printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' >"$PKI/$n.ext"
    openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout "$PKI/$n.key" -out "$PKI/$n.csr" 2>/dev/null
    openssl x509 -req -in "$PKI/$n.csr" -CA "$PKI/ca.pem" -CAkey "$PKI/ca.key" -CAcreateserial \
      -days 3650 -extfile "$PKI/$n.ext" -out "$PKI/$n.pem" 2>/dev/null
  done
  openssl pkcs12 -export -passout pass: -inkey "$PKI/jf3.key" -in "$PKI/jf3.pem" -out "$PKI/jf3.pfx"
fi

for i in 1 2 3; do
  port=$((BASE_PORT + i)); cfg=$W/s$i/config/config
  mkdir -p "$cfg" "$W/s$i/cache"
  https=false; cert=""
  if [ "$i" = 3 ] && [ -f "$PKI/jf3.pfx" ]; then
    cp "$PKI/jf3.pfx" "$W/s$i/config/jf3.pfx"; chmod 644 "$W/s$i/config/jf3.pfx"
    https=true; cert=/config/jf3.pfx
  fi
  # Seed network.xml before the first start (Jellyfin keeps unknown defaults).
  [ -f "$cfg/network.xml" ] || cat >"$cfg/network.xml" <<XML
<?xml version="1.0" encoding="utf-8"?>
<NetworkConfiguration xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <EnableHttps>$https</EnableHttps>
  <RequireHttps>false</RequireHttps>
  <CertificatePath>$cert</CertificatePath>
  <InternalHttpPort>$port</InternalHttpPort>
  <InternalHttpsPort>$HTTPS_PORT</InternalHttpsPort>
  <PublicHttpPort>$port</PublicHttpPort>
  <PublicHttpsPort>$HTTPS_PORT</PublicHttpsPort>
  <EnableRemoteAccess>true</EnableRemoteAccess>
</NetworkConfiguration>
XML
  if ! $RT container exists "$P$i" 2>/dev/null; then
    $RT run -d --name "$P$i" --network host -v "$W/s$i/config:/config" -v "$W/s$i/cache:/cache" \
      -v "$W/media/s$i:/media:ro" "$IMAGE" >/dev/null
  else
    $RT start "$P$i" >/dev/null
  fi
done

U=http://127.0.0.1
"$HERE/setup-server.sh" "$U:$((BASE_PORT + 1))" admin adminpw "Movies|movies|/media/Movies" "Shows|tvshows|/media/Shows" "Kids|movies|/media/Kids" >/dev/null
"$HERE/setup-server.sh" "$U:$((BASE_PORT + 2))" admin adminpw "Movies|movies|/media/Movies" "Shows|tvshows|/media/Shows" >/dev/null
"$HERE/setup-server.sh" "$U:$((BASE_PORT + 3))" admin adminpw "Movies|movies|/media/Movies" "Shows|movies|/media/Shows" >/dev/null
for i in 1 2 3; do
  echo "$P$i: $U:$((BASE_PORT + i)) $(curl -s "$U:$((BASE_PORT + i))/System/Info/Public" | jq -r .Version)"
done
[ -f "$PKI/jf3.pfx" ] && echo "${P}3 HTTPS: https://localhost:$HTTPS_PORT (CA $PKI/ca.pem)"
echo "admin: admin/adminpw   proxy upstream user: ${JMP_UPSTREAM_USER:-jmp}/${JMP_UPSTREAM_PASS:-jmppw}"
