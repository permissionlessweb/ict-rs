# ict-rs

Rust re-implementation of [strangelove-ventures/interchaintest](https://github.com/strangelove-ventures/interchaintest). Docker-based multi-chain integration testing for Cosmos SDK chains.

## Install

Minimal (mock tests only):

```toml
[dependencies]
ict-rs = { path = "ict-rs", default-features = false, features = ["testing"] }
```

Docker-backed tests:

```toml
[dependencies]
ict-rs = { path = "ict-rs", default-features = false, features = ["docker", "testing"] }
```

Everything (default):

```toml
[dependencies]
ict-rs = { path = "ict-rs" }
```

## Quick Start

### Mock test (no Docker needed)

```rust
use std::sync::Arc;
use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, TestContext};
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::spec::builtin_chain_config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let runtime = Arc::new(MockRuntime::new());
    let config = builtin_chain_config("terp")?;
    let mut chain = CosmosChain::new(config, 1, 0, runtime);

    let ctx = TestContext {
        test_name: "my-test".into(),
        network_id: "test-net".into(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;

    println!("Chain {} running", chain.chain_id());
    Ok(())
}
```

```sh
cargo run --example basic_cosmos
```

### Docker test

Same code, swap the runtime:

```rust
use ict_rs::runtime::docker::DockerBackend;

let runtime = Arc::new(DockerBackend::new(Default::default()).await?);
```

```sh
cargo run --example ibc_transfer_e2e --features docker
```

### IBC test

```rust
use ict_rs::interchain::{Interchain, InterchainBuildOptions, InterchainLink};

let mut ic = Interchain::new();
ic.add_chain(chain_a);
ic.add_chain(chain_b);
ic.add_relayer(relayer, "my-relayer");
ic.add_link(InterchainLink {
    chain_a: "terp-1", chain_b: "osmosis-1",
    relayer: "my-relayer", path: "transfer",
});
ic.build(&ctx, InterchainBuildOptions::default()).await?;
```

```sh
cargo run --example ibc_transfer
```

## Workspace

| Crate | Purpose |
|-------|---------|
| `ict-rs` | Core framework: chain, node, runtime, extensions |
| `ict-rs-derive` | Proc macros for typed chain interactions (`ExecuteFns`, `QueryFns`) |
| `ict-rs-codegen` | Proto-to-Rust code generation |
| `ict-rs-cw-orch` | cw-orch adapter |

Rust edition 2021, MSRV 1.88.

## Features

| Feature | What it enables |
|---------|-----------------|
| `full` (default) | docker + ethereum + testing + terp |
| `docker` | Docker runtime via bollard |
| `testing` | TestChain, TestEnv, mock relayer |
| `ethereum` | Anvil/EVM chain support (sha3) |
| `terp` | Terp modules: tokenfactory, feeshare, drip, clock, hashmerchant, smartaccount, globalfee |
| `kuasar` | Lightweight sandbox runtime (tonic) |
| `akash` | Akash chain + oracle |

## Built-in Chains

Defined in `src/spec.rs`, loaded via `builtin_chain_config(name)`:

| Chain | Binary | Denom |
|-------|--------|-------|
| gaia | gaiad | uatom |
| osmosis | osmosisd | uosmo |
| terp | terpd | uterp |
| juno | junod | ujuno |
| akash | akash | uakt |
| anvil | anvil | wei |

## Examples

| Example | Features | Description | Run |
|---------|----------|-------------|-----|
| basic_cosmos | -- | Single chain lifecycle (mock) | `cargo run --example basic_cosmos` |
| ibc_transfer | -- | IBC transfer (mock) | `cargo run --example ibc_transfer` |
| ibc_transfer_e2e | docker | IBC transfer (Docker) | `cargo run --example ibc_transfer_e2e --features docker` |
| integration_test | -- | General integration tests | `cargo run --example integration_test` |
| ibc_hooks | -- | IBC hooks | `cargo run --example ibc_hooks` |
| pfm | -- | Packet forwarding middleware | `cargo run --example pfm` |
| polytone | docker | Cross-chain CosmWasm | `cargo run --example polytone --features docker` |
| cosmos_upgrade | docker | Chain upgrade | `cargo run --example cosmos_upgrade --features docker` |
| hashmerchant | docker,ethereum,hashmerchant | Hash-based trading | `cargo run --example hashmerchant --features docker,ethereum,hashmerchant` |
| no_rick | docker | NFT minting | `cargo run --example no_rick --features docker` |
| headstash | docker | ZK headstash lifecycle | `cargo run --example headstash --features docker` |
| ibc_v2 | -- | IBC v2 with SP1 proving | `cargo run --example ibc_v2` |
| ibc_wasm_lc | -- | IBC wasm light client | `cargo run --example ibc_wasm_lc` |
| trustless_builder | -- | Trustless builder NFT | `cargo run --example trustless_builder` |

## Environment Variables

| Variable | Effect |
|----------|--------|
| `ICT_MOCK=1` | Use mock runtime (no Docker) |
| `ICT_KEEP_CONTAINERS=1` | Keep containers after test |
| `ICT_SHOW_LOGS=1` | Dump container logs on failure |
| `ICT_SHOW_LOGS=always` | Always dump container logs |
| `ICT_IMAGE_REPO` | Override Docker image repo |
| `ICT_IMAGE_VERSION` | Override Docker image version |

## Architecture

Extension traits are blanket-impl'd on any `Chain`:
- **CosmWasmExt** -- store, instantiate, execute, query contracts
- **GovernanceExt** -- submit, vote, query proposals
- **FaucetExt** -- fund accounts via in-container HTTP faucet

Runtimes are pluggable: Docker (default), Mock (`ICT_MOCK=1`), Kuasar.

`ChainSpec` resolves to `ChainConfig` from built-in defaults. The prelude re-exports everything: `use ict_rs::prelude::*;`

## Faucet

Opt-in, in-container HTTP faucet for funding addresses at runtime. Requires a Docker image with Node.js (e.g. `localterp`).

```rust
let cfg = TestEnv::terp_localterp_config(); // config with faucet enabled
let tc = setup_chain("my_test", cfg).await?;
tc.faucet_fund("terp1abc...").await?;
```

For genesis-only funding, use `genesis_wallets` in `start()` instead.

## Terp Modules

Feature-gated under `terp`: tokenfactory, feeshare, drip, clock, hashmerchant, smartaccount, globalfee.

## Claude Skill

An ict-rs skill is available at `.claude/skills/ictrs/`. To install it globally:

```bash
ln -s /path/to/ict-rs/.claude/skills/ictrs ~/.claude/skills/
```

Activates on interchain test, Docker chain test, IBC test, and CosmWasm test topics.

## Development

Requires [just](https://github.com/casey/just).

| Recipe | What it does |
|--------|-------------|
| `just test` | All mock tests, all features |
| `just test-unit` | Unit + lib tests only |
| `just test-file <name>` | Single integration test file |
| `just test-docker` | Docker-backed tests (requires Docker) |
| `just check` | cargo check, all features |
| `just clippy` | Lint, all features, deny warnings |
| `just example <name>` | Run a specific example |
| `just bench` | Run benchmarks |
