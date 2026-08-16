#!/usr/bin/env bash
# Rewrite sibling workspaces so `just ci-build` does not load unused members.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PARENT="$(cd "$ROOT/.." && pwd)"

sed -i.bak "s/\"ict-rs\", \"ict-rs-codegen\", \"ict-rs-cw-orch\", \"ict-rs-derive\"/\"ict-rs\", \"ict-rs-derive\"/" "$ROOT/Cargo.toml"

# cw-orch-core is pulled as a path crate; Cargo still loads its workspace.
# Keep only that member and drop optional zk/halo2 path deps (need the files to exist).
python3 - <<PY
from pathlib import Path
core = Path("$PARENT/cw-orchestrator/packages/cw-orch-core/Cargo.toml")
t = core.read_text()
t = t.replace("default = [\"zk\"]", "default = []")
# comment path-only optional crates so the files need not exist
t = t.replace(
    "zk-cosmwasm = { workspace = true, optional = true }",
    "# zk-cosmwasm omitted for bin release",
)
t = t.replace(
    "halo2_proofs = { package = \"halo2_proofs\", path = \"../../../zcash/halo2/halo2_proofs\", default-features = false, optional = true }",
    "# halo2_proofs omitted for bin release",
)
t = t.replace("zk = [\"cosmwasm-std/zk\", \"dep:zk-cosmwasm\", \"dep:halo2_proofs\", \"terp\"]", "zk = [\"terp\"]")
core.write_text(t)

ws = Path("$PARENT/cw-orchestrator/Cargo.toml")
# keep header through resolver, replace members
text = ws.read_text()
start = text.find("[workspace.package]")
if start < 0:
    raise SystemExit("no workspace.package")
# rewrite members only
import re
text2 = re.sub(
    r"members = \[[\s\S]*?\]",
    "members = [\n    \"packages/cw-orch-core\",\n]",
    text,
    count=1,
)
ws.write_text(text2)
print("slimmed cw-orchestrator + cw-orch-core")
PY

# terp-rs: only the sdk package ict-rs links
if [ -f "$PARENT/terp-rs/Cargo.toml" ]; then
  python3 - <<PY
from pathlib import Path
import re
p = Path("$PARENT/terp-rs/Cargo.toml")
t = p.read_text()
t = re.sub(r"members = \[[\s\S]*?\]", "members = [\"crates/sdk\"]", t, count=1)
p.write_text(t)
print("slimmed terp-rs")
PY
fi
