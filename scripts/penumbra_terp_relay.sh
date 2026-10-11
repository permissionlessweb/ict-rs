#!/usr/bin/env bash
# Local terp ↔ Penumbra packet relay using the Rotko Hermes fork.
#
# Brings up the existing kms-signed pd devnet (loopback only), a one-node
# Terp chain in Docker, and a host Hermes whose Penumbra stanza points at
# local pd gRPC + CometBFT RPC. Relays one ICS-20 packet terp → pd.
#
# Never joins penumbra-1 / rotko. Outbound from Penumbra is pcli tx withdraw
# (pd rejects MsgTransfer). Round-trip: terp MsgTransfer → hermes RecvPacket
# on pd → pcli withdraw → hermes RecvPacket on terp.
set -euo pipefail

for v in PENUMBRA_PD_JOIN_URL JOIN_RPC PENUMBRA_RPC PD_JOIN HERMES_PENUMBRA_RPC; do
  val="${!v:-}"
  case "$val" in
    *rotko*|*penumbra.zone*|*snapshot.rotko*|*penumbra-1*)
      echo "refusing public penumbra endpoint in $v" >&2
      exit 2
      ;;
  esac
done

: "${PD_BIN:?set PD_BIN to a local pd binary}"
: "${COMETBFT_BIN:?set COMETBFT_BIN to cometbft v0.37.x}"
: "${KMS_BIN:?set KMS_BIN to the cometbft-0.37 kms binary}"
: "${PCLI_BIN:?set PCLI_BIN to pcli}"
: "${HERMES_BIN:?set HERMES_BIN to the rotko hermes binary (not hermes-agent)}"

HERE="$(cd "$(dirname "$0")" && pwd)"
CHAIN_ID="${CHAIN_ID:-penumbra-kms-local}"
TERP_CHAIN_ID="${TERP_CHAIN_ID:-terp-penumbra-e2e}"
TERP_IMAGE="${TERP_IMAGE:-terp-core:v630-arm64}"
TERP_RPC_PORT="${TERP_RPC_PORT:-36657}"
TERP_GRPC_PORT="${TERP_GRPC_PORT:-39090}"
TERP_CONTAINER="${TERP_CONTAINER:-ict-penumbra-terp}"
RPC_PORT="${RPC_PORT:-27657}"
GRPC_PORT="${GRPC_PORT:-28080}"
ROOT="${PENUMBRA_RELAY_ROOT:-$(mktemp -d "${TMPDIR:-/tmp}/penumbra-relay-XXXX")}"
RELAY_ROOT="$ROOT"
PD_ROOT="$ROOT/pd"
RLY_PCLI="$ROOT/rly-pcli"
VIEW_DIR="$ROOT/hermes-view"
HERMES_CFG="$ROOT/hermes.toml"
TERP_HOME="$ROOT/terp"
MNEMONIC_FILE="$ROOT/terp-validator.mnemonic"
AMOUNT="${AMOUNT:-1000}"
PIDS=""
DOCKER_ID=""

cleanup() {
  if [[ -n "${DOCKER_ID:-}" ]]; then
    docker rm -f "$TERP_CONTAINER" >/dev/null 2>&1 || true
  fi
  if [[ -n "${PIDS:-}" ]]; then
    kill $PIDS 2>/dev/null || true
    wait $PIDS 2>/dev/null || true
  fi
}
trap cleanup EXIT

mkdir -p "$ROOT" "$VIEW_DIR" "$TERP_HOME" "$PD_ROOT"
chmod 700 "$ROOT"

if ! "$HERMES_BIN" version >/dev/null 2>&1; then
  echo "HERMES_BIN does not look like ibc-relayer-cli (rotko hermes). Got: $HERMES_BIN" >&2
  echo "Do not use ~/.local/bin/hermes if that is hermes-agent." >&2
  exit 2
fi

echo "workdir $ROOT"
echo "init throwaway relayer pcli (spend key stays in $RLY_PCLI; not printed)"
"$PCLI_BIN" --home "$RLY_PCLI" init --grpc-url "http://127.0.0.1:${GRPC_PORT}" soft-kms generate >/dev/null
ADDR="$("$PCLI_BIN" --home "$RLY_PCLI" view address 0)"
[[ -n "$ADDR" ]] || { echo "pcli view address failed" >&2; exit 1; }
echo "relayer allocation address derived (not printing spend key)"

