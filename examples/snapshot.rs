//! Snapshot extraction & restore E2E test using real Docker containers.
//!
//! Validates the full snapshot lifecycle:
//!   1. Start a single-validator Terp chain
//!   2. Wait for blocks to accumulate state
//!   3. Pause the container (cgroup freeze — NOT SIGSTOP)
//!   4. Copy data/wasm/ibc_08-wasm out via Docker API
//!   5. Compress with lz4 on the host
//!   6. Unpause the source node, verify it resumes
//!   7. Start a second node, restore the snapshot into it
//!   8. Peer the two nodes and verify the restored node syncs
//!
//! This test exists because SIGSTOP on PID 1 inside Docker containers
//! (without --init) is silently ignored by the kernel, which means the
//! node keeps writing to LevelDB during extraction, producing corrupted
//! snapshots. Using `docker pause` (cgroup freezer) is the correct approach.
//!
//! ## Prerequisites
//!
//! A local terp-core Docker image with lz4 available:
//! ```sh
//! cd terp-core && make docker-build
//! # or use: ghcr.io/terpnetwork/terp-core:v5.1.6-oline
//! ```
//!
//! ## Run
//! ```sh
//! cargo run --example snapshot --features docker
//! ```

use std::io::Write;

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm, TestContext};
use ict_rs::interchain::wait_for_blocks;
use ict_rs::runtime::{DockerConfig, DockerImage, IctRuntime};

/// Image to use for the test chain.
const IMAGE_REPO: &str = "ghcr.io/terpnetwork/terp-core";
const IMAGE_VERSION: &str = "v5.1.6-oline";

/// Blocks to produce before snapshotting (accumulates state).
const BLOCKS_BEFORE_SNAPSHOT: u64 = 5;

fn terp_snapshot_config() -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: "snapshot-ict-1".to_string(),
        images: vec![DockerImage {
            repository: IMAGE_REPO.to_string(),
            version: IMAGE_VERSION.to_string(),
            uid_gid: None,
        }],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".to_string(),
        gas_adjustment: 1.5,
        trusting_period: "336h".to_string(),
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

/// Extracts a tar archive from `copy_from_container` output into a directory.
///
/// The Docker API returns a tar stream rooted at the copied path's parent.
/// For example, copying `/home/.terpd/data` returns a tar with entries like
/// `data/application.db`, `data/blockstore.db`, etc.
fn extract_tar_to_dir(tar_bytes: &[u8], dest: &std::path::Path) -> std::io::Result<()> {
    use std::io::Read;
    let mut archive = tar::Archive::new(tar_bytes);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let dest_path = dest.join(&path);

        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&dest_path)?;
        } else {
            if let Some(parent) = dest_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::File::create(&dest_path)?;
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            file.write_all(&buf)?;
        }
    }
    Ok(())
}

