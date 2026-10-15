//! Pay and query a circuit-upload deposit on a terp-core image.
//!
//! ```sh
//! cargo run -p ict-rs --features docker,testing,terp --example circuit_deposit
//! ```

use ict_rs::chain::Chain;
use ict_rs::cosmwasm::CosmWasmExt;
use ict_rs::testing::{setup_chain, TestEnv};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("circuit_deposit uses the terp-core image. Unset ICT_MOCK.".into());
    }
    let mut tc = setup_chain("circuit-deposit", TestEnv::terp_config()).await?;
    let payer = tc.chain.key_address("validator").await?;
    let tx = tc.chain.pay_circuit_deposit("validator", 1).await?;
    println!("pay-circuit-deposit tx={} height={}", tx.tx_hash, tx.height);
    let q = tc.chain.query_circuit_deposit(&payer).await?;
    println!("circuit-deposit query: {q}");
    if q.get("covered").is_some() && q["covered"] != true && q["covered"] != "true" {
        tc.cleanup().await?;
        return Err(format!("validator {payer} is not covered: {q}").into());
    }
    tc.cleanup().await?;
    println!("circuit_deposit ok");
    Ok(())
}
