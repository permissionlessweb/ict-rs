//! Bootstrap Mainnet Sync ICT Test.
//!
//! Spins up a Docker container with the `terpd` image and runs
//! `terpd bootstrap --chain-id morocco-1` to state-sync against the live
//! mainnet.  Polls the node's RPC until it reaches a positive block height,
//! confirming production bootstrap works end-to-end.
//!
//! ## Prerequisites
//!
//! Build the Docker image before running:
//!
//! ```sh
//! cd terp-core && docker build -t terpnetwork/terp-core:latest .
//! ```
//!
//! ## Run
//!
//! ```sh
//! cargo run --example bootstrap_mainnet --features docker
//! ```
//!
//! Override the image with environment variables:
//!
//! ```sh
//! TERP_IMAGE_REPO=ghcr.io/terpnetwork/terp-core \
//! TERP_IMAGE_VERSION=v5.0.0 \
//! cargo run --example bootstrap_mainnet --features docker
//! ```

use std::time::{Duration, Instant};

use ict_rs::runtime::{
    ContainerOptions, DockerConfig, DockerImage, IctRuntime, PortBinding, RuntimeBackend,
};

/// Docker image to use. Override with TERP_IMAGE_REPO / TERP_IMAGE_VERSION env vars.
fn terp_image() -> DockerImage {
    let repo =
        std::env::var("TERP_IMAGE_REPO").unwrap_or_else(|_| "terpnetwork/terp-core".to_string());
    let version =
        std::env::var("TERP_IMAGE_VERSION").unwrap_or_else(|_| "latest".to_string());
    DockerImage {
        repository: repo,
        version,
        uid_gid: None,
    }
}

/// Maximum time to wait for state-sync to complete.
const SYNC_TIMEOUT_SECS: u64 = 480;

/// How often to poll the node's status.
const POLL_INTERVAL_SECS: u64 = 10;

/// Seconds to wait after starting bootstrap before first poll.
const STARTUP_WAIT_SECS: u64 = 30;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    println!("=== Bootstrap Mainnet Sync ICT Test ===\n");

    // ── Phase 1: Docker setup ────────────────────────────────────────────

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    println!("Docker runtime connected.");

    let image = terp_image();
    println!("Pulling image {}...", image);
    runtime.pull_image(&image).await?;

    let network_id = runtime.create_network("ict-bootstrap-mainnet").await?;
    println!("Network created.");

    let container_id = runtime
        .create_container(&ContainerOptions {
            image: image.clone(),
            name: "ict-bootstrap-mainnet-node".to_string(),
            network_id: Some(network_id.clone()),
            env: vec![],
            // Idle entrypoint — bootstrap will be exec'd in.
            cmd: vec!["sleep".to_string(), "infinity".to_string()],
            entrypoint: None,
            ports: vec![PortBinding {
                host_port: 0, // Docker assigns a random host port
                container_port: 26657,
                protocol: "tcp".to_string(),
            }],
            volumes: vec![],
            labels: vec![(
                "ict-test".to_string(),
                "bootstrap-mainnet".to_string(),
            )],
            hostname: Some("bootstrap-node".to_string()),
        })
        .await?;
    println!("Container created.");

    runtime.start_container(&container_id).await?;
    println!("Container started.\n");

    // ── Phase 2: Run bootstrap ───────────────────────────────────────────

    println!("--- Running terpd bootstrap (state-sync to morocco-1) ---");
    runtime
        .exec_in_container_background(
            &container_id,
            &[
                "terpd",
                "bootstrap",
                "--chain-id",
                "morocco-1",
                "--moniker",
                "ict-bootstrap-test",
                "--public", // enable PEX for broader peer discovery
            ],
            &[],
        )
        .await?;
    println!("Bootstrap started.");

    println!(
        "Waiting {}s for node initialization...\n",
        STARTUP_WAIT_SECS
    );
    tokio::time::sleep(Duration::from_secs(STARTUP_WAIT_SECS)).await;

    // ── Phase 3: Poll for sync completion ────────────────────────────────

    println!(
        "Polling for state-sync completion (timeout: {}s)...",
        SYNC_TIMEOUT_SECS
    );
    let deadline = Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS);

    let result = poll_until_synced(&*runtime, &container_id, deadline).await;

    // ── Phase 4: Cleanup (always) ────────────────────────────────────────

    println!("\n--- Cleanup ---");
    if let Err(e) = runtime.stop_container(&container_id).await {
        eprintln!("Warning: stop container: {}", e);
    }
    if let Err(e) = runtime.remove_container(&container_id).await {
        eprintln!("Warning: remove container: {}", e);
    }
    if let Err(e) = runtime.remove_network(&network_id).await {
        eprintln!("Warning: remove network: {}", e);
    }

    match result {
        Ok(()) => {
            println!("\nBootstrap mainnet sync test PASSED!");
            Ok(())
        }
        Err(e) => {
            eprintln!("\nTest FAILED: {}", e);
            Err(e)
        }
    }
}

/// Poll the node via `terpd status` until latest_block_height > 0 or deadline is reached.
async fn poll_until_synced(
    runtime: &dyn RuntimeBackend,
    container_id: &ict_rs::runtime::ContainerId,
    deadline: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        if Instant::now() > deadline {
            // Dump tail of container logs for debugging.
            if let Ok(logs) = runtime.container_logs(container_id).await {
                let lines: Vec<&str> = logs.lines().collect();
                let tail_start = lines.len().saturating_sub(60);
                eprintln!("\n--- Container logs (last 60 lines) ---");
                for line in &lines[tail_start..] {
                    eprintln!("{}", line);
                }
            }
            return Err(format!(
                "State-sync did not complete within {} seconds",
                SYNC_TIMEOUT_SECS
            )
            .into());
        }

        match runtime
            .exec_in_container(container_id, &["terpd", "status"], &[])
            .await
        {
            Ok(output) if output.exit_code == 0 => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Ok(status) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    let height = parse_height(&status);

                    if height > 0 {
                        let network = status
                            .pointer("/node_info/network")
                            .or_else(|| status.pointer("/NodeInfo/network"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let app_hash = status
                            .pointer("/sync_info/latest_app_hash")
                            .or_else(|| status.pointer("/SyncInfo/latest_app_hash"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");

                        println!("\n=== STATE SYNC COMPLETE ===");
                        println!("  Height:   {}", height);
                        println!("  Network:  {}", network);
                        println!("  App hash: {}", app_hash);

                        assert!(height > 0, "height must be positive");
                        assert_eq!(
                            network, "morocco-1",
                            "node must be synced to mainnet (morocco-1)"
                        );

                        return Ok(());
                    }

                    println!("  State-sync in progress (height: {})...", height);
                } else {
                    println!("  Node starting up (status not parseable yet)...");
                }
            }
            _ => {
                println!("  Node starting up...");
            }
        }

        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

/// Extract `latest_block_height` from the status JSON, handling both
/// CometBFT camelCase and snake_case field styles.
fn parse_height(status: &serde_json::Value) -> u64 {
    status
        .pointer("/sync_info/latest_block_height")
        .or_else(|| status.pointer("/SyncInfo/latest_block_height"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}
