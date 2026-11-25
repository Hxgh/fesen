#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -f .env.local ]]; then
  while IFS='=' read -r k v; do
    [[ "$k" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
    v="${v%$'\r'}"
    v="${v%\"}"; v="${v#\"}"
    v="${v%\'}"; v="${v#\'}"
    if [[ -z "${!k+x}" ]]; then
      export "$k=$v"
    fi
  done < .env.local
fi

: "${FESEN_LAB_TARGET_IP:?请在环境变量或 .env.local 中设置 FESEN_LAB_TARGET_IP}"
: "${FESEN_LAB_TARGET_PORT:?请在环境变量或 .env.local 中设置 FESEN_LAB_TARGET_PORT}"
FESEN_LAB_TARGET_NAME="${FESEN_LAB_TARGET_NAME:-lab-http}"

# Disposable network namespace; no host mounts, exposed ports or production accounts.
scratch=$(mktemp -d)
cleanup() {
  if [[ -s "$scratch/cid" ]]; then
    docker rm -f "$(cat "$scratch/cid")" >/dev/null 2>&1 || true
  fi
  rm -f "$scratch/cid"
  rmdir "$scratch"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
COPYFILE_DISABLE=1 tar -cf - Cargo.toml Cargo.lock .cargo packages apps/core apps/server scripts/webview |
  docker run --rm --init -i --cidfile "$scratch/cid" --name "fesen-webview-$$" \
    --cap-add NET_ADMIN --device /dev/net/tun --shm-size 256m \
    -e FESEN_WEBVIEW_LAB=1 -e CARGO_PROFILE_TEST_DEBUG=0 \
    -e "FESEN_LAB_TARGET_IP=$FESEN_LAB_TARGET_IP" \
    -e "FESEN_LAB_TARGET_PORT=$FESEN_LAB_TARGET_PORT" \
    -e "FESEN_LAB_TARGET_NAME=$FESEN_LAB_TARGET_NAME" \
    -w /work rust:1.90-bookworm sh -ec '
      tar --warning=no-unknown-keyword -xf -
      sed -i "s|http://deb.debian.org|https://deb.debian.org|g" /etc/apt/sources.list.d/debian.sources
      apt-get -qq update
      DEBIAN_FRONTEND=noninteractive apt-get -y --no-install-recommends install \
        iproute2 python3-gi gir1.2-webkit2-4.1 xvfb xauth fonts-wqy-microhei dbus-daemon >/dev/null
      install -d -m 0700 -o 65534 -g 65534 /tmp/ph-web-home
      # Only the browser UID uses table 176. It cannot fall back to the publisher network.
      for family in -4 -6; do
        ip "$family" route add unreachable default table 176
        ip "$family" rule add priority 100 uidrange 65534-65534 lookup 176
      done
      cargo test --locked -p fesen-server tests::webview::original_address_renders_in_webview \
        -- --ignored --exact --nocapture
    '
