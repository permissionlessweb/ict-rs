# ict-rs Architecture

## Workspace

| Crate | Purpose |
|-------|---------|
| `ict-rs` | Main — chain, node, runtime, extensions |
| `ict-rs-derive` | Proc macros: `ExecuteFns`, `QueryFns` |
| `ict-rs-codegen` | Proto-to-Rust code generation |
| `ict-rs-cw-orch` | cw-orch adapter |

## Flow

```
ChainConfig  ->  CosmosChain  ->  ChainNode(s)  ->  Docker containers
                      |
                      +-- genesis pipeline
                      +-- sidecar processes
                      +-- faucet (optional)
                      +-- IBC via Relayer trait
```

## Extension Traits (Blanket-Impl'd)

Any chain automatically gets these — no manual impl needed:

- `CosmWasmExt` — store, instantiate, execute, query contracts
- `GovernanceExt` — submit/vote/query proposals
- `FaucetExt` — fund accounts via in-container HTTP faucet

## Key Modules

- `chain/mod.rs` — `Chain` trait, `ChainConfig`, `ChainType`, `FaucetConfig`, `SidecarConfig`
- `cosmos/interchain.rs` — `Interchain` struct, `InterchainLink`, multi-chain orchestration
- `runtime/docker.rs` — `DockerBackend` (bollard). `runtime/mock.rs` for testing without Docker
- `spec.rs` — `ChainSpec`, `builtin_chain_config()` for gaia/osmosis/terp/juno/akash/anvil
- `relayer/` — `Relayer` trait with Hermes and CosmosRly impls

## Gotchas

- `cosmrs` and `tendermint` deps use `zk-mvp` branch forks — not upstream crates.io
- `cw-orch-core` also uses `zk-mvp` branch
- GenesisStyle matters: Akash uses `Modern` (SDK 0.50+), most others use `Legacy`
- `ICT_MOCK=1` env var switches to mock runtime — read source for what's stubbed

## Prelude

`use ict_rs::prelude::*;` re-exports all common types. See `src/lib.rs` for the full list.
