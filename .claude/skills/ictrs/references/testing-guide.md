# Testing Guide

## Single Chain

```rust
use ict_rs::prelude::*;
use ict_rs::testing::{setup_chain, TestEnv};

#[tokio::test]
async fn test_basic() {
    let cfg = TestEnv::terp_config();
    let tc = setup_chain("my_test", cfg).await.unwrap();
    // create keys, send funds, query balances...
    tc.cleanup().await.unwrap();
}
```

## IBC (Multi-Chain)

Use `Interchain` to wire up chains + relayers + IBC paths. See `examples/ibc_transfer.rs` and `examples/ibc_transfer_e2e.rs` for working patterns.

Key types: `Interchain`, `InterchainLink`, `InterchainBuildOptions`, `wait_for_blocks`.

IBC denom helpers: `ibc_denom("transfer", "channel-0", "uatom")`, `ibc_denom_multi_hop`.

## Environment Variables

| Variable | Description |
|----------|-------------|
| `ICT_MOCK=1` | Mock runtime (no Docker) |
| `ICT_KEEP_CONTAINERS=1` | Don't clean up after test |
| `ICT_SHOW_LOGS=1` | Dump logs on failure |
| `ICT_SHOW_LOGS=always` | Always dump logs |
| `ICT_IMAGE_REPO` | Override image repo |
| `ICT_IMAGE_VERSION` | Override image version |
