#!/usr/bin/env bash
# Fetch terp-core prebuilt image for TERP_CORE_COMMIT. Does not compile terpd.
# Layout: $PREBUILT_HOST/releases/terp-core/commits/<sha>/
set -euo pipefail
HERE="$(cd "$(dirname "$0")/../.." && pwd)"
. "$HERE/scripts/ci/terp-core.env"
HOST="${PREBUILT_HOST:-https://minio.terp.network}"
COMMIT="${TERP_CORE_COMMIT:?set TERP_CORE_COMMIT in terp-core.env}"
TAR="${TERP_IMAGE_TAR:-terp-core-local-linux-amd64.tar}"
DEST="${1:-/tmp/terp-prebuilt}"
BASE="${HOST}/releases/terp-core/commits/${COMMIT}"
mkdir -p "$DEST"
echo "==> fetch $BASE/$TAR"
curl -fsSL -o "$DEST/manifest.json" "${BASE}/manifest.json"
curl -fsSL -o "$DEST/${TAR}" "${BASE}/${TAR}"
if curl -fsSL -o "$DEST/terpd-linux-amd64.sha256" "${BASE}/terpd-linux-amd64.sha256"; then
  curl -fsSL -o "$DEST/terpd-linux-amd64" "${BASE}/terpd-linux-amd64"
  got="$(sha256sum "$DEST/terpd-linux-amd64" | awk "{print \$1}")"
  want="$(awk "{print \$1}" "$DEST/terpd-linux-amd64.sha256")"
  if [ "$got" != "$want" ]; then
    echo "ERROR: terpd sha256 $got != $want" >&2
    exit 1
  fi
  chmod +x "$DEST/terpd-linux-amd64"
  echo "terpd=$got"
fi
echo "$COMMIT" > "$DEST/SOURCE_COMMIT"
echo "ok $DEST commit=$COMMIT image=$TAR"
