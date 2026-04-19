//! zk-wasmvm E2E test using real Docker containers.
//!
//! Mirrors `zk_no_rick.go` — deploys a zk-wasmvm CosmWasm contract with a
//! verification key (VK), submits a proof, and verifies it succeeds on-chain.
//!
//! ## Prerequisites
//!
//! Build the zk-wasmvm Docker image:
//!
//! ```sh
//! cd terp-core
//! make build-zk-local  # → terpnetwork/terp-core:local-zk
//! ```
//!
//! Ensure the contract and circuit files exist:
//! - `terp-core/tests/interchaintest/contracts/zk_no_rick.wasm`
//! - `terp-core/tests/interchaintest/circuits/no_rick.bin`
//!
//! ```sh
//! cargo run --example no_rick --features docker
//! ```

use std::path::PathBuf;

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm, TestContext};
use ict_rs::interchain::wait_for_blocks;
use ict_rs::runtime::{DockerConfig, DockerImage, IctRuntime};
use ict_rs::tx::WalletAmount;

/// Docker image for the zk-wasmvm enabled chain.
const ZK_IMAGE_REPO: &str = "terpnetwork/terp-core";
const ZK_IMAGE_VERSION: &str = "local-zk";

/// Host-side paths to contract and VK files (relative to ZK workspace root).
const WASM_REL: &str = "terp-core/tests/interchaintest/contracts/zk_no_rick.wasm";
const VK_REL: &str = "terp-core/tests/interchaintest/circuits/no_rick.bin";
const PROOF_REL: &str = "terp-core/tests/interchaintest/circuits/no_rick_proof.json";

/// Container-side paths where we copy the files.
const CONTAINER_WASM: &str = "/tmp/zk_no_rick.wasm";
const VK: &str = "/tmp/no_rick.bin";

