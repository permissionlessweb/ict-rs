#!/usr/bin/env bash
# Upload a packed ict-ci tarball for this ict-rs commit.
#   TARBALL=dist/ict-ci-linux-x86_64.tar.gz scripts/ci/publish-prebuilt-commit.sh
set -euo pipefail
HERE="$(cd "$(dirname "$0")/../.." && pwd)"
COMMIT="${ICT_RS_COMMIT:-$(git -C "$HERE" rev-parse HEAD)}"
COMMIT="$(printf '%s' "$COMMIT" | tr '[:upper:]' '[:lower:]')"
TARBALL="${TARBALL:?set TARBALL to ict-ci-linux-x86_64.tar.gz}"
test -f "$TARBALL"
NAME="$(basename "$TARBALL")"
MINIO_ALIAS="${MINIO_ALIAS:-usb2}"
DEST="${MINIO_ALIAS}/releases/ict-rs/commits/${COMMIT}"
STAGE="$(mktemp -d)"
cp "$TARBALL" "$STAGE/$NAME"
if [ -f "${TARBALL}.sha256" ]; then
  cp "${TARBALL}.sha256" "$STAGE/${NAME}.sha256"
else
  (cd "$STAGE" && sha256sum "$NAME" > "${NAME}.sha256")
fi
echo "$COMMIT" > "$STAGE/SOURCE_COMMIT"
printf '{"commit":"%s","tarball":"%s"}\n' "$COMMIT" "$NAME" > "$STAGE/manifest.json"
mc cp "$STAGE/$NAME" "$STAGE/${NAME}.sha256" "$STAGE/manifest.json" "$STAGE/SOURCE_COMMIT" "${DEST}/"
echo "published ${DEST}"
echo "public https://minio.terp.network/releases/ict-rs/commits/${COMMIT}/${NAME}"
rm -rf "$STAGE"
