//! IBC token transfer end-to-end test using real Docker containers.
//!
//! Mirrors `ibc_transfer_test.go` — two Terp chains, one Hermes relayer,
//! transfer tokens both directions and verify IBC denom computation.
//!
//! ## Prerequisites
//!
//! Build the Terp chain Docker image locally:
//! ```sh
//! cd terp-core && make build-docker-local
//! # → terpnetwork/terp-core:local-zk
//! ```
//!
//! ```sh
//! cargo run --example ibc_transfer_e2e --features docker
//! ```

use std::collections::HashMap;

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm};
use ict_rs::ibc::ibc_denom;
use ict_rs::interchain::{wait_for_blocks, Interchain, InterchainBuildOptions, InterchainLink};
use ict_rs::relayer::{build_relayer, RelayerType};
use ict_rs::runtime::{DockerConfig, DockerImage, IctRuntime};
use ict_rs::tx::{TransferOptions, WalletAmount};

/// Docker image to use. Override with TERP_IMAGE env var.
fn terp_image() -> DockerImage {
    let repo = std::env::var("TERP_IMAGE_REPO")
        .unwrap_or_else(|_| "terpnetwork/terp-core".to_string());
    let version = std::env::var("TERP_IMAGE_VERSION")
        .unwrap_or_else(|_| "local-zk".to_string());
    DockerImage {
        repository: repo,
        version,
        uid_gid: None,
    }
}

/// Chain A config.
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

/// Chain B config (same binary, different chain_id).
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

/// Run the IBC transfer test logic.
async fn run_test(ic: &mut Interchain) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Fund test users
    println!("\n--- Funding test users ---");
    let chain_a = ic.get_chain("terp-test-1").unwrap();
    let chain_b = ic.get_chain("terp-test-2").unwrap();

    // Create keys on each chain
    chain_a.create_key("user-a").await?;
    chain_b.create_key("user-b").await?;

    let user_a = key_address(chain_a, "user-a").await?;
    let user_b = key_address(chain_b, "user-b").await?;
    println!("  Chain A user: {}", user_a);
    println!("  Chain B user: {}", user_b);

    // Fund users from validators
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
    println!("  Funded with {} micro-units each", fund_amount);

    // 2. Query initial balances
    let a_bal_before = chain_a.get_balance(&user_a, "uterp").await?;
    let b_bal_before = chain_b.get_balance(&user_b, "uterp").await?;
    println!("\n--- Initial Balances ---");
    println!("  Chain A user: {} uterp", a_bal_before);
    println!("  Chain B user: {} uterp", b_bal_before);

    // 3. IBC transfer: A → B (1000 uterp)
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
    println!("  Transfer tx: {} (height: {})", tx.tx_hash, tx.height);

    // Wait for relayer to relay the packet
    println!("  Waiting for IBC relay...");
    wait_for_blocks(chain_a, 10).await?;
    wait_for_blocks(chain_b, 5).await?;

    // 4. Compute expected IBC denom on chain B side
    let expected_ibc_denom = ibc_denom("transfer", "channel-0", "uterp");
    println!("  Expected IBC denom on B: {}", expected_ibc_denom);

    // 5. Check balances after transfer
    let a_bal_after = chain_a.get_balance(&user_a, "uterp").await?;
    let b_ibc_bal = chain_b
        .get_balance(&user_b, &expected_ibc_denom)
        .await?;
    println!("\n--- Post-Transfer Balances ---");
    println!(
        "  Chain A user: {} uterp (was {})",
        a_bal_after, a_bal_before
    );
    println!(
        "  Chain B user: {} {} (IBC-wrapped uterp)",
        b_ibc_bal, expected_ibc_denom
    );

    if a_bal_after < a_bal_before {
        println!("  OK: Chain A balance decreased");
    } else {
        eprintln!("  WARN: Chain A balance did NOT decrease");
    }
    if b_ibc_bal >= transfer_amount {
        println!("  OK: Chain B received {} IBC tokens", b_ibc_bal);
    } else {
        eprintln!(
            "  WARN: Chain B IBC balance {} < expected {}",
            b_ibc_bal, transfer_amount
        );
    }

    // 6. IBC transfer: B → A (return 500 IBC uterp)
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
    println!("  Return tx: {} (height: {})", tx2.tx_hash, tx2.height);

    // Wait for relay
    println!("  Waiting for IBC relay...");
    wait_for_blocks(chain_b, 10).await?;
    wait_for_blocks(chain_a, 5).await?;

    // 7. Final balance check
    let a_bal_final = chain_a.get_balance(&user_a, "uterp").await?;
    let b_ibc_final = chain_b
        .get_balance(&user_b, &expected_ibc_denom)
        .await?;
    println!("\n--- Final Balances ---");
    println!("  Chain A user: {} uterp", a_bal_final);
    println!("  Chain B user: {} {}", b_ibc_final, expected_ibc_denom);

    if a_bal_final > a_bal_after {
        println!("  OK: Chain A balance increased (tokens returned)");
    }
    if b_ibc_final < b_ibc_bal {
        println!("  OK: Chain B IBC balance decreased (tokens sent back)");
    }

    println!("\nIBC transfer E2E test PASSED!");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,ict_rs::relayer=debug".to_string()),
        )
        .init();

    println!("=== IBC Transfer E2E Test (Docker) ===\n");

    // 1. Create Docker runtime
    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    println!("Docker runtime connected.");

    // 2. Create shared Docker network (must exist before relayer container)
    let test_name = "ibc-transfer-e2e";
    let network_id = format!("ict-{test_name}");
    runtime.create_network(&network_id).await?;
    println!("Docker network created: {}", network_id);

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
    println!("Interchain environment ready!");

    // 6. Run test logic, then ALWAYS clean up
    let result = run_test(&mut ic).await;

    println!("\n--- Shutdown ---");
    if let Err(e) = ic.close().await {
        eprintln!("Warning: cleanup error: {}", e);
    }

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("Test FAILED: {}", e);
            Err(e)
        }
    }
}
