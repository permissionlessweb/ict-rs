//! Multi-chain IBC transfer example using Docker runtime and Hermes relayer.
//!
//! Demonstrates spinning up two Terp chains with a relayer, performing IBC
//! transfers in both directions, and asserting balance changes with real
//! `assert!` / `assert_eq!` macros.
//!
//! ## Prerequisites
//!
//! Build the Terp chain Docker image locally:
//! ```sh
//! cd terp-core && make build-docker-local
//! # → terpnetwork/terp-core:local
//! ```
//!
//! ```sh
//! cargo run --example ibc_transfer --features docker
//! ```

use std::collections::HashMap;

use ict_rs::prelude::*;

/// Docker image to use. Override with TERP_IMAGE_REPO and TERP_IMAGE_VERSION env vars.
fn terp_image() -> DockerImage {
    let repo = std::env::var("TERP_IMAGE_REPO")
        .unwrap_or_else(|_| "registry.terp.network/terp-core".to_string());
    let version = std::env::var("TERP_IMAGE_VERSION")
        .unwrap_or_else(|_| "local".to_string());
    DockerImage {
        repository: repo,
        version,
        uid_gid: None,
    }
}

/// Chain A config (terp-test-1).
fn chain_a_config() -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp-a".to_string(),
        chain_id: "terp-test-1".to_string(),
        images: vec![terp_image()],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".to_string(),
        gas_adjustment: 2.0,
        trusting_period: "112h".to_string(),
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: HashMap::new(),
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

/// Chain B config (terp-test-2, same binary, different chain_id).
fn chain_b_config() -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp-b".to_string(),
        chain_id: "terp-test-2".to_string(),
        images: vec![terp_image()],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".to_string(),
        gas_adjustment: 2.0,
        trusting_period: "112h".to_string(),
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: HashMap::new(),
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

/// Get a key's bech32 address from a chain via chain_exec.
async fn key_address(
    chain: &dyn Chain,
    key_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let output = chain
        .chain_exec(&[
            "keys", "show", key_name, "-a",
            "--keyring-backend", "test",
        ])
        .await?;
    let addr = output.stdout_str().trim().to_string();
    if addr.is_empty() {
        return Err(format!("empty address for key '{key_name}'").into());
    }
    Ok(addr)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "info,ict_rs::relayer=debug".to_string()),
        )
        .init();

    println!("=== IBC Transfer Example (Docker + Hermes) ===\n");

    // 1. Create Docker runtime
    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    println!("Docker runtime connected.");

    // 2. Create shared Docker network (must exist before relayer container)
    let test_name = "ibc-transfer-example";
    let network_id = format!("ict-{test_name}");
    runtime.create_network(&network_id).await?;
    println!("Docker network created: {network_id}");

    // 3. Create two Terp chains
    let chain_a = CosmosChain::new(chain_a_config(), 1, 0, runtime.clone());
    let chain_b = CosmosChain::new(chain_b_config(), 1, 0, runtime.clone());
    println!("Chains: {} and {}", chain_a.chain_id(), chain_b.chain_id());

    // 4. Create Hermes relayer
    let relayer = build_relayer(
        RelayerType::Hermes,
        runtime.clone(),
        test_name,
        &network_id,
    )
    .await?;
    println!("Hermes relayer created.");

    // 5. Build interchain environment (init chains, start, configure relayer,
    //    create IBC clients+connections+channels, start relayer)
    let mut ic = Interchain::new(runtime)
        .add_chain(Box::new(chain_a))
        .add_chain(Box::new(chain_b))
        .add_relayer("hermes", relayer)
        .add_link(InterchainLink {
            chain1: "terp-test-1".to_string(),
            chain2: "terp-test-2".to_string(),
            relayer: "hermes".to_string(),
            path: "ibc-path".to_string(),
        });

    println!("\nBuilding interchain environment...");
    ic.build(InterchainBuildOptions {
        test_name: test_name.to_string(),
        ..Default::default()
    })
    .await?;
    println!("Interchain environment ready!\n");

    // 6. Run the IBC transfer test with proper assertions
    let result = run_ibc_transfer_test(&mut ic).await;

    // 7. Always clean up (even on failure)
    println!("\n--- Shutdown ---");
    if let Err(e) = ic.close().await {
        eprintln!("Warning: cleanup error: {e}");
    }

    match result {
        Ok(()) => {
            println!("\nIBC transfer example PASSED!");
            Ok(())
        }
        Err(e) => {
            eprintln!("\nIBC transfer example FAILED: {e}");
            Err(e)
        }
    }
}