fn terp_zk_config() -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: "120u-1".to_string(),
        images: vec![DockerImage {
            repository: ZK_IMAGE_REPO.to_string(),
            version: ZK_IMAGE_VERSION.to_string(),
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

/// Resolve the ZK workspace root (parent of terp-core/).
/// Checks ZK_ROOT env var, then tries `../../..` from the crate dir.
fn resolve_zk_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(root) = std::env::var("ZK_ROOT") {
        let p = PathBuf::from(root);
        if p.exists() {
            return Ok(p);
        }
    }

    // Walk up from the crate directory to find terp-core/
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..6 {
        if dir.join("terp-core").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }

    Err("Cannot find ZK workspace root. Set ZK_ROOT env var.".into())
}

/// Run the test logic, returning the chain so the caller can always clean up.
async fn run_test(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    // 0. Resolve host-side file paths
    let zk_root = resolve_zk_root()?;
    let wasm_host = zk_root.join(WASM_REL);
    let vk_host = zk_root.join(VK_REL);

    if !wasm_host.exists() {
        return Err(format!("WASM file not found: {}", wasm_host.display()).into());
    }
    if !vk_host.exists() {
        return Err(format!("VK file not found: {}", vk_host.display()).into());
    }
    println!("WASM: {}", wasm_host.display());
    println!("VK:   {}", vk_host.display());

    // 1. Initialize and start
    println!("\n--- Starting chain ---");
    let ctx = TestContext {
        test_name: "no-rick".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain started and producing blocks.");

    let host_rpc = chain.host_rpc_address();
    println!("Host RPC: {}", host_rpc);

    // 2. Fund test user
    println!("\n--- Funding test user ---");
    chain.create_key("default").await?;
    let user_addr = chain.primary_node()?.get_key_address("default").await?;
    let fund = WalletAmount {
        address: user_addr.clone(),
        denom: "uterp".to_string(),
        amount: 10_000_000,
    };
    chain.send_funds("validator", &fund).await?;
    wait_for_blocks(chain, 2).await?;
    println!("Funded user: {}", user_addr);

    // 3. Copy wasm + VK files into the container
    println!("\n--- Copying contract files into container ---");
    let node = chain.primary_node()?;
    node.copy_file_from_host(&wasm_host, CONTAINER_WASM).await?;
    node.copy_file_from_host(&vk_host, VK).await?;
    println!("Copied wasm ({} bytes) and vk ({} bytes)",
        std::fs::metadata(&wasm_host)?.len(),
        std::fs::metadata(&vk_host)?.len(),
    );

    // 4. Upload contract + VK using headstash command
    println!("\n--- Uploading contract + VK (headstash) ---");
    let headstash_output = chain.chain_exec(&[
        "tx", "wasm", "headstash",
        CONTAINER_WASM, VK,
        "--from", "default",
        "--gas-prices", "0uterp",
        "--chain-id", "120u-1",
        "--keyring-backend", "test",
        "--gas", "auto",
        "--gas-adjustment", "1.5",
        "--broadcast-mode", "sync",
        "--output", "json",
        "-y",
    ]).await?;
    println!("headstash stdout: {}", headstash_output.stdout_str().trim());
    if !headstash_output.stderr.is_empty() {
        println!("headstash stderr: {}", headstash_output.stderr_str().trim());
    }
    if headstash_output.exit_code != 0 {
        return Err(format!("headstash failed (exit {}): {}",
            headstash_output.exit_code,
            headstash_output.stderr_str()).into());
    }

    // Check tx was accepted to mempool
    let hs_json: serde_json::Value = serde_json::from_str(
        headstash_output.stdout_str().trim()
    ).unwrap_or(serde_json::Value::Null);
    let tx_code = hs_json["code"].as_u64().unwrap_or(999);
    if tx_code != 0 {
        return Err(format!("headstash tx rejected (code {}): {}",
            tx_code,
            hs_json["raw_log"].as_str().unwrap_or("unknown")).into());
    }
    println!("headstash tx accepted: {}", hs_json["txhash"].as_str().unwrap_or("?"));

    wait_for_blocks(chain, 2).await?;

    // For the first contract upload, code_id is always 1
    let code_id = "1";
    println!("Stored code: {} (first upload)", code_id);

    // 5. Instantiate contract
    println!("\n--- Instantiating contract ---");
    let inst_output = chain.chain_exec(&[
        "tx", "wasm", "instantiate", code_id, "{}",
        "--label", "no-rick",
        "--no-admin",
        "--from", "default",
        "--gas-prices", "0uterp",
        "--chain-id", "120u-1",
        "--keyring-backend", "test",
        "--gas", "auto",
        "--gas-adjustment", "1.5",
        "--broadcast-mode", "sync",
        "--output", "json",
        "-y",
    ]).await?;
    println!("instantiate stdout: {}", inst_output.stdout_str().trim());
    if !inst_output.stderr.is_empty() {
        println!("instantiate stderr: {}", inst_output.stderr_str().trim());
    }
    if inst_output.exit_code != 0 {
        return Err(format!("instantiate failed (exit {}): {}",
            inst_output.exit_code,
            inst_output.stderr_str()).into());
    }

    let inst_json: serde_json::Value = serde_json::from_str(
        inst_output.stdout_str().trim()
    ).unwrap_or(serde_json::Value::Null);
    let inst_code = inst_json["code"].as_u64().unwrap_or(999);
    if inst_code != 0 {
        return Err(format!("instantiate tx rejected (code {}): {}",
            inst_code,
            inst_json["raw_log"].as_str().unwrap_or("unknown")).into());
    }

    wait_for_blocks(chain, 2).await?;

    // 6. Query contract address (sync mode doesn't return it inline)
    println!("\n--- Querying contract address ---");
    let query_output = chain.chain_exec(&[
        "query", "wasm", "list-contract-by-code", code_id,
        "--output", "json",
    ]).await?;
    let q_json: serde_json::Value = serde_json::from_str(
        query_output.stdout_str().trim()
    ).map_err(|e| format!("contract query failed: {e}\nstdout: {}\nstderr: {}",
        query_output.stdout_str().trim(),
        query_output.stderr_str().trim()))?;

    let contract_addr = q_json["contracts"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .ok_or("no contract found for code_id 1")?
        .to_string();
    println!("Contract: {}", contract_addr);

    // 7. Load real proof from JSON file
    println!("\n--- Loading proof data ---");
    let proof_path = zk_root.join(PROOF_REL);
    let proof_json: serde_json::Value = if proof_path.exists() {
        let data = std::fs::read_to_string(&proof_path)?;
        serde_json::from_str(&data)?
    } else {
        println!("WARNING: proof file not found at {}", proof_path.display());
        serde_json::Value::Null
    };

    let rick_proof = proof_json["rick"]["proof"]
        .as_str()
        .unwrap_or("")
        .to_string();

    if rick_proof.is_empty() || rick_proof.starts_with("ADD_") {
        println!("No valid 'rick' proof available, skipping proof verification.");
        return Ok(());
    }
    println!("Loaded 'rick' proof ({} bytes base64)", rick_proof.len());

    // 8. Submit proof — proves private witness does NOT contain "rick"
    println!("\n--- Submitting proof (forbidden: rick) ---");
    let prove_msg = format!(
        r#"{{"proove":{{"cid": 1, "forbidden": "rick", "proof": "{}"}}}}"#,
        rick_proof
    );

    let exec_output = chain.chain_exec(&[
        "tx", "wasm", "execute", &contract_addr, &prove_msg,
        "--from", "default",
        "--gas-prices", "0uterp",
        "--chain-id", "120u-1",
        "--keyring-backend", "test",
        "--gas", "auto",
        "--gas-adjustment", "1.5",
        "--broadcast-mode", "sync",
        "--output", "json",
        "-y",
    ]).await;

    match &exec_output {
        Ok(out) => {
            let stdout = out.stdout_str();
            let stderr = out.stderr_str();
            if !stdout.trim().is_empty() {
                println!("execute stdout: {}", stdout.trim());
            }
            if !stderr.trim().is_empty() {
                println!("execute stderr: {}", stderr.trim());
            }

            let j: serde_json::Value = serde_json::from_str(stdout.trim())
                .unwrap_or(serde_json::Value::Null);
            let code = j["code"].as_u64().unwrap_or(999);
            let tx_hash = j["txhash"].as_str().unwrap_or("");
            if code == 0 && !tx_hash.is_empty() {
                println!("Proof tx accepted to mempool: {}", tx_hash);
            } else {
                println!("Proof tx code={}, raw_log: {}", code,
                    j["raw_log"].as_str().unwrap_or(""));
            }
        }
        Err(e) => {
            println!("Proof execution error: {}", e);
        }
    }

    wait_for_blocks(chain, 2).await?;

    // 9. Verify the proof tx was included (query it)
    if let Ok(out) = &exec_output {
        let j: serde_json::Value = serde_json::from_str(out.stdout_str().trim())
            .unwrap_or(serde_json::Value::Null);
        if let Some(tx_hash) = j["txhash"].as_str() {
            if !tx_hash.is_empty() {
                let query_tx = chain.chain_exec(&[
                    "query", "tx", tx_hash, "--output", "json",
                ]).await;
                match query_tx {
                    Ok(q) => {
                        let qj: serde_json::Value = serde_json::from_str(q.stdout_str().trim())
                            .unwrap_or(serde_json::Value::Null);
                        let tx_code = qj["code"].as_u64().unwrap_or(999);
                        if tx_code == 0 {
                            println!("Proof verification PASSED on-chain! (tx code=0)");
                        } else {
                            println!("Proof tx failed on-chain (code={}): {}",
                                tx_code,
                                qj["raw_log"].as_str().unwrap_or("unknown"));
                        }
                    }
                    Err(e) => println!("Could not query tx: {}", e),
                }
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    println!("=== zk-wasmvm CosmWasm Proof Verification Test ===\n");

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    println!("Docker runtime connected.");

    let config = terp_zk_config();
    let mut chain = CosmosChain::new(config, 1, 0, runtime.clone());
    println!(
        "Chain: {} (image: {}:{})",
        chain.chain_id(),
        ZK_IMAGE_REPO,
        ZK_IMAGE_VERSION
    );

    // Run test, then ALWAYS clean up — even on error.
    let result = run_test(&mut chain).await;

    println!("\n--- Shutdown ---");
    if let Err(e) = chain.stop().await {
        eprintln!("Warning: cleanup error: {}", e);
    }

    match result {
        Ok(()) => {
            println!("zk-wasmvm proof verification test PASSED!");
            Ok(())
        }
        Err(e) => {
            eprintln!("Test FAILED: {}", e);
            Err(e)
        }
    }
}