SPEND_KEY="$(python3 - "$RLY_PCLI/config.toml" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
key = None
for line in text.splitlines():
    s = line.strip()
    if s.startswith("spend_key"):
        key = s.split("=", 1)[1].strip().strip('"').strip("'")
        break
if not key:
    sys.exit("spend_key missing from pcli config")
print(key)
PY
)"
chmod 600 "$RLY_PCLI/config.toml"

echo "starting kms-signed local pd"
KEEP_RUNNING=1 PENUMBRA_ROOT="$PD_ROOT" CHAIN_ID="$CHAIN_ID" \
  RPC_PORT="$RPC_PORT" GRPC_PORT="$GRPC_PORT" \
  ALLOCATION_ADDRESS="$ADDR" \
  PD_BIN="$PD_BIN" COMETBFT_BIN="$COMETBFT_BIN" KMS_BIN="$KMS_BIN" \
  bash "$HERE/penumbra_kms_local.sh" >"$ROOT/kms-suite.log" 2>&1 &
SUITE_PID=$!
ok=0
for _ in $(seq 1 90); do
  if [[ -f "$PD_ROOT/env" ]] && grep -q "LIVE local" "$ROOT/kms-suite.log" 2>/dev/null; then
    ok=1
    break
  fi
  if ! kill -0 "$SUITE_PID" 2>/dev/null; then
    echo "kms suite exited early" >&2
    tail -40 "$ROOT/kms-suite.log" >&2 || true
    exit 1
  fi
  sleep 1
done
if [[ "$ok" != 1 ]]; then
  echo "pd did not become LIVE" >&2
  tail -40 "$ROOT/kms-suite.log" >&2 || true
  exit 1
fi
# shellcheck disable=SC1091
source "$PD_ROOT/env"
PIDS="$SUITE_PID ${PIDS:-}"

echo "starting terp docker image $TERP_IMAGE"
docker rm -f "$TERP_CONTAINER" >/dev/null 2>&1 || true
docker run --rm --entrypoint terpd -v "$TERP_HOME:/data" "$TERP_IMAGE" \
  init relay --chain-id "$TERP_CHAIN_ID" --home /data >/dev/null 2>&1

python3 - "$TERP_HOME" "$TERP_CHAIN_ID" <<'PY'
import json, pathlib, sys
home = pathlib.Path(sys.argv[1])
chain_id = sys.argv[2]
cfg = home / "config" / "config.toml"
app = home / "config" / "app.toml"
genp = home / "config" / "genesis.json"
text = cfg.read_text()
text = text.replace('timeout_commit = "5s"', 'timeout_commit = "1s"')
text = text.replace('timeout_commit = "6s"', 'timeout_commit = "1s"')
text = text.replace('laddr = "tcp://127.0.0.1:26657"', 'laddr = "tcp://0.0.0.0:26657"')
text = text.replace('cors_allowed_origins = []', 'cors_allowed_origins = ["*"]')
cfg.write_text(text)
at = app.read_text()
at = at.replace('minimum-gas-prices = ""', 'minimum-gas-prices = "0uterp"')
at = at.replace('minimum-gas-prices = "0stake"', 'minimum-gas-prices = "0uterp"')
at = at.replace("enable = false", "enable = true", 2)
at = at.replace('address = "tcp://localhost:1317"', 'address = "tcp://0.0.0.0:1317"')
at = at.replace('address = "localhost:9090"', 'address = "0.0.0.0:9090"')
app.write_text(at)
g = json.loads(genp.read_text())
appst = g.setdefault("app_state", {})
if "staking" in appst:
    appst["staking"].setdefault("params", {})["bond_denom"] = "uterp"
    kr = appst["staking"]["params"].get("key_rotation_fee")
    if isinstance(kr, dict):
        kr["denom"] = "uterp"
if "mint" in appst:
    appst["mint"].setdefault("params", {})["mint_denom"] = "uterp"
if "crisis" in appst and isinstance(appst["crisis"].get("constant_fee"), dict):
    appst["crisis"]["constant_fee"]["denom"] = "uterp"
govp = appst.get("gov", {}).get("params", {})
for key in ("min_deposit", "expedited_min_deposit"):
    for c in govp.get(key) or []:
        if isinstance(c, dict) and c.get("denom") in ("stake", "uterp"):
            c["denom"] = "uterp"
g["chain_id"] = chain_id
genp.write_text(json.dumps(g, indent=2) + "\n")
PY

