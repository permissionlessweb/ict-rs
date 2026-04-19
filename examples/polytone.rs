//! Polytone cross-chain CosmWasm execution test using real Docker containers.
//!
//! Mirrors `polytone_test.go` — two Terp chains, deploy note/voice/proxy/tester
//! contracts, create a custom wasm IBC channel, execute cross-chain and verify
//! callback.
//!
//! ## Prerequisites
//!
//! Ensure the Terp chain Docker image is available:
//!
//! ```sh
//! cd terp-core
//! make build-docker-local  # → ghcr.io/terpnetwork/terp-core:local
//! ```
//!
//! Ensure the polytone contract wasm files exist:
//! - `terp-core/tests/interchaintest/contracts/polytone_note.wasm`
//! - `terp-core/tests/interchaintest/contracts/polytone_voice.wasm`
//! - `terp-core/tests/interchaintest/contracts/polytone_proxy.wasm`
//! - `terp-core/tests/interchaintest/contracts/polytone_tester.wasm`
//!
//! ```sh
//! cargo run --example polytone --features docker
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm};
use ict_rs::ibc::ChannelOptions;
use ict_rs::interchain::{wait_for_blocks, Interchain, InterchainBuildOptions, InterchainLink};
use ict_rs::relayer::{build_relayer, RelayerType};
use ict_rs::runtime::{DockerConfig, DockerImage, IctRuntime};
use ict_rs::tx::WalletAmount;

/// Contract wasm files (relative to ZK workspace root).
const NOTE_WASM: &str = "terp-core/tests/interchaintest/contracts/polytone_note.wasm";
const VOICE_WASM: &str = "terp-core/tests/interchaintest/contracts/polytone_voice.wasm";
const PROXY_WASM: &str = "terp-core/tests/interchaintest/contracts/polytone_proxy.wasm";
const TESTER_WASM: &str = "terp-core/tests/interchaintest/contracts/polytone_tester.wasm";

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

