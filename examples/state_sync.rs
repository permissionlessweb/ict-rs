//! State Sync E2E test using real Docker containers.
//!
//! Validates CometBFT state sync: starts a chain with snapshot intervals,
//! waits for snapshots, then stops a full node, wipes its data, reconfigures
//! it for state sync, restarts it, and verifies it synced from a snapshot
//! (not from genesis).
//!
//! ## Prerequisites
//!
//! Build the Docker image before running:
//!
//! ```sh
//! cd terp-core && make docker-build
//! docker tag terpnetwork/terp-core:local terpnetwork/terp-core:latest
//! ```
//!
//! ```sh
//! cargo run --example state_sync --features docker
//! ```

use std::collections::HashMap;

use ict_rs::prelude::*;

/// Docker image to use. Override with TERP_IMAGE_REPO / TERP_IMAGE_VERSION env vars.
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

/// Snapshot interval in blocks. The chain will take a snapshot every this many blocks.
const SNAPSHOT_INTERVAL: u64 = 10;

/// How many snapshot intervals to wait before attempting state sync.
const SNAPSHOT_INTERVALS_TO_WAIT: u64 = 2;

/// Timeout in seconds for the state-syncing node to catch up.
const SYNC_TIMEOUT_SECS: u64 = 120;

fn state_sync_chain_config() -> ChainConfig {
    let mut config_overrides = HashMap::new();

    // Enable snapshots in app.toml
    config_overrides.insert(
        "config/app.toml".to_string(),
        serde_json::json!({
            "state-sync": {
                "snapshot-interval": SNAPSHOT_INTERVAL,
                "snapshot-keep-recent": 2
            },
            "pruning": "nothing"
        }),
    );

    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: "statesync-1".to_string(),
        images: vec![terp_image()],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".to_string(),
        gas_adjustment: 1.5,
        trusting_period: "112h".to_string(),
        block_time: "1s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: config_overrides,
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

async fn run_test(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize and start chain (1 validator + 1 full node)
    println!("\n--- Initializing chain ---");
    let ctx = TestContext {
        test_name: "state-sync".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain started with snapshots every {} blocks.", SNAPSHOT_INTERVAL);

    // 2. Wait for enough blocks so at least 2 snapshots are taken
    let blocks_to_wait = SNAPSHOT_INTERVAL * SNAPSHOT_INTERVALS_TO_WAIT;
    println!(
        "\n--- Waiting for {} blocks ({} snapshot intervals) ---",
        blocks_to_wait, SNAPSHOT_INTERVALS_TO_WAIT
    );
    wait_for_blocks(chain, blocks_to_wait).await?;

    let current_height = chain.height().await?;
    println!("Current height: {}", current_height);

    // 3. Get trusted height and block hash from the validator
    //    Trust height should be a recent snapshot height (aligned to snapshot interval)
    let trust_height = (current_height / SNAPSHOT_INTERVAL) * SNAPSHOT_INTERVAL;
    println!("\n--- Getting trust parameters ---");
    println!("Trust height: {}", trust_height);

    let validator = chain.primary_node()?;
    let trust_hash = validator.query_block_hash(trust_height).await?;
    let val_hostname = validator.hostname.clone();
    let val_rpc_port = validator.ports.rpc;
    println!("Trust hash: {}", trust_hash);

    // Build RPC servers string (validator's internal address, listed twice per convention)
    let rpc_servers = format!(
        "tcp://{}:{},tcp://{}:{}",
        val_hostname, val_rpc_port, val_hostname, val_rpc_port
    );

    // 4. Stop the full node container
    println!("\n--- Stopping full node for state sync reconfiguration ---");
    let full_node = &chain.full_nodes()[0];
    let fn_hostname = full_node.hostname.clone();
    full_node.stop_container().await?;
    println!("Full node {} stopped.", fn_hostname);

    // 5. Remove and recreate the full node container (preserving volume for config access)
    let full_node = &mut chain.full_nodes_mut()[0];
    full_node.remove_container().await?;
    full_node.create_container_for_upgrade().await?;
    full_node.start_container().await?;
    println!("Full node container recreated.");

    // 6. Wipe data directory
    let full_node = &chain.full_nodes()[0];
    full_node.wipe_data().await?;
    println!("Data directory wiped.");

    // 7. Apply state sync config to config.toml
    println!("\n--- Configuring state sync ---");
    let statesync_overrides = serde_json::json!({
        "statesync": {
            "enable": true,
            "trust_height": trust_height,
            "trust_hash": trust_hash,
            "rpc_servers": rpc_servers,
            "discovery_time": "10s"
        }
    });
    full_node
        .apply_config_override("config/config.toml", &statesync_overrides)
        .await?;
    println!("State sync config applied:");
    println!("  rpc_servers:  {}", rpc_servers);
    println!("  trust_height: {}", trust_height);
    println!("  trust_hash:   {}", trust_hash);

    // 8. Start the chain binary on the reconfigured full node
    println!("\n--- Starting state-syncing full node ---");
    full_node.exec_start_chain().await?;

    // Give the binary a moment to start before polling
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    // Dump early logs so we can see if state sync is attempting
    let early_logs = full_node.read_chain_log(30).await;
    println!("\n--- Full node early logs ---\n{}", early_logs);

    // 9. Poll until the node is synced (catching_up = false)
    println!("\nWaiting for state sync to complete (timeout: {}s)...", SYNC_TIMEOUT_SECS);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(SYNC_TIMEOUT_SECS);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        if std::time::Instant::now() > deadline {
            // Dump logs for debugging
            let logs = full_node.read_chain_log(80).await;
            eprintln!("\n--- Full node logs (last 80 lines) ---\n{}", logs);
            return Err(format!(
                "State sync timed out after {}s",
                SYNC_TIMEOUT_SECS
            )
            .into());
        }

        match full_node.is_catching_up().await {
            Ok(false) => {
                println!("Full node synced!");
                break;
            }
            Ok(true) => {
                // Still syncing, continue polling
            }
            Err(_) => {
                // Node may not be ready yet (binary still starting), keep trying
            }
        }
    }

    // 10. Validate state sync (not block sync)
    println!("\n--- Validating state sync ---");
    let status = full_node.query_status_json().await?;

    let node_height_str = status["sync_info"]["latest_block_height"]
        .as_str()
        .or_else(|| status["SyncInfo"]["latest_block_height"].as_str())
        .unwrap_or("0");
    let node_height: u64 = node_height_str.parse().unwrap_or(0);

    let earliest_str = status["sync_info"]["earliest_block_height"]
        .as_str()
        .or_else(|| status["SyncInfo"]["earliest_block_height"].as_str())
        .unwrap_or("1");
    let earliest_height: u64 = earliest_str.parse().unwrap_or(1);

    println!("Node height:           {}", node_height);
    println!("Earliest block height: {}", earliest_height);
    println!("Trust height:          {}", trust_height);

    // The critical check: if state sync worked, the earliest block should be
    // near the trust height (not 1). Block sync would replay from genesis.
    assert!(
        node_height >= trust_height,
        "node height ({}) should be >= trust height ({})",
        node_height,
        trust_height
    );

    assert!(
        earliest_height > 1,
        "earliest_block_height ({}) should be > 1 (state sync should skip genesis blocks)",
        earliest_height
    );

    println!(
        "\nState sync VERIFIED: node synced from height {} (not genesis).",
        earliest_height
    );

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    println!("=== CometBFT State Sync E2E Test ===\n");

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    println!("Docker runtime connected.");

    let config = state_sync_chain_config();
    let num_validators = 1;
    let num_full_nodes = 1;
    let mut chain = CosmosChain::new(config, num_validators, num_full_nodes, runtime.clone());

    println!(
        "Chain: {} (1 validator + 1 full node)",
        chain.chain_id()
    );

    // Run test, then ALWAYS clean up
    let result = run_test(&mut chain).await;

    println!("\n--- Shutdown ---");
    if let Err(e) = chain.stop().await {
        eprintln!("Warning: cleanup error: {}", e);
    }

    match result {
        Ok(()) => {
            println!("\nState sync test PASSED!");
            Ok(())
        }
        Err(e) => {
            eprintln!("\nTest FAILED: {}", e);
            Err(e)
        }
    }
}
