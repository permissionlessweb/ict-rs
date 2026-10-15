//! Core module integration test example.
//!
//! Mirrors `chain_start_test.go` — single chain, exercises bank, staking,
//! auth, distribution, slashing, and height queries.
//!
//! ```sh
//! cargo run --example integration_test
//! ```

use ict_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Integration Test: Core Module Smoke Test ===\n");

    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("integration_test uses the terp-core image. Unset ICT_MOCK and set TERP_IMAGE_REPO and TERP_IMAGE_VERSION.".into());
    }
    let mut tc = ict_rs::testing::setup_chain(
        "integration-test",
        ict_rs::testing::TestEnv::terp_config(),
    )
    .await?;
    let chain = &mut tc.chain;
    println!("Chain {} started with 1 validator\n", chain.chain_id());

    chain.create_key("user-a").await?;
    chain.create_key("user-b").await?;
    let user_a_addr = chain.key_address("user-a").await?;
    let user_b_addr = chain.key_address("user-b").await?;
    let user_a = ict_rs::wallet::KeyWallet {
        key_name: "user-a".into(),
        address_bytes: vec![],
        bech32_address: user_a_addr,
        mnemonic_phrase: String::new(),
    };
    let user_b = ict_rs::wallet::KeyWallet {
        key_name: "user-b".into(),
        address_bytes: vec![],
        bech32_address: user_b_addr,
        mnemonic_phrase: String::new(),
    };
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
        chain.send_funds("validator", &fund).await?;
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
        .chain_exec(&["query", "staking", "validators", "--output", "json"])
        .await?;
    if output.exit_code != 0 {
        return Err(format!(
            "staking validators: {}",
            output.stderr_str().trim()
        )
        .into());
    }
    let stdout = output.stdout_str();
    let staking: serde_json::Value = serde_json::from_str(stdout.trim())?;
    let validators = staking["validators"]
        .as_array()
        .expect("validators array");
    println!("  validators: {} found", validators.len());
    assert!(!validators.is_empty(), "should have at least 1 validator");

    // 5. Auth: query module address
    println!("\n--- Auth Module ---");
    let output = chain
        .chain_exec(&[
            "query",
            "auth",
            "module-account",
            "fee_collector",
            "--output",
            "json",
        ])
        .await?;
    if output.exit_code != 0 {
        return Err(format!("auth module-account: {}", output.stderr_str().trim()).into());
    }
    let stdout = output.stdout_str();
    let auth: serde_json::Value = serde_json::from_str(stdout.trim())?;
    let module_addr = auth
        .pointer("/account/base_account/address")
        .or_else(|| auth.pointer("/account/baseAccount/address"))
        .or_else(|| auth.pointer("/account/value/address"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !module_addr.starts_with("terp") {
        return Err(format!("fee_collector address missing: {auth}").into());
    }
    println!("  fee_collector: {}", module_addr);

    // 6. Distribution: query community pool
    println!("\n--- Distribution Module ---");
    let output = chain
        .chain_exec(&[
            "query",
            "distribution",
            "community-pool",
            "--output",
            "json",
        ])
        .await?;
    if output.exit_code != 0 {
        return Err(format!(
            "community pool: {}",
            output.stderr_str().trim()
        )
        .into());
    }
    let stdout = output.stdout_str();
    let dist: serde_json::Value = serde_json::from_str(stdout.trim())?;
    println!("  community pool: {}", dist["pool"]);

    // 7. Slashing: query params
    println!("\n--- Slashing Module ---");
    let output = chain
        .chain_exec(&["query", "slashing", "params", "--output", "json"])
        .await?;
    if output.exit_code != 0 {
        return Err(format!("slashing params: {}", output.stderr_str().trim()).into());
    }
    let stdout = output.stdout_str();
    let slashing: serde_json::Value = serde_json::from_str(stdout.trim())?;
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
    tc.cleanup().await?;
    println!("Chain stopped. Integration test passed!");

    Ok(())
}
