---
name: ictrs
description: >
  ict-rs: Rust interchain test framework — Docker-based multi-chain lifecycle,
  IBC relaying (Hermes/CosmosRly), CosmWasm testing, in-container faucet,
  ChainSpec built-ins (gaia, osmosis, terp, juno, akash, anvil), derive macros,
  cw-orch adapter, pluggable runtimes (Docker/Kuasar/Mock), and Terp Network
  module extensions.
  Use for: "ict-rs", "ictrs", "interchain test", "integration test cosmos",
  "docker chain test", "cross-chain test", "IBC test", "CosmWasm test".
---

# ict-rs

Rust re-implementation of [Interchain Test](https://github.com/strangelove-ventures/interchaintest). Spins up Docker containers for cross-chain integration testing.

## Quick Start

```bash
ICT_MOCK=1 cargo test -p ict-rs --features testing,terp    # mock, no Docker
cargo run --example basic_cosmos --features docker          # Docker
```

## Skill Contents

- `references/architecture.md` -- Key patterns, extension traits, runtime backends
- `references/chain-specs.md` -- Built-in chains and ChainSpec usage
- `references/examples.md` -- All runnable examples with feature requirements
- `references/faucet.md` -- In-container faucet: when to use, config, gotchas
- `references/testing-guide.md` -- How to write tests, env vars
- `rules/features.md` -- Feature flags and common combinations
- `rules/terp-modules.md` -- Terp module extensions
- `rules/derive-macros.md` -- ExecuteFns/QueryFns, codegen crate
- `rules/devops/justfile-recipes.md` -- Test, build, lint recipes
