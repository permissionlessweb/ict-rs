# ict-rs Examples

All examples are in the `examples/` directory at workspace root.

## Example Index

| Example | What it does | Required features | Run command |
|---------|-------------|-------------------|-------------|
| `basic_cosmos` | Single chain lifecycle (mock) | — | `cargo run --example basic_cosmos` |
| `ibc_transfer` | IBC transfer (mock) | — | `cargo run --example ibc_transfer` |
| `ibc_transfer_e2e` | IBC transfer (Docker) | `docker` | `cargo run --example ibc_transfer_e2e --features docker` |
| `integration_test` | General integration test | — | `cargo run --example integration_test` |
| `polytone` | Cross-chain CosmWasm (Docker) | `docker` | `cargo run --example polytone --features docker` |
| `cosmos_upgrade` | Chain upgrade (Docker) | `docker` | `cargo run --example cosmos_upgrade --features docker` |
| `hashmerchant` | Hashmerchant module (Docker) | `docker,ethereum,hashmerchant` | `cargo run --example hashmerchant --features docker,ethereum,hashmerchant` |
| `no_rick` | NFT minting (Docker) | `docker` | `cargo run --example no_rick --features docker` |
| `headstash` | Headstash ZK test (Docker) | `docker` | `cargo run --example headstash --features docker` |
| `pfm` | Packet forwarding middleware | — | `cargo run --example pfm` |
| `ibc_hooks` | IBC hooks | — | `cargo run --example ibc_hooks` |
| `ibc_v2` | IBC v2 protocol | — | `cargo run --example ibc_v2` |
| `ibc_wasm_lc` | IBC wasm light client | — | `cargo run --example ibc_wasm_lc` |
| `trustless_builder` | Trustless builder | — | `cargo run --example trustless_builder` |

## Running with Just

```bash
# Run any example
just example basic_cosmos
just example ibc_transfer_e2e
just example hashmerchant
```

## Test Files

| Test file | What it tests | Features |
|-----------|--------------|----------|
| `tests/unit_tests.rs` | Core unit tests | testing,terp |
| `tests/integration_tests.rs` | Integration tests | testing,terp |
| `tests/genesis_validation.rs` | Genesis pipeline | docker,testing,terp |
| `tests/cleanup_tests.rs` | Container cleanup | docker,testing,terp |
| `tests/ibc_transfer_test.rs` | IBC transfer | testing |
| `tests/relayer_tests.rs` | Relayer lifecycle | testing |
| `tests/derive_integration.rs` | Derive macro tests | testing |
| `tests/terp_tokenfactory.rs` | Terp tokenfactory | testing,terp |
| `tests/anvil_tests.rs` | Ethereum/Anvil | ethereum |

## Benchmarks

| Benchmark | File |
|-----------|------|
| `crypto_bench` | `benches/crypto_bench.rs` |
| `runtime_bench` | `benches/runtime_bench.rs` |

```bash
just bench
```
