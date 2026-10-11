#!/usr/bin/env bash
# Curate ict-ci tarballs for a checkpoint (commit-keyed) or a release (tag-keyed).
#
# Heuristics (fail-closed):
#   1. Suite list is parsed from ict-ci.rs (not a second copy).
#   2. Every suite must be an [[example]] in ict-rs/Cargo.toml.
#   3. Packed tree must contain ict-ci + every suite binary, executable.
#   4. linux/amd64 tarball binaries must be ELF x86-64.
#   5. Checkpoint: object key commits/<full-sha>/.
#   6. Release: also copy to ict-rs/<tag>/ (tag required).
#
# Usage:
#   scripts/ci/curate-tarball.sh checkpoint              # build+pack+verify, no upload
#   scripts/ci/curate-tarball.sh checkpoint --publish    # + MinIO commits/<sha>
#   RELEASE_TAG=v0.2.0 scripts/ci/curate-tarball.sh release --publish
#   SKIP_BUILD=1 scripts/ci/curate-tarball.sh checkpoint  # pack existing target dir
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ICT_RS_ROOT="$(cd "$HERE/../.." && pwd)"
# shellcheck source=suites.sh
. "$HERE/suites.sh"
export ICT_RS_ROOT

MODE="${1:-}"
PUBLISH=0
shift || true
for a in "$@"; do
  case "$a" in
    --publish) PUBLISH=1 ;;
    *) echo "unknown arg: $a" >&2; exit 2 ;;
  esac
done

case "$MODE" in
  checkpoint|release) ;;
  *)
    echo "usage: $0 checkpoint|release [--publish]" >&2
    exit 2
    ;;
esac

COMMIT="$(git -C "$ICT_RS_ROOT" rev-parse HEAD)"
COMMIT="$(printf '%s' "$COMMIT" | tr '[:upper:]' '[:lower:]')"
if [ -n "$(git -C "$ICT_RS_ROOT" status --porcelain)" ] && [ "${ALLOW_DIRTY:-0}" != "1" ]; then
  echo "worktree dirty; set ALLOW_DIRTY=1 to curate anyway" >&2
  git -C "$ICT_RS_ROOT" status -sb | head -20 >&2
  exit 1
fi

ict_ci_assert_examples_declared
mapfile -t SUITES < <(ict_ci_suites)
if [ "${#SUITES[@]}" -lt 2 ]; then
  echo "parsed fewer than 2 suites from ict-ci.rs — refusing" >&2
  exit 1
fi
echo "suites: ${SUITES[*]}"

OUTDIR="${ICT_RS_DIST:-$ICT_RS_ROOT/dist}"
PLATFORM="${ICT_RS_BUILD_PLATFORM:-linux/amd64}"
case "$PLATFORM" in
  linux/amd64|linux/x86_64) TARBALL_NAME=ict-ci-linux-x86_64.tar.gz; WANT_ELF=1 ;;
  *) TARBALL_NAME="ict-ci-${PLATFORM//\//-}.tar.gz"; WANT_ELF=0 ;;
esac

if [ "${SKIP_BUILD:-0}" != "1" ]; then
  echo "build $PLATFORM"
  ICT_RS_BUILD_PLATFORM="$PLATFORM" "$HERE/build-bins-docker.sh" "$OUTDIR"
else
  echo "SKIP_BUILD=1 — packing existing artifacts"
  ICT_RS_TARBALL_NAME="$TARBALL_NAME" "$HERE/pack-bins.sh" "$OUTDIR"
fi

TAR="$OUTDIR/$TARBALL_NAME"
test -f "$TAR"
if [ ! -f "${TAR}.sha256" ]; then
  if command -v shasum >/dev/null; then
    (cd "$OUTDIR" && shasum -a 256 "$TARBALL_NAME" > "${TARBALL_NAME}.sha256")
  else
    (cd "$OUTDIR" && sha256sum "$TARBALL_NAME" > "${TARBALL_NAME}.sha256")
  fi
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
tar -C "$STAGE" -xzf "$TAR"
test -x "$STAGE/ict-ci"
for s in "${SUITES[@]}"; do
  if [ ! -x "$STAGE/examples/$s" ]; then
    echo "tarball missing executable examples/$s" >&2
    tar -tzf "$TAR" >&2
    exit 1
  fi
  if [ "$WANT_ELF" = "1" ]; then
    desc="$(file -b "$STAGE/examples/$s")"
    case "$desc" in
      *ELF*x86-64*|*ELF*x86_64*) ;;
      *)
        echo "examples/$s is not ELF x86-64: $desc" >&2
        exit 1
        ;;
    esac
  fi
done
if [ "$WANT_ELF" = "1" ]; then
  desc="$(file -b "$STAGE/ict-ci")"
  case "$desc" in
    *ELF*x86-64*|*ELF*x86_64*) ;;
    *) echo "ict-ci is not ELF x86-64: $desc" >&2; exit 1 ;;
  esac
fi

SUM="$(awk '{print $1}' "${TAR}.sha256")"
MANIFEST="$OUTDIR/curate-manifest.json"
{
  printf '{'
  printf '"mode":"%s",' "$MODE"
  printf '"commit":"%s",' "$COMMIT"
  printf '"tarball":"%s",' "$TARBALL_NAME"
  printf '"sha256":"%s",' "$SUM"
  printf '"suites":['
  first=1
  for s in "${SUITES[@]}"; do
    [ "$first" = 1 ] || printf ','
    first=0
    printf '"%s"' "$s"
  done
  printf ']'
  if [ "$MODE" = "release" ]; then
    printf ',"tag":"%s"' "${RELEASE_TAG:?set RELEASE_TAG for release}"
  fi
  printf '}\n'
} > "$MANIFEST"
echo "curated $TAR"
echo "sha256 $SUM"
cat "$MANIFEST"

if [ "$PUBLISH" != "1" ]; then
  echo "dry: no upload (pass --publish)"
  exit 0
fi

command -v mc >/dev/null || { echo "mc not on PATH" >&2; exit 1; }
MINIO_ALIAS="${MINIO_ALIAS:-usb2}"
export ICT_RS_COMMIT="$COMMIT"
export TARBALL="$TAR"
"$HERE/publish-prebuilt-commit.sh"

# Point the pin at the SHA we just published (file only; does not git commit).
PIN="$HERE/prebuilt.env"
{
  echo "# SHA that produced the published linux-x86_64 tarball."
  echo "# Move this only after publishing that SHA to MinIO."
  echo "ICT_RS_COMMIT=${COMMIT}"
  echo "ICT_RS_TARBALL=${TARBALL_NAME}"
} > "$PIN"
echo "updated $PIN"

if [ "$MODE" = "release" ]; then
  TAG="${RELEASE_TAG:?}"
  DEST="${MINIO_ALIAS}/releases/ict-rs/${TAG}"
  mc cp "$TAR" "${TAR}.sha256" "$MANIFEST" "${DEST}/"
  echo "published ${DEST}"
  echo "public https://minio.terp.network/releases/ict-rs/${TAG}/${TARBALL_NAME}"
fi