docker run --rm --entrypoint terpd -v "$TERP_HOME:/data" -e HOME=/data "$TERP_IMAGE" \
  keys add validator --keyring-backend test --home /data --output json >"$ROOT/val.json"
python3 - "$ROOT/val.json" "$MNEMONIC_FILE" <<'PY'
import json,sys
j=json.load(open(sys.argv[1]))
open(sys.argv[2],"w").write(j["mnemonic"]+"\n")
print(j["address"], file=sys.stderr)
PY
chmod 600 "$MNEMONIC_FILE" "$ROOT/val.json"
VAL_ADDR="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["address"])' "$ROOT/val.json")"

if docker run --rm --entrypoint terpd -v "$TERP_HOME:/data" "$TERP_IMAGE" genesis --help >/dev/null 2>&1; then
  GEN="genesis"
else
  GEN=""
fi
docker run --rm --entrypoint terpd -v "$TERP_HOME:/data" "$TERP_IMAGE" \
  ${GEN:+$GEN} add-genesis-account "$VAL_ADDR" 1000000000000uterp --home /data
docker run --rm --entrypoint terpd -v "$TERP_HOME:/data" "$TERP_IMAGE" \
  ${GEN:+$GEN} gentx validator 100000000uterp --keyring-backend test \
  --chain-id "$TERP_CHAIN_ID" --home /data
docker run --rm --entrypoint terpd -v "$TERP_HOME:/data" "$TERP_IMAGE" \
  ${GEN:+$GEN} collect-gentxs --home /data >/dev/null

docker run -d --name "$TERP_CONTAINER" \
  -p "127.0.0.1:${TERP_RPC_PORT}:26657" \
  -p "127.0.0.1:${TERP_GRPC_PORT}:9090" \
  -v "$TERP_HOME:/data" \
  --entrypoint terpd "$TERP_IMAGE" \
  start --home /data \
  --rpc.laddr tcp://0.0.0.0:26657 \
  --grpc.enable --grpc.address 0.0.0.0:9090 \
  --api.enable --api.address tcp://0.0.0.0:1317 \
  --minimum-gas-prices 0uterp >/dev/null
DOCKER_ID="$TERP_CONTAINER"

wait_http() {
  local url="$1" label="$2"
  for _ in $(seq 1 90); do
    if curl -fsS --max-time 1 "$url" >/dev/null 2>&1; then
      echo "$label up"
      return 0
    fi
    if [[ "$label" == terp-rpc ]]; then
      if ! docker ps --format '{{.Names}}' | grep -qx "$TERP_CONTAINER"; then
        echo "$label container exited" >&2
        docker logs "$TERP_CONTAINER" 2>&1 | tail -40 >&2 || true
        return 1
      fi
    fi
    sleep 0.5
  done
  echo "$label not up: $url" >&2
  docker logs "$TERP_CONTAINER" 2>&1 | tail -40 >&2 || true
  return 1
}
wait_http "http://127.0.0.1:${TERP_RPC_PORT}/status" terp-rpc
wait_http "http://127.0.0.1:${RPC_PORT}/status" pd-rpc

python3 - "$HERMES_CFG" "$CHAIN_ID" "$RPC_PORT" "$GRPC_PORT" \
  "$TERP_CHAIN_ID" "$TERP_RPC_PORT" "$TERP_GRPC_PORT" "$SPEND_KEY" "$VIEW_DIR" <<'PY'
import sys
path, chain_id, rpc, grpc, terp_id, terp_rpc, terp_grpc, spend, view = sys.argv[1:]
open(path, "w").write(f"""
[global]
log_level = 'info'

[mode.clients]
enabled = true
refresh = true
misbehaviour = true

[mode.connections]
enabled = true

[mode.channels]
enabled = true

[mode.packets]
enabled = true
clear_interval = 10
clear_on_start = true
tx_confirmation = true

[rest]
enabled = false
host = '127.0.0.1'
port = 3000

[telemetry]
enabled = false
host = '127.0.0.1'
port = 3001

[[chains]]
id = '{chain_id}'
type = 'Penumbra'
rpc_addr = 'http://127.0.0.1:{rpc}'
grpc_addr = 'http://127.0.0.1:{grpc}'
rpc_timeout = '15s'
event_source = {{ mode = 'pull', interval = '1s' }}
clock_drift = '15s'
max_block_time = '15s'
kms_config = {{ spend_key = '{spend}' }}
view_service_storage_dir = '{view}'

[[chains]]
id = '{terp_id}'
type = 'CosmosSdk'
rpc_addr = 'http://127.0.0.1:{terp_rpc}'
grpc_addr = 'http://127.0.0.1:{terp_grpc}'
rpc_timeout = '15s'
account_prefix = 'terp'
key_name = 'rly-terp'
store_prefix = 'ibc'
event_source = {{ mode = 'push', url = 'ws://127.0.0.1:{terp_rpc}/websocket', batch_delay = '500ms' }}
gas_price = {{ price = 0.0, denom = 'uterp' }}
gas_multiplier = 1.4
max_gas = 2000000
clock_drift = '15s'
max_block_time = '15s'
trusting_period = '2minutes'
compat_mode = '0.37'
""")
PY
chmod 600 "$HERMES_CFG"
unset SPEND_KEY

