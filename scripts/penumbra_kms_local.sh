#!/usr/bin/env bash
# Local Penumbra devnet whose single validator signs consensus with cosmos/kms.
# Never joins penumbra-1, rotko, or any other public RPC.
set -euo pipefail

for v in PENUMBRA_PD_JOIN_URL JOIN_RPC PENUMBRA_RPC PD_JOIN; do
  val="${!v:-}"
  case "$val" in
    *rotko*|*penumbra.zone*|*penumbra.zone*|*snapshot.rotko*|*penumbra-1*)
      echo "refusing public penumbra endpoint in $v" >&2
      exit 2
      ;;
  esac
done

: "${PD_BIN:?set PD_BIN to a local pd binary}"
: "${COMETBFT_BIN:?set COMETBFT_BIN to cometbft v0.37.x}"
: "${KMS_BIN:?set KMS_BIN to the cometbft-0.37 kms binary}"

CHAIN_ID="${CHAIN_ID:-penumbra-kms-local}"
RPC_PORT="${RPC_PORT:-27657}"
P2P_PORT="${P2P_PORT:-27656}"
ABCI_PORT="${ABCI_PORT:-27658}"
PRIVVAL_PORT="${PRIVVAL_PORT:-27659}"
GRPC_PORT="${GRPC_PORT:-28080}"
METRICS_PORT="${METRICS_PORT:-19000}"
if [[ -n "${PENUMBRA_ROOT:-}" ]]; then
  ROOT="$PENUMBRA_ROOT"
  mkdir -p "$ROOT"
else
  ROOT="$(mktemp -d "${TMPDIR:-/tmp}/penumbra-kms-XXXX")"
