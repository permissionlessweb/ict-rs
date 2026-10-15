//! Abstract-account surface on a terp-core image.
//!
//! The chain module is `x/smart-account`. This queries params, funds an
//! account, and queries its authenticator list. Contract wasm for
//! `crates/abstract-account` is not in `artifacts/` yet, so this does not
//! pretend to register a before_tx account.
//!
//! ```sh
//! cargo run -p ict-rs --example abstract_account --features docker,terp,testing
//! ```

use ict_rs::prelude::*;
use ict_rs::testing::{setup_chain, TestEnv};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("abstract_account uses the terp-core image. Unset ICT_MOCK.".into());
    }
    let mut tc = setup_chain("abstract-account", TestEnv::terp_config()).await?;
    let outcome = run(&mut tc).await;
    if let Err(e) = tc.cleanup().await {
        eprintln!("cleanup: {e}");
    }
    outcome?;
    println!("abstract_account passed");
    Ok(())
}

async fn run(tc: &mut ict_rs::testing::TestChain) -> Result<(), Box<dyn std::error::Error>> {
    let params = tc
        .chain
        .chain_exec(&["query", "smartaccount", "params", "--output", "json"])
        .await?;
    if params.exit_code != 0 {
        return Err(format!(
            "smartaccount params failed: stdout={} stderr={}",
            params.stdout_str().trim(),
            params.stderr_str().trim()
        )
        .into());
    }
    let parsed: serde_json::Value = serde_json::from_str(params.stdout_str().trim())?;
    println!("smartaccount params {parsed}");
    if !parsed.is_object() {
        return Err(format!("smartaccount params were not an object: {parsed}").into());
    }

    tc.chain.create_key("aa").await?;
    let addr = tc.chain.key_address("aa").await?;
    tc.chain
        .send_funds(
            "validator",
            &WalletAmount {
                address: addr.clone(),
                denom: "uterp".into(),
                amount: 10_000_000_000,
            },
        )
        .await?;
    let listed = tc
        .chain
        .chain_exec(&[
            "query",
            "smartaccount",
            "authenticators",
            &addr,
            "--output",
            "json",
        ])
        .await?;
    if listed.exit_code != 0 {
        return Err(format!(
            "authenticators query failed: stdout={} stderr={}",
            listed.stdout_str().trim(),
            listed.stderr_str().trim()
        )
        .into());
    }
    println!("authenticators {} {}", addr, listed.stdout_str().trim());
    Ok(())
}