HERMES_HOME="$ROOT/hermes-home"
mkdir -p "$HERMES_HOME"
export HOME="$HERMES_HOME"

"$HERMES_BIN" --config "$HERMES_CFG" keys add --chain "$TERP_CHAIN_ID" \
  --key-name rly-terp --mnemonic-file "$MNEMONIC_FILE" >/dev/null

echo "hermes health-check"
"$HERMES_BIN" --config "$HERMES_CFG" health-check

echo "create transfer channel (new clients + connection)"
"$HERMES_BIN" --config "$HERMES_CFG" create channel \
  --a-chain "$TERP_CHAIN_ID" --b-chain "$CHAIN_ID" \
  --a-port transfer --b-port transfer --new-client-connection --yes

start_hermes() {
  "$HERMES_BIN" --config "$HERMES_CFG" start >>"$ROOT/hermes.log" 2>&1 &
  HERMES_PID=$!
  PIDS="$PIDS $HERMES_PID"
  sleep 3
}
stop_hermes() {
  if [[ -n "${HERMES_PID:-}" ]]; then
    kill "$HERMES_PID" 2>/dev/null || true
    wait "$HERMES_PID" 2>/dev/null || true
    PIDS="${PIDS//$HERMES_PID/}"
    HERMES_PID=""
  fi
}

: >"$ROOT/hermes.log"
echo "start hermes"
start_hermes

echo "ics20 terp → penumbra $AMOUNT uterp"
docker exec "$TERP_CONTAINER" terpd tx ibc-transfer transfer transfer channel-0 \
  "$ADDR" "${AMOUNT}uterp" \
  --from validator --keyring-backend test --home /data \
  --chain-id "$TERP_CHAIN_ID" --yes --fees 5000uterp \
  --node tcp://127.0.0.1:26657 >/tmp/ibc-xfer.out 2>&1 || {
    echo "ibc-transfer failed" >&2
    cat /tmp/ibc-xfer.out >&2
    exit 1
  }

echo "waiting for RecvPacket / empty outstanding packets"
ok=0
for _ in $(seq 1 40); do
  if grep -Eiq 'relayed packet|recv_packet|successfully submitted RecvPacket|packet relay' "$ROOT/hermes.log" 2>/dev/null; then
    ok=1
    break
  fi
  if "$HERMES_BIN" --config "$HERMES_CFG" query packet pending \
      --chain "$TERP_CHAIN_ID" --port transfer --channel channel-0 2>/dev/null \
      | grep -Eiq 'unreceived packets: \[\]|no unreceived'; then
    ok=1
    break
  fi
  sleep 2
done

echo "---- hermes tail ----"
tail -30 "$ROOT/hermes.log" || true

if [[ "$ok" != 1 ]]; then
  echo "FAILED: terp→pd packet was not relayed" >&2
  docker logs "$TERP_CONTAINER" 2>&1 | tail -20 >&2 || true
  tail -40 "$PD_ROOT/pd.log" 2>/dev/null >&2 || true
  exit 1
fi
echo "inbound RecvPacket on pd observed"

terp_bal() {
  docker exec "$TERP_CONTAINER" terpd q bank balances "$VAL_ADDR" \
    --output json --node tcp://127.0.0.1:26657 2>/dev/null \
    | python3 -c '
import json,sys
raw=sys.stdin.read().strip()
if not raw:
    print(0); raise SystemExit
d=json.loads(raw)
bals=d.get("balances") or d.get("result",{}).get("balances") or []
one=d.get("balance")
if isinstance(one, dict) and one.get("denom")=="uterp":
    print(one.get("amount") or 0)
    raise SystemExit
for c in bals:
    if c.get("denom")=="uterp":
        print(c.get("amount") or 0)
        raise SystemExit
print(0)
'
}
BAL_AFTER_SEND="$(terp_bal)"
echo "terp validator uterp after send: $BAL_AFTER_SEND"

