#!/usr/bin/env bash
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

ensure "$PARENT/cosmos-rust"       https://github.com/permissionlessweb/cosmos-rust.git       main
ensure "$PARENT/tendermint-rs"     https://github.com/permissionlessweb/tendermint-rs.git     main
ensure "$PARENT/cw-orchestrator"   https://github.com/permissionlessweb/cw-orchestrator.git   cw3
ensure "$PARENT/terp-rs"           https://github.com/permissionlessweb/terp-rs.git           feat/zk-wasmvm
ensure "$PARENT/ibc-proto-rs"      https://github.com/permissionlessweb/ibc-proto-rs.git      main
ensure "$PARENT/cosmwasm"          https://github.com/permissionlessweb/cosmwasm.git          v3.1.0-zk.0
ensure "$PARENT/cw-storage-plus"   https://github.com/permissionlessweb/cw-storage-plus.git   v3.1.0-zk.0
ensure "$PARENT/cw-minus"          https://github.com/permissionlessweb/cw-minus.git          main
ensure "$PARENT/cw-multi-test-fork" https://github.com/permissionlessweb/cw-multi-test-fork.git zk-mvp
ensure "$PARENT/ics23"             https://github.com/permissionlessweb/ics23.git             stable
ensure "$PARENT/pbjson"            https://github.com/permissionlessweb/pbjson.git            v3.1.0-zk.0