/// Run the IBC transfer test logic with real assertions (not eprintln warnings).
async fn run_ibc_transfer_test(
    ic: &mut Interchain,
) -> Result<(), Box<dyn std::error::Error>> {
    let chain_a = ic.get_chain("terp-test-1").unwrap();
    let chain_b = ic.get_chain("terp-test-2").unwrap();

    // ── 1. Create and fund test users ────────────────────────────
    println!("--- Funding test users ---");
    chain_a.create_key("user-a").await?;
    chain_b.create_key("user-b").await?;

    let user_a = key_address(chain_a, "user-a").await?;
    let user_b = key_address(chain_b, "user-b").await?;
    println!("  Chain A user: {user_a}");
    println!("  Chain B user: {user_b}");

    let fund_amount = 10_000_000_000u128;
    chain_a
        .send_funds(
            "validator",
            &WalletAmount {
                address: user_a.clone(),
                denom: "uterp".to_string(),
                amount: fund_amount,
            },
        )
        .await?;
    chain_b
        .send_funds(
            "validator",
            &WalletAmount {
                address: user_b.clone(),
                denom: "uterp".to_string(),
                amount: fund_amount,
            },
        )
        .await?;
    wait_for_blocks(chain_a, 3).await?;
    wait_for_blocks(chain_b, 3).await?;
    println!("  Funded with {fund_amount} micro-units each\n");

    // ── 2. Query initial balances ────────────────────────────────
    let a_bal_before = chain_a.get_balance(&user_a, "uterp").await?;
    let b_bal_before = chain_b.get_balance(&user_b, "uterp").await?;
    println!("--- Initial Balances ---");
    println!("  Chain A user: {a_bal_before} uterp");
    println!("  Chain B user: {b_bal_before} uterp");

    assert!(
        a_bal_before > 0,
        "Chain A user should have been funded, got {a_bal_before}"
    );
    assert!(
        b_bal_before > 0,
        "Chain B user should have been funded, got {b_bal_before}"
    );

    // ── 3. IBC transfer: A → B (1000 uterp) ─────────────────────
    println!("\n--- IBC Transfer: A → B (1000 uterp) ---");
    let transfer_amount = 1000u128;
    let tx = chain_a
        .send_ibc_transfer(
            "channel-0",
            "user-a",
            &WalletAmount {
                address: user_b.clone(),
                denom: "uterp".to_string(),
                amount: transfer_amount,
            },
            &TransferOptions::default(),
        )
        .await?;
    println!(
        "  Transfer tx: {} (height: {})",
        tx.tx_hash, tx.height
    );

    // Wait for relayer to relay the packet
    println!("  Waiting for IBC relay...");
    wait_for_blocks(chain_a, 10).await?;
    wait_for_blocks(chain_b, 5).await?;

    // ── 4. Compute expected IBC denom on chain B ─────────────────
    let expected_ibc_denom = ibc_denom("transfer", "channel-0", "uterp");
    println!("  Expected IBC denom on B: {expected_ibc_denom}");

    // ── 5. Assert balance changes after A→B transfer ─────────────
    let a_bal_after = chain_a.get_balance(&user_a, "uterp").await?;
    let b_ibc_bal = chain_b
        .get_balance(&user_b, &expected_ibc_denom)
        .await?;
    println!("\n--- Post-Transfer (A→B) Balances ---");
    println!(
        "  Chain A user: {a_bal_after} uterp (was {a_bal_before})"
    );
    println!(
        "  Chain B user: {b_ibc_bal} {expected_ibc_denom} (IBC-wrapped uterp)"
    );

    assert!(
        a_bal_after < a_bal_before,
        "Chain A balance should have decreased: was {a_bal_before}, now {a_bal_after}"
    );
    assert!(
        b_ibc_bal >= transfer_amount,
        "Chain B should have received at least {transfer_amount} IBC tokens, got {b_ibc_bal}"
    );
    println!("  ASSERTIONS PASSED: A balance decreased, B received IBC tokens");

    // ── 6. IBC transfer: B → A (return 500 IBC uterp) ───────────
    println!("\n--- IBC Transfer: B → A (return 500 IBC uterp) ---");
    let return_amount = 500u128;
    let tx2 = chain_b
        .send_ibc_transfer(
            "channel-0",
            "user-b",
            &WalletAmount {
                address: user_a.clone(),
                denom: expected_ibc_denom.clone(),
                amount: return_amount,
            },
            &TransferOptions::default(),
        )
        .await?;
    println!(
        "  Return tx: {} (height: {})",
        tx2.tx_hash, tx2.height
    );

    // Wait for relay
    println!("  Waiting for IBC relay...");
    wait_for_blocks(chain_b, 10).await?;
    wait_for_blocks(chain_a, 5).await?;

    // ── 7. Assert balance changes after B→A return transfer ──────
    let a_bal_final = chain_a.get_balance(&user_a, "uterp").await?;
    let b_ibc_final = chain_b
        .get_balance(&user_b, &expected_ibc_denom)
        .await?;
    println!("\n--- Final Balances ---");
    println!("  Chain A user: {a_bal_final} uterp");
    println!(
        "  Chain B user: {b_ibc_final} {expected_ibc_denom}"
    );

    assert!(
        a_bal_final > a_bal_after,
        "Chain A balance should have increased after return transfer: \
         was {a_bal_after}, now {a_bal_final}"
    );
    assert!(
        b_ibc_final < b_ibc_bal,
        "Chain B IBC balance should have decreased after return transfer: \
         was {b_ibc_bal}, now {b_ibc_final}"
    );
    println!("  ASSERTIONS PASSED: A balance increased, B IBC balance decreased");

    println!("\nAll assertions passed — IBC transfers work correctly in both directions!");
    Ok(())
}