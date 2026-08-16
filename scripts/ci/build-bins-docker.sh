#!/usr/bin/env bash
# Build ict-ci + CI suites inside Docker so the tarball is not host-arch.
# Default: linux/amd64 (GitHub ubuntu-latest) using rust:1.89-bookworm.
#
# Usage:
#   scripts/ci/build-bins-docker.sh [outdir]
#   ICT_RS_BUILD_PLATFORM=linux/arm64 scripts/ci/build-bins-docker.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CRATES="$(cd "$ROOT/.." && pwd)"
OUTDIR="${1:-$ROOT/dist}"
PLATFORM="${ICT_RS_BUILD_PLATFORM:-linux/amd64}"
IMAGE="${ICT_RS_BUILD_IMAGE:-rust:1.89-bookworm}"

case "$PLATFORM" in
  linux/amd64|linux/x86_64) TDIR="target-linux-amd64" ;;
  linux/arm64|linux/aarch64) TDIR="target-linux-arm64" ;;
  *) TDIR="target-docker" ;;
esac

echo "build-bins-docker: image=$IMAGE platform=$PLATFORM crates=$CRATES target=$TDIR"

docker run --rm --platform "$PLATFORM" \
  -v "$CRATES:/crates" \
  -v ict-rs-cargo-registry:/usr/local/cargo/registry \
  -v ict-rs-cargo-git:/usr/local/cargo/git \
  -w /crates/ict-rs \
  -e CARGO_TERM_COLOR=always \
  -e CARGO_TARGET_DIR="/crates/ict-rs/$TDIR" \
  -e PATH="/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
  "$IMAGE" \
  bash -c "set -euo pipefail
    apt-get update
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \\
      pkg-config libssl-dev protobuf-compiler cmake clang git ca-certificates
    cargo build -p ict-rs --release --features docker,terp,testing \\
      --bin ict-ci --example ibc_transfer --example polytone
  "

CARGO_TARGET_DIR="$ROOT/$TDIR" "$ROOT/scripts/ci/pack-bins.sh" "$OUTDIR"
