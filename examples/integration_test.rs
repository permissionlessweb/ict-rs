//! Core module integration test example.
//!
//! Mirrors `chain_start_test.go` — single chain, exercises bank, staking,
//! auth, distribution, slashing, and height queries.
//!
//! ```sh
//! cargo run --example integration_test
//! ```

use std::sync::Arc;

use ict_rs::auth::generate_mnemonic;
use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, TestContext};
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::runtime::RuntimeBackend;
use ict_rs::spec::builtin_chain_config;
use ict_rs::tx::WalletAmount;
use ict_rs::wallet::KeyWallet;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Integration Test: Core Module Smoke Test ===\n");

    // 1. Create and start a single Terp chain (1 val, 0 full nodes)
    let runtime: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
    let config = builtin_chain_config("terp")?;
    let mut chain = CosmosChain::new(config, 1, 0, runtime);

    let ctx = TestContext {
        test_name: "integration-test".to_string(),
        network_id: "ict-integration-test".to_string(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain {} started with 1 validator\n", chain.chain_id());

    // 2. Create test users locally (derive bech32 addresses from mnemonics)
    let user_a = KeyWallet::from_mnemonic("user-a", &generate_mnemonic(), "terp", 118)?;
    let user_b = KeyWallet::from_mnemonic("user-b", &generate_mnemonic(), "terp", 118)?;
    println!("Test users:");
    println!("  User A: {}", user_a.bech32_address);
    println!("  User B: {}", user_b.bech32_address);

    // Fund both users from the validator account
    for user in [&user_a, &user_b] {
        let fund = WalletAmount {
            address: user.bech32_address.clone(),
            denom: "uterp".to_string(),
            amount: 10_000_000_000,
        };
        chain.send_funds("validator-0", &fund).await?;
    }
    println!("  Funded both users with 10_000_000_000 uterp");

    // 3. Bank: send funds and check balance
    println!("\n--- Bank Module ---");
    let send_amount = WalletAmount {
        address: user_b.bech32_address.clone(),
        denom: "uterp".to_string(),
        amount: 1_000_000,
    };
    let tx_hash = chain.send_funds(&user_a.key_name, &send_amount).await?;
    println!(
        "  send_funds: {} -> {} (1000000uterp) tx={}",
        user_a.bech32_address, user_b.bech32_address, tx_hash
    );

    let balance = chain.get_balance(&user_b.bech32_address, "uterp").await?;
    println!("  get_balance({}): {} uterp", user_b.bech32_address, balance);
    assert!(balance > 0, "balance should be > 0");

    // 4. Staking: query validators
    println!("\n--- Staking Module ---");
    let output = chain
        .exec(
            &["query", "staking", "validators", "--output", "json"],
            &[],
        )
        .await?;
    let stdout = output.stdout_str();
    let staking: serde_json::Value = serde_json::from_str(&stdout)?;
    let validators = staking["validators"]
        .as_array()
        .expect("validators array");
    println!("  validators: {} found", validators.len());
    assert!(!validators.is_empty(), "should have at least 1 validator");

    // 5. Auth: query module address
    println!("\n--- Auth Module ---");
    let output = chain
        .exec(
            &[
                "query",
                "auth",
                "module-address",
                "bank",
                "--output",
                "json",
            ],
            &[],
        )
        .await?;
    let stdout = output.stdout_str();
    let auth: serde_json::Value = serde_json::from_str(&stdout)?;
    let module_addr = auth["address"].as_str().unwrap_or("unknown");
    println!("  bank module address: {}", module_addr);

    // 6. Distribution: query community pool
    println!("\n--- Distribution Module ---");
    let output = chain
        .exec(
            &[
                "query",
                "distribution",
                "community-pool",
                "--output",
                "json",
            ],
            &[],
        )
        .await?;
    let stdout = output.stdout_str();
    let dist: serde_json::Value = serde_json::from_str(&stdout)?;
    println!("  community pool: {}", dist["pool"]);

    // 7. Slashing: query params
    println!("\n--- Slashing Module ---");
    let output = chain
        .exec(
            &["query", "slashing", "params", "--output", "json"],
            &[],
        )
        .await?;
    let stdout = output.stdout_str();
    let slashing: serde_json::Value = serde_json::from_str(&stdout)?;
    let signed_blocks = slashing["params"]["signed_blocks_window"]
        .as_str()
        .unwrap_or("0");
    println!("  signed_blocks_window: {}", signed_blocks);

    // 8. Height: query and assert > 0
    println!("\n--- Chain Height ---");
    let height = chain.height().await?;
    println!("  current height: {}", height);
    assert!(height > 0, "height should be > 0");

    // 9. Shutdown
    println!("\n--- Shutdown ---");
    chain.stop().await?;
    println!("Chain stopped. Integration test passed!");

    Ok(())
}