fi
NET="$ROOT/network_data"
KMS_HOME="$ROOT/kms"
cleanup() {
  if [[ -n "${PIDS:-}" ]]; then
    kill $PIDS 2>/dev/null || true
    wait $PIDS 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "workdir $ROOT"
GEN_ARGS=(
  network --network-dir "$NET" generate
  --chain-id "$CHAIN_ID"
  --preserve-chain-id
  --timeout-commit 1s
  --epoch-duration 100
  --unbonding-delay "${UNBONDING_DELAY:-500}"
)
if [[ -n "${ALLOCATION_ADDRESS:-}" ]]; then
  GEN_ARGS+=(--allocation-address "$ALLOCATION_ADDRESS")
fi
"$PD_BIN" "${GEN_ARGS[@]}"

NODE="$NET/node0"
CFG="$NODE/cometbft/config/config.toml"
GENESIS="$NODE/cometbft/config/genesis.json"
KEY="$NODE/cometbft/config/priv_validator_key.json"
[[ -f "$KEY" && -f "$GENESIS" && -f "$CFG" ]] || { echo "generate did not write node0"; exit 1; }

ACTUAL_ID="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["chain_id"])' "$GENESIS")"
if [[ "$ACTUAL_ID" != "$CHAIN_ID" ]]; then
  echo "chain id drifted: $ACTUAL_ID" >&2
  exit 1
fi

mkdir -p "$KMS_HOME/state"
"$KMS_BIN" init --home "$KMS_HOME" >/dev/null
cp "$KEY" "$KMS_HOME/priv_validator_key.json"
rm -f "$KEY"
cat >"$KMS_HOME/kms.yaml" <<EOF
chains:
  - id: ${CHAIN_ID}
    state_file: state/${CHAIN_ID}.json
validators:
  - chain_id: ${CHAIN_ID}
    addr: tcp://127.0.0.1:${PRIVVAL_PORT}
    identity_key: identity.json
keys:
  - chain_ids: ["${CHAIN_ID}"]
    backend: file
    algorithm: ed25519
    key_file: priv_validator_key.json
EOF

python3 - "$CFG" "$RPC_PORT" "$P2P_PORT" "$ABCI_PORT" "$PRIVVAL_PORT" <<'PY'
import sys
path, rpc, p2p, abci, privval = sys.argv[1:]
text = open(path).read().splitlines()
out = []
seen_priv = False
for line in text:
    stripped = line.lstrip("# ").strip()
    if stripped.startswith("priv_validator_laddr"):
        out.append(f'priv_validator_laddr = "tcp://127.0.0.1:{privval}"')
        seen_priv = True
    elif stripped.startswith("proxy_app"):
        out.append(f'proxy_app = "tcp://127.0.0.1:{abci}"')
    elif stripped.startswith("laddr = ") and "26657" in stripped:
        out.append(f'laddr = "tcp://127.0.0.1:{rpc}"')
    elif stripped.startswith("laddr = ") and "26656" in stripped:
        out.append(f'laddr = "tcp://127.0.0.1:{p2p}"')
    else:
        out.append(line)
if not seen_priv:
    sys.exit("priv_validator_laddr missing")
open(path, "w").write("\n".join(out) + "\n")
PY

wait_port() {
  local port="$1" label="$2"
  local i
  for i in $(seq 1 40); do
    if python3 -c "import socket,sys; s=socket.create_connection(('127.0.0.1', int(sys.argv[1])), 0.3); s.close()" "$port" 2>/dev/null; then
      echo "$label listening on $port"
      return 0
    fi
    sleep 0.25
  done
  echo "$label did not listen on $port" >&2
  return 1
}

"$PD_BIN" start --home "$NODE/pd" \
  --grpc-bind "127.0.0.1:${GRPC_PORT}" \
  --abci-bind "127.0.0.1:${ABCI_PORT}" \
  --cometbft-addr "http://127.0.0.1:${RPC_PORT}" \
  --metrics-bind "127.0.0.1:${METRICS_PORT}" \
  >"$ROOT/pd.log" 2>&1 &
PIDS="$!"
wait_port "$ABCI_PORT" pd-abci
"$COMETBFT_BIN" start --home "$NODE/cometbft" >"$ROOT/comet.log" 2>&1 &
PIDS="$PIDS $!"
wait_port "$PRIVVAL_PORT" comet-privval
"$KMS_BIN" start --home "$KMS_HOME" --allow-fresh-state "$CHAIN_ID" >"$ROOT/kms.log" 2>&1 &
PIDS="$PIDS $!"

echo "waiting for a signed block (height >= 2)"
ok=0
for _ in $(seq 1 60); do
  if body="$(curl -fsS --max-time 2 "http://127.0.0.1:${RPC_PORT}/status" 2>/dev/null)"; then
    read -r height net <<<"$(python3 -c 'import json,sys; d=json.loads(sys.argv[1])["result"]; print(d["sync_info"]["latest_block_height"], d["node_info"]["network"])' "$body" || true)"
    echo "height=${height:-?} network=${net:-?}"
    if [[ "${net:-}" != "$CHAIN_ID" ]]; then
      echo "status network is not the fresh chain" >&2
      exit 1
    fi
    if [[ "${height:-0}" -ge 2 && "${height:-0}" -lt 1000 ]]; then
      ok=1
      break
    fi
  fi
  sleep 2
done

if [[ "$ok" != 1 ]]; then
  echo "FAILED to reach height 2" >&2
  echo "---- pd ----" >&2
  tail -40 "$ROOT/pd.log" >&2 || true
  echo "---- comet ----" >&2
  tail -40 "$ROOT/comet.log" >&2 || true
  echo "---- kms ----" >&2
  tail -40 "$ROOT/kms.log" >&2 || true
  exit 1
fi

echo "LIVE local chain_id=$CHAIN_ID height=$height"
echo "logs kept at $ROOT"
cat >"$ROOT/env" <<EOF
ROOT=$ROOT
CHAIN_ID=$CHAIN_ID
RPC_PORT=$RPC_PORT
P2P_PORT=$P2P_PORT
ABCI_PORT=$ABCI_PORT
PRIVVAL_PORT=$PRIVVAL_PORT
GRPC_PORT=$GRPC_PORT
METRICS_PORT=$METRICS_PORT
NET=$NET
NODE=$NODE
KMS_HOME=$KMS_HOME
PIDS="$PIDS"
EOF
if [[ "${KEEP_RUNNING:-0}" == 1 ]]; then
  echo "KEEP_RUNNING=1; env at $ROOT/env"
  trap cleanup INT TERM
  wait $PIDS || true
  exit 0
fi
# leave processes for the trap to stop; copy logs path for the caller
trap - EXIT
kill $PIDS 2>/dev/null || true
wait $PIDS 2>/dev/null || true
echo "stopped"