echo "waiting for pcli view to see inbound IBC notes"
note_ok=0
for _ in $(seq 1 40); do
  BAL="$("$PCLI_BIN" --home "$RLY_PCLI" view balance 2>/dev/null || true)"
  if echo "$BAL" | grep -Eq 'transfer/channel-0/uterp|[0-9]+uterp'; then
    echo "$BAL"
    note_ok=1
    break
  fi
  sleep 2
done
if [[ "$note_ok" != 1 ]]; then
  echo "FAILED: pcli view never showed inbound uterp" >&2
  echo "$BAL" >&2
  exit 1
fi

# Hermes and pcli share the relayer spend key. Stop Hermes so it cannot
# pick the same UM notes while we withdraw.
echo "stop hermes before pcli withdraw (same spend key; pd rejects ICS-20 MsgTransfer)"
stop_hermes

echo "pcli tx withdraw ${AMOUNT}transfer/channel-0/uterp → $VAL_ADDR via channel-0"
"$PCLI_BIN" --home "$RLY_PCLI" tx withdraw \
  --to "$VAL_ADDR" --channel 0 \
  "${AMOUNT}transfer/channel-0/uterp" >"$ROOT/withdraw.log" 2>&1 || {
    echo "FAILED: pcli tx withdraw" >&2
    cat "$ROOT/withdraw.log" >&2
    exit 1
  }
tail -20 "$ROOT/withdraw.log"

echo "restart hermes to RecvPacket on terp"
start_hermes

echo "waiting for pd→terp RecvPacket to credit uterp"
ok=0
BAL_AFTER_RETURN="$BAL_AFTER_SEND"
for _ in $(seq 1 60); do
  BAL_AFTER_RETURN="$(terp_bal)"
  if python3 -c 'import sys; b,a,n=map(int,sys.argv[1:]); sys.exit(0 if a>=b+n else 1)' \
       "$BAL_AFTER_SEND" "$BAL_AFTER_RETURN" "$AMOUNT"; then
    ok=1
    break
  fi
  sleep 2
done

echo "---- hermes tail (return path) ----"
tail -50 "$ROOT/hermes.log" || true

TXHASH="$(python3 - "$ROOT/hermes.log" <<'PY'
import re,sys
h=""
for line in open(sys.argv[1]):
    m=re.search(r"Ok:([0-9A-F]{64}) target_chain=terp-penumbra-e2e", line)
    if m:
        h=m.group(1)
print(h)
PY
)"
if [[ -n "$TXHASH" ]]; then
  echo "terp RecvPacket tx $TXHASH"
  docker exec "$TERP_CONTAINER" terpd q tx "$TXHASH" --output json --node tcp://127.0.0.1:26657 \
    2>/dev/null | python3 -c 'import json,sys
d=json.load(sys.stdin)
print("code", d.get("code"), "raw", (d.get("raw_log") or "")[:500])' || true
fi

echo "terp validator uterp after withdraw: $BAL_AFTER_RETURN"

if [[ "$ok" != 1 ]]; then
  echo "FAILED: expected terp uterp to rise by at least $AMOUNT: $BAL_AFTER_SEND -> $BAL_AFTER_RETURN" >&2
  "$HERMES_BIN" --config "$HERMES_CFG" query packet pending \
    --chain "$CHAIN_ID" --port transfer --channel channel-0 2>&1 | tail -20 >&2 || true
  exit 1
fi
echo "terp uterp recovered $((BAL_AFTER_RETURN - BAL_AFTER_SEND)) (want >= $AMOUNT)"

echo "PASS terp↔penumbra round-trip via rotko hermes + local pd"
echo "ROOT=$RELAY_ROOT"
echo "design: terpd MsgTransfer → hermes RecvPacket on pd; pcli tx withdraw → hermes RecvPacket on terp"
echo "compat: terp SDK 0.55 health-check is Unhealthy/warn not a spawn reject; compat_mode=0.37 is the 0.38/0.39/0.40 RPC dialect (config 0.38 is invalid on this fork)"
