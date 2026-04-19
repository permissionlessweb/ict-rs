# ict-rs

Rust re-implementation of the [Interchain Test](https://github.com/strangelove-ventures/interchaintest) framework. Spins up real Docker chains for integration testing.

## Quick Start

```sh
# Run an example (requires Docker)
cargo run --example basic_cosmos --features docker

# Run tests
cargo test
```

## Features

| Feature    | What it enables                                      |
|------------|------------------------------------------------------|
| `docker`   | Docker runtime backend (default, required for real tests) |
| `testing`  | `TestChain`, `TestEnv`, mock relayer                 |
| `ethereum` | Anvil/EVM chain support                              |
| `terp`     | Terp Network module extensions (tokenfactory, hashmerchant, etc.) |
| `full`     | All of the above                                     |

## Project Layout

```
ict-rs/          Main crate (chain, node, runtime, extensions)
ict-rs-derive/   Proc macros for typed chain interactions
ict-rs-codegen/  Proto-to-Rust code generation
ict-rs-cw-orch/  cw-orch adapter
examples/        Runnable integration test examples
```

## Architecture

```
ChainConfig  ─>  CosmosChain  ─>  ChainNode(s)  ─>  Docker containers
                      │
                      ├── genesis pipeline (keys, accounts, gentx, collect)
                      ├── sidecar processes
                      ├── faucet (optional, in-container)
                      └── IBC via Relayer trait (Hermes / CosmosRly)
```

**Extension traits** add capabilities to any `Chain`:
- `CosmWasmExt` -- store/instantiate/execute/query contracts
- `GovernanceExt` -- submit/vote/query proposals
- `FaucetExt` -- fund accounts via in-container HTTP faucet

All are blanket-impl'd, so any chain gets them automatically.

## Faucet Support

The faucet is an **opt-in, in-container** HTTP token dispenser. It runs inside the chain node container (not a sidecar), calling `terpd tx bank send` under the hood.

### When to use it

- You need to fund arbitrary addresses at runtime (not just genesis accounts)
- You're using the `localterp` Docker image which bundles Node.js + `faucet_server.js`
- You want the same funding workflow in tests that you use in local development

### When NOT to use it

- You only need genesis-funded accounts -- use `genesis_wallets` in `start()`
- You're testing against production images -- they don't have Node.js
- You need precise control over amounts/denoms -- use `send_funds()` directly

### How it works

1. `FaucetConfig` on `ChainConfig` tells the framework to:
   - Create a funded faucet key during genesis
   - Expose the faucet port (default 5000) from the container
   - Start the faucet process after the chain produces blocks
2. `FaucetExt` trait gives you `faucet_fund(address)` and `faucet_status()`
3. Under the hood, these exec `curl` inside the container -- no extra Rust dependencies

### Config

```rust
use ict_rs::prelude::*;

// Option A: Use the convenience helper (recommended for Terp)
let cfg = TestEnv::terp_localterp_config();

// Option B: Add faucet to any existing config
let mut cfg = TestEnv::terp_config();
cfg.faucet = Some(FaucetConfig::default());
// FaucetConfig::default() =
//   key_name:  "faucet"
//   port:      5000
//   start_cmd: ["node", "/code/faucet_server.js"]
//   env:       FAUCET_WALLET_NAME=faucet, FAUCET_AMOUNT=1000000000, DENOMS=uterp

// Option C: Custom faucet config
cfg.faucet = Some(FaucetConfig {
    key_name: "my-faucet".into(),
    port: 5000,
    start_cmd: vec!["node".into(), "/code/faucet_server.js".into()],
    env: vec![
        ("FAUCET_WALLET_NAME".into(), "my-faucet".into()),
        ("FAUCET_AMOUNT".into(), "500000000".into()),
        ("DENOMS".into(), "uterp,uthiol".into()),
    ],
});
```

### Usage in tests

```rust
use ict_rs::prelude::*;

#[tokio::test]
async fn test_faucet_funding() {
    let cfg = TestEnv::terp_localterp_config();
    let tc = setup_chain("faucet_test", cfg).await.unwrap();

    // Create a fresh account
    tc.create_key("alice").await.unwrap();
    let alice_addr = tc.get_address("alice").await.unwrap();
    let alice_bech32 = bech32::encode("terp", &alice_addr).unwrap();

    // Fund it via faucet
    let response = tc.faucet_fund(&alice_bech32).await.unwrap();
    println!("faucet response: {response}");

    // Verify balance
    let balance = tc.get_balance(&alice_bech32, "uterp").await.unwrap();
    assert!(balance > 0);

    tc.cleanup().await.unwrap();
}
```

### Faucet HTTP API (inside container)

| Endpoint | Method | Response |
|----------|--------|----------|
| `/status` | GET | `{"faucet_address":"terp1...","amount":"1000000000","denoms":["uterp","uthiol"]}` |
| `/faucet?address=terp1abc` | GET | `{"txhash":"4E108E..."}` |

### Environment variables (faucet process)

| Variable | Default | Description |
|----------|---------|-------------|
| `FAUCET_WALLET_NAME` | `faucet` (ict-rs) / `a` (standalone) | Keyring key name to send from |
| `FAUCET_AMOUNT` | `1000000000` | Amount per denom per request |
| `DENOMS` | `uterp` (ict-rs) / `uterp,uthiol` (standalone) | Comma-separated denoms to send |

## Terp Network: localterp Workflow

### 1. Build the image

```sh
cd terp-core
docker buildx build --target localterp -t terpnetwork/terp-core:localterp --load .
```

This produces a ~400MB image with Node.js and the faucet. Compare to `--target runtime` (~200MB, no faucet).

### 2. Run standalone (no ict-rs)

```sh
docker run --rm -it \
  -p 26657:26657 -p 1317:1317 -p 5000:5000 -p 9090:9090 \
  terpnetwork/terp-core:localterp
```

Pre-funded keys: `validator`, `a`, `b`, `c`, `d` (all with 1e18 uterp+uthiol).

```sh
curl localhost:5000/status
curl "localhost:5000/faucet?address=terp1youraddr"
```

### 3. Run via ict-rs

```rust
use ict_rs::prelude::*;
use ict_rs::testing::{setup_chain, TestEnv};

let cfg = TestEnv::terp_localterp_config();
let tc = setup_chain("my_test", cfg).await?;

// Fund any address at runtime
tc.faucet_fund("terp1abc...").await?;

// Check faucet status
let status = tc.faucet_status().await?;
println!("{status}");
```

### 4. Run via ict-rs with IBC

```rust
let mut terp_a = TestEnv::terp_localterp_config();
terp_a.chain_id = "terp-a-1".into();

let terp_b = TestEnv::terp_config(); // no faucet on chain B
terp_b.chain_id = "terp-b-1".into();

// Use faucet on chain A, send_funds on chain B
```

## Examples

| Example | What it does | Run |
|---------|-------------|-----|
| `basic_cosmos` | Single chain lifecycle (mock) | `cargo run --example basic_cosmos` |
| `ibc_transfer` | IBC transfer (mock) | `cargo run --example ibc_transfer` |
| `ibc_transfer_e2e` | IBC transfer (Docker) | `cargo run --example ibc_transfer_e2e --features docker` |
| `polytone` | Cross-chain CosmWasm (Docker) | `cargo run --example polytone --features docker` |
| `cosmos_upgrade` | Chain upgrade (Docker) | `cargo run --example cosmos_upgrade --features docker` |
| `hashmerchant` | Hashmerchant module (Docker) | `cargo run --example hashmerchant --features docker,ethereum,hashmerchant` |
| `no_rick` | NFT minting (Docker) | `cargo run --example no_rick --features docker` |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `ICT_MOCK=1` | unset | Use mock runtime (no Docker) |
| `ICT_KEEP_CONTAINERS=1` | unset | Don't clean up containers after test |
| `ICT_SHOW_LOGS=1` | unset | Dump container logs on failure |
| `ICT_SHOW_LOGS=always` | unset | Always dump container logs |
| `ICT_IMAGE_REPO` | `terpnetwork/terp-core` | Override Docker image repo |
| `ICT_IMAGE_VERSION` | `local-zk` | Override Docker image version |
