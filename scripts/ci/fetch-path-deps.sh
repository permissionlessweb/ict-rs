#!/usr/bin/env bash
# Clone path-deps next to this ict-rs checkout.
# ict-rs/ict-rs/Cargo.toml uses ../../<repo> from the inner crate,
# so siblings must live beside the ict-rs repo root.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PARENT="$(cd "$ROOT/.." && pwd)"

ensure() {
  local dest="$1" url="$2" branch="$3"
  if [ -d "$dest/.git" ] || [ -f "$dest/.git" ]; then
    echo "present $dest"
    return 0
  fi
  echo "clone --depth 1 --branch $branch $url -> $dest"
  rm -rf "$dest"
  git clone --depth 1 --branch "$branch" "$url" "$dest"
}

ensure "$PARENT/cosmos-rust"     https://github.com/permissionlessweb/cosmos-rust.git     main
ensure "$PARENT/tendermint-rs"   https://github.com/permissionlessweb/tendermint-rs.git   main
ensure "$PARENT/cw-orchestrator" https://github.com/permissionlessweb/cw-orchestrator.git cw3
ensure "$PARENT/terp-rs"         https://github.com/permissionlessweb/terp-rs.git         feat/zk-wasmvm
ensure "$PARENT/ibc-proto-rs"    https://github.com/permissionlessweb/ibc-proto-rs.git    main
ensure "$PARENT/o-line"           https://github.com/permissionlessweb/o-line.git           master
