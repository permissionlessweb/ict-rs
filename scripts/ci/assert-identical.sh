#!/usr/bin/env bash
set -euo pipefail
A="${1:?prebuilt}"
B="${2:?rebuilt}"
test -f "$A" && test -f "$B"
ha="$(sha256sum "$A" | awk '{print $1}')"
hb="$(sha256sum "$B" | awk '{print $1}')"
echo "prebuilt $ha  $A"
echo "rebuilt  $hb  $B"
if [ "$ha" != "$hb" ]; then
  echo "ERROR: rebuilt ict-ci does not match prebuilt" >&2
  exit 1
fi
echo "identical"
