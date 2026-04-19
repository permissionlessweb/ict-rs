//! Chain upgrade E2E test using real Docker containers.
//!
//! Mirrors `chain_upgrade_test.go` — starts a Terp chain with the pre-upgrade
//! image, submits a software upgrade governance proposal, votes, waits for the
//! chain to halt at the upgrade height, swaps the Docker image to the upgraded
//! version, restarts nodes, and verifies the chain resumes producing blocks.
//!
//! ## Prerequisites
//!
//! Build two Docker images before running:
//!
//! ```sh
//! # Pre-upgrade image (main branch, no hashmerchant module)
//! cd terp-core && git checkout main
//! make docker-build  # → terpnetwork/terp-core:v5
//! docker tag terpnetwork/terp-core:local terpnetwork/terp-core:v5
//!
//! # Post-upgrade image (feat/hashmerchant-module branch)
//! cd terp-core && git checkout feat/hashmerchant-module
//! make docker-build  # → terpnetwork/terp-core:local
//! ```
//!
//! ```sh
//! cargo run --example cosmos_upgrade --features docker
//! ```

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm, TestContext};
use ict_rs::governance::{status, GovernanceExt};
use ict_rs::interchain::wait_for_blocks;
use ict_rs::runtime::{DockerConfig, DockerImage, IctRuntime};
use ict_rs::tx::WalletAmount;

/// How many blocks in the future to schedule the upgrade halt.
const HALT_HEIGHT_DELTA: u64 = 9;

/// Blocks to wait after upgrade to confirm chain resumed.
const BLOCKS_AFTER_UPGRADE: u64 = 7;

/// Pre-upgrade image tag.
const START_VERSION: &str = "v5";
/// Upgrade name in the governance proposal.
const UPGRADE_NAME: &str = "v6";
/// Post-upgrade image repo.
const UPGRADE_REPO: &str = "terpnetwork/terp-core";
/// Post-upgrade image tag.
const UPGRADE_VERSION: &str = "local";

fn terp_upgrade_config(version: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: "120u-1".to_string(),
        images: vec![DockerImage {
            repository: "terpnetwork/terp-core".to_string(),
            version: version.to_string(),
            uid_gid: None,
        }],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".to_string(),
        gas_adjustment: 1.5,
        trusting_period: "112h".to_string(),
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: std::collections::HashMap::new(),
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

/// Run the upgrade test logic. Returns Ok on success or the first error encountered.
async fn run_test(chain: &mut CosmosChain, num_validators: usize) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize and start chain
    println!("\n--- Initializing chain ---");
    let ctx = TestContext {
        test_name: "cosmos-upgrade".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain started and producing blocks.");

    // 2. Create and fund a test user
    println!("\n--- Funding test user ---");
    chain.create_key("testuser").await?;
    let user_addr = chain.primary_node()?.get_key_address("testuser").await?;
    let fund = WalletAmount {
        address: user_addr.clone(),
        denom: "uterp".to_string(),
        amount: 10_000_000_000,
    };
    chain.send_funds("validator", &fund).await?;
    println!("Funded user: {}", user_addr);

    // 3. Get current height and compute halt height
    let current_height = chain.height().await?;
    let halt_height = current_height + HALT_HEIGHT_DELTA;
    println!("\n--- Submitting upgrade proposal ---");
    println!("Current height: {}", current_height);
    println!("Halt height:    {}", halt_height);

    // 4. Submit software upgrade proposal
    let deposit = "500000000uterp".to_string();
    let proposal_id = chain
        .submit_software_upgrade_proposal("testuser", UPGRADE_NAME, halt_height, &deposit)
        .await?;
    println!("Proposal submitted: ID={}", proposal_id);

    // 5. All validators vote yes
    println!("\n--- Voting on proposal ---");
    chain
        .vote_on_proposal_all_validators(proposal_id, "yes")
        .await?;
    println!("All {} validators voted yes.", num_validators);

    // 6. Poll until proposal passes
    println!("\n--- Waiting for proposal to pass ---");
    chain
        .poll_for_proposal_status(proposal_id, status::PASSED, 60)
        .await?;
    println!("Proposal PASSED.");

    // 7. Wait for chain to halt at upgrade height
    println!("\n--- Waiting for chain halt at height {} ---", halt_height);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        match chain.height().await {
            Ok(h) if h >= halt_height => {
                println!("Chain halted at height: {}", h);
                break;
            }
            Ok(_) => {}
            Err(_) => {
                println!("Chain halted (connection error — expected at halt height).");
                break;
            }
        }
    }

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let halted_height = chain.height().await.unwrap_or(halt_height);
    println!("Confirmed halt height: {}", halted_height);

    // 8. Stop all nodes
    println!("\n--- Stopping nodes for upgrade ---");
    chain.stop_all_nodes().await?;
    println!("All nodes stopped.");

    // 9. Upgrade version (swap Docker image)
    println!("\n--- Upgrading to {}:{} ---", UPGRADE_REPO, UPGRADE_VERSION);
    chain.upgrade_version(UPGRADE_REPO, UPGRADE_VERSION);

    // 10. Restart all nodes with new image
    println!("\n--- Starting upgraded nodes ---");
    chain.start_all_nodes().await?;
    println!("All nodes restarted with upgraded image.");

    // 11. Wait for blocks after upgrade
    println!("\n--- Verifying post-upgrade block production ---");
    let post_upgrade_height = chain.height().await?;
    println!("Post-upgrade height: {}", post_upgrade_height);

    wait_for_blocks(chain, BLOCKS_AFTER_UPGRADE).await?;
    let final_height = chain.height().await?;
    println!(
        "Chain produced {} blocks after upgrade (height: {})",
        final_height - post_upgrade_height,
        final_height
    );
    assert!(
        final_height >= post_upgrade_height + BLOCKS_AFTER_UPGRADE,
        "chain did not produce enough blocks after upgrade"
    );

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    println!("=== Terp Chain Upgrade Test (v5 → v6) ===\n");

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    println!("Docker runtime connected.");

    let config = terp_upgrade_config(START_VERSION);
    let num_validators = 2;
    let num_full_nodes = 0;
    let mut chain = CosmosChain::new(config, num_validators, num_full_nodes, runtime.clone());

    println!(
        "Chain: {} (image: terpnetwork/terp-core:{})",
        chain.chain_id(),
        START_VERSION
    );
    println!("Validators: {}, Full Nodes: {}", num_validators, num_full_nodes);

    // Run test, then ALWAYS clean up — even on error.
    let result = run_test(&mut chain, num_validators).await;

    println!("\n--- Shutdown ---");
    if let Err(e) = chain.stop().await {
        eprintln!("Warning: cleanup error: {}", e);
    }

    match result {
        Ok(()) => {
            println!("Chain upgrade test PASSED! (v5 → v6)");
            Ok(())
        }
        Err(e) => {
            eprintln!("Test FAILED: {}", e);
            Err(e)
        }
    }
}