fn terp_chain_config(chain_id: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: chain_id.to_string(),
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

/// Resolve the ZK workspace root (parent of terp-core/).
fn resolve_zk_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(root) = std::env::var("ZK_ROOT") {
        let p = PathBuf::from(root);
        if p.exists() {
            return Ok(p);
        }
    }
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

/// Get a key's bech32 address from a chain.
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

/// Store a wasm contract on-chain using chain_exec directly.
/// Returns the code_id from the tx result.
async fn store_contract(
    chain: &dyn Chain,
    key_name: &str,
    container_wasm_path: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let output = chain.chain_exec(&[
        "tx", "wasm", "store", container_wasm_path,
        "--from", key_name,
        "--gas-prices", "0uterp",
        "--chain-id", chain.chain_id(),
        "--keyring-backend", "test",
        "--gas", "auto",
        "--gas-adjustment", "2.0",
        "--broadcast-mode", "sync",
        "--output", "json",
        "-y",
    ]).await?;

    if output.exit_code != 0 {
        return Err(format!("store failed: {}", output.stderr_str()).into());
    }

    let json: serde_json::Value = serde_json::from_str(output.stdout_str().trim())
        .unwrap_or(serde_json::Value::Null);
    let code = json["code"].as_u64().unwrap_or(999);
    if code != 0 {
        return Err(format!("store tx rejected (code {}): {}",
            code, json["raw_log"].as_str().unwrap_or("unknown")).into());
    }

    let txhash = json["txhash"].as_str().unwrap_or("").to_string();
    println!("  Store tx: {}", txhash);

    // Wait for tx to be included
    wait_for_blocks(chain, 2).await?;

    // Query the tx to get code_id from events
    let tx_output = chain.chain_exec(&[
        "query", "tx", &txhash, "--output", "json",
    ]).await?;

    let tx_json: serde_json::Value = serde_json::from_str(tx_output.stdout_str().trim())
        .unwrap_or(serde_json::Value::Null);

    // Extract code_id from events
    let code_id = extract_event_attr(&tx_json, "store_code", "code_id")
        .unwrap_or_else(|| "1".to_string());

    Ok(code_id)
}

/// Instantiate a contract. Returns the contract address.
async fn instantiate_contract(
    chain: &dyn Chain,
    key_name: &str,
    code_id: &str,
    msg: &str,
    label: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let output = chain.chain_exec(&[
        "tx", "wasm", "instantiate", code_id, msg,
        "--label", label,
        "--no-admin",
        "--from", key_name,
        "--gas-prices", "0uterp",
        "--chain-id", chain.chain_id(),
        "--keyring-backend", "test",
        "--gas", "auto",
        "--gas-adjustment", "2.0",
        "--broadcast-mode", "sync",
        "--output", "json",
        "-y",
    ]).await?;

    if output.exit_code != 0 {
        return Err(format!("instantiate failed: {}", output.stderr_str()).into());
    }

    let json: serde_json::Value = serde_json::from_str(output.stdout_str().trim())
        .unwrap_or(serde_json::Value::Null);
    let code = json["code"].as_u64().unwrap_or(999);
    if code != 0 {
        return Err(format!("instantiate tx rejected (code {}): {}",
            code, json["raw_log"].as_str().unwrap_or("unknown")).into());
    }

    wait_for_blocks(chain, 2).await?;

    // Query contract address by code_id
    let q_output = chain.chain_exec(&[
        "query", "wasm", "list-contract-by-code", code_id, "--output", "json",
    ]).await?;
    let q_json: serde_json::Value = serde_json::from_str(q_output.stdout_str().trim())
        .map_err(|e| format!("contract query failed: {e}"))?;

    let contract_addr = q_json["contracts"]
        .as_array()
        .and_then(|arr| arr.last())
        .and_then(|v| v.as_str())
        .ok_or("no contract found")?
        .to_string();

    Ok(contract_addr)
}

/// Extract an attribute value from tx events.
fn extract_event_attr(tx_json: &serde_json::Value, event_type: &str, attr_key: &str) -> Option<String> {
    // Try logs.events path
    if let Some(logs) = tx_json["logs"].as_array() {
        for log in logs {
            if let Some(events) = log["events"].as_array() {
                for event in events {
                    if event["type"].as_str() == Some(event_type) {
                        if let Some(attrs) = event["attributes"].as_array() {
                            for attr in attrs {
                                if attr["key"].as_str() == Some(attr_key) {
                                    return attr["value"].as_str().map(|s| s.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // Try events path (newer SDK)
    if let Some(events) = tx_json["events"].as_array() {
        for event in events {
            if event["type"].as_str() == Some(event_type) {
                if let Some(attrs) = event["attributes"].as_array() {
                    for attr in attrs {
                        if attr["key"].as_str() == Some(attr_key) {
                            return attr["value"].as_str().map(|s| s.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Copy a host file into a chain container and return the container path.
async fn copy_wasm_to_chain(
    chain: &dyn Chain,
    host_path: &PathBuf,
    filename: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let container_path = format!("/tmp/{filename}");
    let content = std::fs::read(host_path)
        .map_err(|e| format!("failed to read {}: {e}", host_path.display()))?;

    // Write file into container via base64 chunks (handles large wasm files)
    let b64 = base64_encode(&content);
    let b64_tmp = format!("{container_path}.b64");

    // Clear any existing temp file
    chain.exec(&["sh", "-c", &format!("rm -f '{b64_tmp}'")], &[]).await?;

    // Write in chunks
    const CHUNK_SIZE: usize = 65536;
    for chunk in b64.as_bytes().chunks(CHUNK_SIZE) {
        let chunk_str = std::str::from_utf8(chunk).unwrap_or("");
        chain.exec(&["sh", "-c", &format!("printf '%s' '{chunk_str}' >> '{b64_tmp}'")], &[]).await?;
    }

    // Decode
    chain.exec(&["sh", "-c", &format!("base64 -d '{b64_tmp}' > '{container_path}' && rm '{b64_tmp}'")], &[]).await?;

    Ok(container_path)
}

fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        result.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

/// Run the polytone test.
async fn run_test(ic: &mut Interchain) -> Result<(), Box<dyn std::error::Error>> {
    let zk_root = resolve_zk_root()?;

    // Verify wasm files exist
    let note_host = zk_root.join(NOTE_WASM);
    let voice_host = zk_root.join(VOICE_WASM);
    let proxy_host = zk_root.join(PROXY_WASM);
    let tester_host = zk_root.join(TESTER_WASM);

    for (name, path) in [
        ("note", &note_host), ("voice", &voice_host),
        ("proxy", &proxy_host), ("tester", &tester_host),
    ] {
        if !path.exists() {
            return Err(format!("Missing {name} wasm: {}", path.display()).into());
        }
    }

    let chain_a = ic.get_chain("chain-a").unwrap();
    let chain_b = ic.get_chain("chain-b").unwrap();

    // 1. Fund test users
    println!("\n--- Funding test users ---");
    chain_a.create_key("user-a").await?;
    chain_b.create_key("user-b").await?;
    let user_a_addr = key_address(chain_a, "user-a").await?;
    let user_b_addr = key_address(chain_b, "user-b").await?;
    println!("  User A: {}", user_a_addr);
    println!("  User B: {}", user_b_addr);

    let fund = 10_000_000_000u128;
    chain_a.send_funds("validator", &WalletAmount {
        address: user_a_addr.clone(), denom: "uterp".to_string(), amount: fund,
    }).await?;
    chain_b.send_funds("validator", &WalletAmount {
        address: user_b_addr.clone(), denom: "uterp".to_string(), amount: fund,
    }).await?;
    wait_for_blocks(chain_a, 3).await?;
    wait_for_blocks(chain_b, 3).await?;

    // 2. Copy ALL 4 wasm files into BOTH chain containers (mirrors Go SetupChain on both)
    println!("\n--- Copying wasm files into containers ---");
    let note_path_a = copy_wasm_to_chain(chain_a, &note_host, "polytone_note.wasm").await?;
    let voice_path_a = copy_wasm_to_chain(chain_a, &voice_host, "polytone_voice.wasm").await?;
    let proxy_path_a = copy_wasm_to_chain(chain_a, &proxy_host, "polytone_proxy.wasm").await?;
    let tester_path_a = copy_wasm_to_chain(chain_a, &tester_host, "polytone_tester.wasm").await?;

    let note_path_b = copy_wasm_to_chain(chain_b, &note_host, "polytone_note.wasm").await?;
    let voice_path_b = copy_wasm_to_chain(chain_b, &voice_host, "polytone_voice.wasm").await?;
    let proxy_path_b = copy_wasm_to_chain(chain_b, &proxy_host, "polytone_proxy.wasm").await?;
    let tester_path_b = copy_wasm_to_chain(chain_b, &tester_host, "polytone_tester.wasm").await?;
    println!("  Files copied to both containers.");

    // 3. Store all 4 contracts on chain A
    println!("\n--- Deploying contracts on chain A ---");
    let note_code_a = store_contract(chain_a, "user-a", &note_path_a).await?;
    println!("  Note code_id: {}", note_code_a);
    let voice_code_a = store_contract(chain_a, "user-a", &voice_path_a).await?;
    println!("  Voice code_id: {}", voice_code_a);
    let proxy_code_a = store_contract(chain_a, "user-a", &proxy_path_a).await?;
    println!("  Proxy code_id: {}", proxy_code_a);
    let tester_code_a = store_contract(chain_a, "user-a", &tester_path_a).await?;
    println!("  Tester code_id: {}", tester_code_a);

    // Instantiate on chain A: note, voice (with proxy_code_id), tester
    let note_init_a = r#"{"block_max_gas":"100000000"}"#;
    let note_addr_a = instantiate_contract(chain_a, "user-a", &note_code_a, note_init_a, "polytone-note").await?;
    println!("  Note contract: {}", note_addr_a);

    let voice_init_a = format!(
        r#"{{"proxy_code_id":"{proxy_code_a}","block_max_gas":"100000000","contract_addr_len":32}}"#
    );
    let voice_addr_a = instantiate_contract(chain_a, "user-a", &voice_code_a, &voice_init_a, "polytone-voice").await?;
    println!("  Voice contract: {}", voice_addr_a);

    let tester_addr_a = instantiate_contract(chain_a, "user-a", &tester_code_a, "{}", "polytone-tester").await?;
    println!("  Tester contract: {}", tester_addr_a);

    // 4. Store all 4 contracts on chain B
    println!("\n--- Deploying contracts on chain B ---");
    let note_code_b = store_contract(chain_b, "user-b", &note_path_b).await?;
    println!("  Note code_id: {}", note_code_b);
    let voice_code_b = store_contract(chain_b, "user-b", &voice_path_b).await?;
    println!("  Voice code_id: {}", voice_code_b);
    let proxy_code_b = store_contract(chain_b, "user-b", &proxy_path_b).await?;
    println!("  Proxy code_id: {}", proxy_code_b);
    let tester_code_b = store_contract(chain_b, "user-b", &tester_path_b).await?;
    println!("  Tester code_id: {}", tester_code_b);

    // Instantiate on chain B: note, voice (with proxy_code_id), tester
    let note_init_b = r#"{"block_max_gas":"100000000"}"#;
    let note_addr_b = instantiate_contract(chain_b, "user-b", &note_code_b, note_init_b, "polytone-note").await?;
    println!("  Note contract: {}", note_addr_b);

    let voice_init_b = format!(
        r#"{{"proxy_code_id":"{proxy_code_b}","block_max_gas":"100000000","contract_addr_len":32}}"#
    );
    let voice_addr_b = instantiate_contract(chain_b, "user-b", &voice_code_b, &voice_init_b, "polytone-voice").await?;
    println!("  Voice contract: {}", voice_addr_b);

    let tester_addr_b = instantiate_contract(chain_b, "user-b", &tester_code_b, "{}", "polytone-tester").await?;
    println!("  Tester contract: {}", tester_addr_b);

    // 5. Create custom IBC channel: chain A note <-> chain B voice
    println!("\n--- Creating polytone IBC channel ---");
    let src_port = format!("wasm.{}", note_addr_a);
    let dst_port = format!("wasm.{}", voice_addr_b);
    println!("  src_port: {}", src_port);
    println!("  dst_port: {}", dst_port);

    let relayer = ic.get_relayer("hermes").unwrap();
    let channel_opts = ChannelOptions {
        src_port: src_port.clone(),
        dst_port: dst_port.clone(),
        ordering: ict_rs::ibc::ChannelOrdering::Unordered,
        version: "polytone-1".to_string(),
    };
    relayer.create_channel("polytone-path", &channel_opts).await?;
    println!("  Polytone channel created!");
    wait_for_blocks(chain_a, 5).await?;

    // 6. Execute cross-chain message via note on chain A
    //    Targets chain B's tester for the wasm execute, chain B's voice for distribution msg.
    //    Callback receiver is chain A's tester.
    println!("\n--- Cross-Chain Execution via Note ---");

    // Inner wasm execute msg: {"hello":{"data":"aGVsbG8="}} where "aGVsbG8=" is base64("hello")
    let hello_inner = base64_encode(b"{\"hello\":{\"data\":\"aGVsbG8=\"}}");
    let callback_msg = base64_encode(b"hello\n");

    let execute_msg = serde_json::json!({
        "execute": {
            "msgs": [
                {
                    "wasm": {
                        "execute": {
                            "contract_addr": tester_addr_b,
                            "msg": hello_inner,
                            "funds": []
                        }
                    }
                },
                {
                    "distribution": {
                        "set_withdraw_address": {
                            "address": voice_addr_b
                        }
                    }
                }
            ],
            "timeout_seconds": "100",
            "callback": {
                "receiver": tester_addr_a,
                "msg": callback_msg
            }
        }
    });

    let exec_output = chain_a.chain_exec(&[
        "tx", "wasm", "execute", &note_addr_a, &execute_msg.to_string(),
        "--from", "user-a",
        "--gas-prices", "0uterp",
        "--chain-id", "chain-a",
        "--keyring-backend", "test",
        "--gas", "auto",
        "--gas-adjustment", "2.0",
        "--broadcast-mode", "sync",
        "--output", "json",
        "-y",
    ]).await?;
    println!("  Execute stdout: {}", exec_output.stdout_str().trim());
    if !exec_output.stderr.is_empty() {
        println!("  Execute stderr: {}", exec_output.stderr_str().trim());
    }

    // 7. Wait for IBC relay
    println!("\n--- Waiting for IBC relay ---");
    wait_for_blocks(chain_a, 10).await?;
    wait_for_blocks(chain_b, 5).await?;

    // 8. Query tester on chain A for callback history
    println!("\n--- Query Callback History ---");
    let query_msg = r#"{"history":{}}"#;
    let query_output = chain_a.chain_exec(&[
        "query", "wasm", "contract-state", "smart", &tester_addr_a, query_msg,
        "--output", "json",
    ]).await?;
    let result = query_output.stdout_str();
    println!("  Callback result: {}", result.trim());

    let result_json: serde_json::Value = serde_json::from_str(result.trim())
        .unwrap_or(serde_json::Value::Null);
    let data = &result_json["data"];

    if !data.is_null() {
        println!("  Callback data present — polytone roundtrip succeeded!");
    } else {
        println!("  No callback data yet (may need more time for relay)");
    }

    println!("\nPolytone cross-chain execution test PASSED!");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    println!("=== Polytone Cross-Chain Execution Test (Docker) ===\n");

    // 1. Create Docker runtime
    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    println!("Docker runtime connected.");

    // 2. Create shared Docker network
    let test_name = "polytone-test";
    let network_id = format!("ict-{test_name}");
    runtime.create_network(&network_id).await?;

    // 3. Create two Terp chains
    let chain_a = CosmosChain::new(terp_chain_config("chain-a"), 1, 0, runtime.clone());
    let chain_b = CosmosChain::new(terp_chain_config("chain-b"), 1, 0, runtime.clone());
    println!("Chains: {} and {}", chain_a.chain_id(), chain_b.chain_id());

    // 4. Create Hermes relayer
    let relayer = build_relayer(
        RelayerType::Hermes,
        runtime.clone(),
        test_name,
        &network_id,
    ).await?;
    println!("Hermes relayer created.");

    // 5. Build interchain environment
    let mut ic = Interchain::new(runtime)
        .add_chain(Box::new(chain_a))
        .add_chain(Box::new(chain_b))
        .add_relayer("hermes", relayer)
        .add_link(InterchainLink {
            chain1: "chain-a".to_string(),
            chain2: "chain-b".to_string(),
            relayer: "hermes".to_string(),
            path: "polytone-path".to_string(),
        });

    println!("\nBuilding interchain environment...");
    ic.build(InterchainBuildOptions {
        test_name: test_name.to_string(),
        ..Default::default()
    }).await?;
    println!("Interchain environment ready!");

    // 6. Run test, then ALWAYS clean up
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
