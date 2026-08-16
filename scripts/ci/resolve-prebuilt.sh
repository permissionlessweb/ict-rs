#!/usr/bin/env bash
# Resolve prebuilt ict-ci URLs for an ict-rs git commit.
# Layout: $PREBUILT_HOST/releases/ict-rs/commits/<full-sha>/
set -euo pipefail
HERE="$(cd "$(dirname "$0")/../.." && pwd)"
PREBUILT_HOST="${PREBUILT_HOST:-https://minio.terp.network}"
# Pin file wins over GITHUB_SHA so a CI-script commit does not 404.
if [ -z "${ICT_RS_COMMIT:-}" ] && [ -f "$HERE/scripts/ci/prebuilt.env" ]; then
  # shellcheck disable=SC1091
  . "$HERE/scripts/ci/prebuilt.env"
fi
COMMIT="${ICT_RS_COMMIT:-${PREBUILT_COMMIT:-}}"
if [ -z "$COMMIT" ]; then
  COMMIT="$(git -C "$HERE" rev-parse HEAD 2>/dev/null || true)"
fi
if [ -z "$COMMIT" ]; then
  echo "ERROR: set ICT_RS_COMMIT or run from an ict-rs git checkout" >&2
  exit 1
fi
COMMIT="$(printf '%s' "$COMMIT" | tr '[:upper:]' '[:lower:]')"
BASE="${PREBUILT_HOST}/releases/ict-rs/commits/${COMMIT}"
TARBALL="${ICT_RS_TARBALL:-ict-ci-linux-x86_64.tar.gz}"
echo "ICT_RS_COMMIT=${COMMIT}"
echo "ICT_RS_PREBUILT_BASE=${BASE}"
echo "ICT_RS_TARBALL=${TARBALL}"
echo "ICT_RS_TARBALL_URL=${BASE}/${TARBALL}"
echo "ICT_RS_TARBALL_SHA_URL=${BASE}/${TARBALL}.sha256"
echo "ICT_RS_MANIFEST_URL=${BASE}/manifest.json"
