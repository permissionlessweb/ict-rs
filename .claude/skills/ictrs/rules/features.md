# Feature Flags

## ict-rs Crate Features

| Feature | Dependencies | What it enables |
|---------|-------------|-----------------|
| `default` | `full` | Everything except `kuasar` |
| `full` | `docker,ethereum,testing,terp` | All standard features |
| `docker` | `bollard` | Docker runtime backend (required for real tests) |
| `kuasar` | `tonic` | Lightweight Kuasar sandbox runtime |
| `testing` | — | `TestChain`, `TestEnv`, mock relayer |
| `ethereum` | `sha3` | Anvil/EVM chain support |
| `terp` | (sub-features below) | All Terp Network module extensions |
| `akash` | — | Akash chain + oracle support |

## Terp Sub-Features

Enabled by `terp` feature:

| Feature | Module |
|---------|--------|
| `tokenfactory` | Create/mint/burn native denoms |
| `feeshare` | Fee distribution to contract developers |
| `drip` | Periodic token distribution |
| `clock` | Scheduled contract execution |
| `hashmerchant` | Hash-based token trading |
| `smartaccount` | Account abstraction |
| `globalfee` | Global minimum fee configuration |

## Common Feature Combinations

```toml
# Mock tests (fast, no Docker)
[dev-dependencies]
ict-rs = { path = "...", features = ["testing", "terp"] }

# Docker integration tests
[dev-dependencies]
ict-rs = { path = "...", features = ["docker", "testing", "terp"] }

# Full with Ethereum
[dev-dependencies]
ict-rs = { path = "...", features = ["full"] }

# O-line integration (with Akash)
[dependencies]
ict-rs = { path = "...", features = ["docker", "testing", "akash"], optional = true }
```

## Runtime Selection

```bash
# Mock runtime — no Docker daemon needed
ICT_MOCK=1 cargo test -p ict-rs --features testing,terp

# Docker runtime — requires running Docker daemon
cargo test -p ict-rs --features docker,testing,terp
```
