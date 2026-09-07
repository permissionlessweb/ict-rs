#!/usr/bin/env bash
# Compile bulk-meter-guest with the host rustc (must be ≥ 1.87) to wasm32.
# Output: crates/ict-rs/contracts/bulk-meter-guest/artifacts/bulk_meter_guest.wasm
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GUEST="$ROOT/contracts/bulk-meter-guest"
OUT="$GUEST/artifacts"
mkdir -p "$OUT"
rustc --version
ver="$(rustc --version | awk '{print $2}' | cut -d. -f1,2)"
# crude 1.87+ check
maj="${ver%%.*}"
min="${ver#*.}"
if [ "$maj" -lt 1 ] || { [ "$maj" -eq 1 ] && [ "${min%%.*}" -lt 87 ]; }; then
  echo "ERROR: rustc $ver < 1.87 — this guest must emit memory.copy" >&2
  exit 1
fi
rustup target add wasm32-unknown-unknown >/dev/null
(
  cd "$GUEST"
  cargo build --release --target wasm32-unknown-unknown
)
src="$(echo "$GUEST"/target/wasm32-unknown-unknown/release/*.wasm | awk '{print $1}')"
cp "$src" "$OUT/bulk_meter_guest.wasm"
ls -la "$OUT/bulk_meter_guest.wasm"
echo "ok: rustc $ver artifact at $OUT/bulk_meter_guest.wasm"
