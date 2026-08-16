#!/usr/bin/env bash
# Download prebuilt ict-ci tarball for ICT_RS_COMMIT. Does not compile.
set -euo pipefail
HERE="$(cd "$(dirname "$0")/../.." && pwd)"
DEST="${1:-/tmp/ict-prebuilt}"
eval "$("$HERE/scripts/ci/resolve-prebuilt.sh")"
mkdir -p "$DEST"
echo "==> fetch $ICT_RS_PREBUILT_BASE"
curl -fsSL -o "$DEST/manifest.json" "$ICT_RS_MANIFEST_URL"
curl -fsSL -o "$DEST/${ICT_RS_TARBALL}.sha256" "$ICT_RS_TARBALL_SHA_URL"
curl -fsSL -o "$DEST/${ICT_RS_TARBALL}" "$ICT_RS_TARBALL_URL"
got="$(sha256sum "$DEST/${ICT_RS_TARBALL}" | awk '{print $1}')"
want="$(awk '{print $1}' "$DEST/${ICT_RS_TARBALL}.sha256")"
if [ "$got" != "$want" ]; then
  echo "ERROR: tarball sha256 $got != $want" >&2
  exit 1
fi
tar -C "$DEST" -xzf "$DEST/${ICT_RS_TARBALL}"
chmod +x "$DEST/ict-ci" "$DEST/examples/"*
test -x "$DEST/examples/ibc_transfer"
test -x "$DEST/examples/polytone"
echo "ICT_RS_COMMIT=$ICT_RS_COMMIT" > "$DEST/SOURCE_COMMIT"
echo "ok $DEST ict-ci tarball=$got"
