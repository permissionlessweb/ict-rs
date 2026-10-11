#!/usr/bin/env bash
# Pack prebuilt ict-ci + CI suite examples into the layout CI extracts.
# Suite names come from ict-ci.rs via suites.sh.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
ICT_RS_ROOT="$ROOT"
# shellcheck source=suites.sh
. "$(cd "$(dirname "$0")" && pwd)/suites.sh"
export ICT_RS_ROOT
OUTDIR="${1:-$ROOT/dist}"
REL="${CARGO_TARGET_DIR:-$ROOT/target}/release"
HOST="$(uname -s | tr "[:upper:]" "[:lower:]")"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ARCH=x86_64 ;;
  arm64|aarch64) ARCH=aarch64 ;;
esac
NAME="${ICT_RS_TARBALL_NAME:-ict-ci-${HOST}-${ARCH}.tar.gz}"

ict_ci_assert_examples_declared
need=("$REL/ict-ci")
while IFS= read -r s; do
  [ -n "$s" ] || continue
  need+=("$REL/examples/$s")
done < <(ict_ci_suites)

for f in "${need[@]}"; do
  if [ ! -x "$f" ]; then
    echo "missing executable: $f (run: just ci-build or scripts/ci/build-bins-docker.sh)" >&2
    exit 1
  fi
done

STAGE="$(mktemp -d)"
mkdir -p "$STAGE/examples"
cp "${need[0]}" "$STAGE/ict-ci"
for f in "${need[@]:1}"; do
  cp "$f" "$STAGE/examples/$(basename "$f")"
done
chmod +x "$STAGE/ict-ci" "$STAGE/examples/"*
mkdir -p "$OUTDIR"
tar -C "$STAGE" -czf "$OUTDIR/$NAME" ict-ci examples
rm -rf "$STAGE"
if command -v shasum >/dev/null; then
  (cd "$OUTDIR" && shasum -a 256 "$NAME" > "${NAME}.sha256")
elif command -v sha256sum >/dev/null; then
  (cd "$OUTDIR" && sha256sum "$NAME" > "${NAME}.sha256")
fi
ls -lh "$OUTDIR/$NAME"
echo "$OUTDIR/$NAME"