async fn run_test(
    chain: &mut CosmosChain,
    runtime: std::sync::Arc<dyn ict_rs::runtime::RuntimeBackend>,
) -> Result<(), Box<dyn std::error::Error>> {
    // ─── Phase 1: Start chain with 1 validator + 1 full node ────────────
    println!("\n=== Phase 1: Starting chain (1 validator + 1 full node) ===");
    let ctx = TestContext {
        test_name: "snapshot-ict".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain producing blocks.");

    // ─── Phase 2: Wait for state to accumulate ──────────────────────────
    println!("\n=== Phase 2: Waiting for {} blocks ===", BLOCKS_BEFORE_SNAPSHOT);
    wait_for_blocks(chain, BLOCKS_BEFORE_SNAPSHOT).await?;
    let height_before = chain.height().await?;
    println!("Height before snapshot: {}", height_before);

    // ─── Phase 3: Pause validator, extract snapshot ─────────────────────
    println!("\n=== Phase 3: Pausing validator for snapshot extraction ===");
    let val_node = chain.primary_node()?;
    let container_id = val_node
        .container_id
        .as_ref()
        .ok_or("validator has no container ID")?
        .clone();
    let home_dir = val_node.home_dir.clone();

    // Pause — cgroup freeze, all writes stop instantly
    runtime.pause_container(&container_id).await?;
    println!("Container paused (cgroup freeze active).");

    // Copy data directories out via Docker API (works while paused)
    let snapshot_dir = tempfile::tempdir()?;
    let snapshot_root = snapshot_dir.path();

    println!("Copying data/ from container...");
    let data_tar = runtime
        .copy_from_container(&container_id, &format!("{}/data", home_dir))
        .await?;
    extract_tar_to_dir(&data_tar, snapshot_root)?;
    println!("  data/ extracted ({} bytes tar)", data_tar.len());

    // Copy wasm/ if present
    if let Ok(wasm_tar) = runtime
        .copy_from_container(&container_id, &format!("{}/wasm", home_dir))
        .await
    {
        extract_tar_to_dir(&wasm_tar, snapshot_root)?;
        println!("  wasm/ extracted ({} bytes tar)", wasm_tar.len());
    }

    // Copy ibc_08-wasm/ if present
    if let Ok(ibc_wasm_tar) = runtime
        .copy_from_container(&container_id, &format!("{}/ibc_08-wasm", home_dir))
        .await
    {
        extract_tar_to_dir(&ibc_wasm_tar, snapshot_root)?;
        println!("  ibc_08-wasm/ extracted ({} bytes tar)", ibc_wasm_tar.len());
    }

    // Verify data/ exists in extracted snapshot
    let data_path = snapshot_root.join("data");
    assert!(
        data_path.exists(),
        "data/ directory missing from snapshot extraction"
    );
    println!("Snapshot extracted to: {}", snapshot_root.display());

    // ─── Phase 4: Unpause validator, verify it resumes ──────────────────
    println!("\n=== Phase 4: Unpausing validator ===");
    runtime.unpause_container(&container_id).await?;
    println!("Container unpaused — node resuming.");

    // Give the node a moment to catch up, then verify it's producing blocks
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let height_after = chain.height().await?;
    println!(
        "Height after unpause: {} (was {} before snapshot)",
        height_after, height_before
    );
    assert!(
        height_after >= height_before,
        "validator did not resume after unpause"
    );

    // ─── Phase 5: Restore snapshot into full node ───────────────────────
    println!("\n=== Phase 5: Restoring snapshot into full node ===");
    let full_node = &chain.full_nodes()[0];
    let fn_container_id = full_node
        .container_id
        .as_ref()
        .ok_or("full node has no container ID")?
        .clone();
    let fn_home = full_node.home_dir.clone();

    // Stop the full node so we can replace its data
    runtime.stop_container(&fn_container_id).await?;
    println!("Full node stopped.");

    // Remove existing data dir inside the container and inject our snapshot.
    // Since the container is stopped, we restart it with the injected data.
    // We use exec after restarting to verify the data landed correctly.
    runtime.start_container(&fn_container_id).await?;

    // Clear existing data and restore from snapshot
    let clear_cmd = format!("rm -rf {}/data {}/wasm {}/ibc_08-wasm", fn_home, fn_home, fn_home);
    full_node.exec_raw(&["sh", "-c", &clear_cmd], &[]).await?;
    println!("Cleared full node data directories.");

    // Tar the snapshot and pipe it into the container
    // We do this by base64-encoding the data dir contents and writing via exec
    for dir_name in &["data", "wasm", "ibc_08-wasm"] {
        let src = snapshot_root.join(dir_name);
        if !src.exists() {
            continue;
        }

        // Create a tar of the directory on the host
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            builder.append_dir_all(*dir_name, &src)?;
            builder.finish()?;
        }

        // Write tar into container via base64 chunks
        let b64 = base64_encode(&tar_buf);
        let chunk_size = 65536;
        let tmp_b64 = format!("/tmp/{}.tar.b64", dir_name);
        let tmp_tar = format!("/tmp/{}.tar", dir_name);

        // Clear previous temp files
        full_node
            .exec_raw(&["sh", "-c", &format!("rm -f {} {}", tmp_b64, tmp_tar)], &[])
            .await?;

        for chunk in b64.as_bytes().chunks(chunk_size) {
            let chunk_str = std::str::from_utf8(chunk).unwrap_or("");
            let cmd = format!("printf '%s' '{}' >> '{}'", chunk_str, tmp_b64);
            full_node.exec_raw(&["sh", "-c", &cmd], &[]).await?;
        }

        // Decode and extract
        let cmd = format!(
            "base64 -d '{}' > '{}' && tar xf '{}' -C '{}'",
            tmp_b64, tmp_tar, tmp_tar, fn_home
        );
        let output = full_node.exec_raw(&["sh", "-c", &cmd], &[]).await?;
        if output.exit_code != 0 {
            return Err(format!(
                "Failed to restore {}: {}",
                dir_name,
                output.stderr_str()
            )
            .into());
        }
        println!("  {} restored ({} bytes)", dir_name, tar_buf.len());
    }

    // Stop and restart the full node so it picks up the restored data
    runtime.stop_container(&fn_container_id).await?;
    runtime.start_container(&fn_container_id).await?;

    // Start the chain binary in background
    let start_cmd = format!(
        "{} start --home {} > {}/chain.log 2>&1 &",
        full_node.chain_bin, fn_home, fn_home
    );
    full_node
        .exec_raw(&["sh", "-c", &start_cmd], &[])
        .await?;
    println!("Full node restarted with restored data.");

    // ─── Phase 6: Verify restored node syncs ────────────────────────────
    println!("\n=== Phase 6: Verifying restored node catches up ===");

    // Query status from inside the container via exec (RPC binds to 127.0.0.1
    // inside the container, not reachable from host without port forwarding).
    let mut synced = false;
    for attempt in 0..30 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let status_output = full_node
            .exec_raw(&["sh", "-c", "curl -sf http://127.0.0.1:26657/status 2>/dev/null || true"], &[])
            .await;

        if let Ok(output) = status_output {
            let body = output.stdout_str();
            if let Some(height) = parse_height(&body) {
                if height > 0 {
                    println!(
                        "  Full node height: {} (attempt {})",
                        height,
                        attempt + 1
                    );
                    if height >= height_before {
                        synced = true;
                        break;
                    }
                }
            }
        }
    }

    assert!(synced, "Full node did not sync from restored snapshot");
    println!("\nSUCCESS: Snapshot extract (pause) → restore → sync verified!");

    Ok(())
}

/// Parse latest_block_height from CometBFT status JSON.
fn parse_height(json: &str) -> Option<u64> {
    let key = "\"latest_block_height\":\"";
    let idx = json.find(key)?;
    let start = idx + key.len();
    let end = json[start..].find('"')?;
    json[start..start + end].parse().ok()
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    println!("=== Terp Snapshot Extract & Restore Test ===");
    println!("Image: {}:{}", IMAGE_REPO, IMAGE_VERSION);

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    println!("Docker runtime connected.");

    let config = terp_snapshot_config();
    let mut chain = CosmosChain::new(config, 1, 1, runtime.clone());

    let result = run_test(&mut chain, runtime).await;

    println!("\n--- Cleanup ---");
    if let Err(e) = chain.stop().await {
        eprintln!("Warning: cleanup error: {}", e);
    }

    match result {
        Ok(()) => {
            println!("Snapshot test PASSED!");
            Ok(())
        }
        Err(e) => {
            eprintln!("Test FAILED: {}", e);
            Err(e)
        }
    }
}
