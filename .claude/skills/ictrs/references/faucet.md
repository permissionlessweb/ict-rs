# Faucet

In-container HTTP token dispenser. Calls `tx bank send` under the hood via Node.js.

## When to Use

- Fund arbitrary addresses at runtime (not just genesis accounts)
- Using `localterp` image (bundles Node.js + `faucet_server.js`)

## When NOT to Use

- Only need genesis-funded accounts — use `genesis_wallets`
- Production images — no Node.js
- Need precise amounts — use `send_funds()` directly

## Setup

```rust
// Recommended for Terp
let cfg = TestEnv::terp_localterp_config();

// Add faucet to any config
let mut cfg = TestEnv::terp_config();
cfg.faucet = Some(FaucetConfig::default());
// Default: key_name="faucet", port=5000, denoms=uterp
```

See `src/chain/mod.rs` for `FaucetConfig` fields.

## Usage

```rust
tc.faucet_fund("terp1abc...").await?;  // FaucetExt trait
tc.faucet_status().await?;
```

## HTTP API (inside container)

- `GET /status` — faucet address, amount, denoms
- `GET /faucet?address=terp1abc` — send tokens, returns txhash

## localterp Image

```bash
cd terp-core && docker buildx build --target localterp -t terpnetwork/terp-core:localterp --load .
```

Pre-funded keys: `validator`, `a`, `b`, `c`, `d`. Ports: 26657 (RPC), 1317 (REST), 5000 (faucet), 9090 (gRPC).
