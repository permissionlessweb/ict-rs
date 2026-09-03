//! Circuit deposit + runway epoch smoke (ict-rs mock).
//!
//! Go keeper tests (`circuit_runway_epoch_test.go`) are the invariant suite:
//! join/leave bonded set, missed-block weight 0 reallocates, all-offline keeps the bag.
//! This example keeps pay + query covered for CI mock. Docker: same binary with
//! `ICT_MOCK=0` once terpd exposes epoch events (`circuit_val_pool_payout`).
//!
//! ```sh
//! cargo run -p ict-rs --features testing,terp --example circuit_deposit
//! ```

use std::sync::Arc;

use ict_rs::cosmwasm::CosmWasmExt;
use ict_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
    let config = builtin_chain_config("terp")?;
    let mut chain = CosmosChain::new(config, 1, 0, runtime);

    let ctx = TestContext {
        test_name: "circuit-deposit".to_string(),
        network_id: "ict-circuit-deposit".to_string(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;

    let tx = chain.pay_circuit_deposit("validator-0", 1).await?;
    println!("pay-circuit-deposit tx={} height={}", tx.tx_hash, tx.height);

    let q = chain
        .query_circuit_deposit("terp1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq6m2qj3")
        .await?;
    println!("circuit-deposit query: {q}");
    if q["covered"] != true {
        return Err("expected covered=true from mock query".into());
    }

    chain.stop().await?;
    println!("circuit_deposit ok");
    Ok(())
}
