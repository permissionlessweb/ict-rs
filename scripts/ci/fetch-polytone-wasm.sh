#!/usr/bin/env bash
# Fetch polytone wasm for TERP_CORE_COMMIT. Does not compile contracts.
set -euo pipefail
HERE="$(cd "$(dirname "$0")/../.." && pwd)"
. "$HERE/scripts/ci/terp-core.env"
HOST="${PREBUILT_HOST:-https://minio.terp.network}"
COMMIT="${TERP_CORE_COMMIT:?set TERP_CORE_COMMIT}"
DEST="${1:-$HERE}"
DIR="$DEST/tests/interchaintest/contracts"
BASE="${HOST}/releases/terp-core/commits/${COMMIT}/contracts"
mkdir -p "$DIR"
for f in polytone_note.wasm polytone_voice.wasm polytone_proxy.wasm polytone_tester.wasm; do
  echo "==> fetch $BASE/$f"
  curl -fsSL -o "$DIR/$f" "$BASE/$f"
  test -s "$DIR/$f"
done
echo "ok $DIR commit=$COMMIT"
