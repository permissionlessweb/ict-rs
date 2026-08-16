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

use ict_rs::prelude::*;

/// Contract wasm files (relative to the terp-core repo root).
const NOTE_WASM: &str = "tests/interchaintest/contracts/polytone_note.wasm";
const VOICE_WASM: &str = "tests/interchaintest/contracts/polytone_voice.wasm";
const PROXY_WASM: &str = "tests/interchaintest/contracts/polytone_proxy.wasm";
const TESTER_WASM: &str = "tests/interchaintest/contracts/polytone_tester.wasm";

/// Docker image to use. Override with TERP_IMAGE env var.
fn terp_image() -> DockerImage {
    let repo = std::env::var("TERP_IMAGE_REPO")
        .unwrap_or_else(|_| "terpnetwork/terp-core".to_string());
    let version = std::env::var("TERP_IMAGE_VERSION")
        .unwrap_or_else(|_| "local".to_string());
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

/// Directory that contains `tests/interchaintest/contracts/`.
/// Prefers runtime env (CI artifact jobs) over compile-time CARGO_MANIFEST_DIR.
fn resolve_terp_core() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let marker = "tests/interchaintest/contracts/polytone_note.wasm";
    let mut cands = Vec::new();
    for key in ["TERP_CORE", "GITHUB_WORKSPACE"] {
        if let Ok(v) = std::env::var(key) {
            cands.push(PathBuf::from(v));
        }
    }
    if let Ok(z) = std::env::var("ZK_ROOT") {
        cands.push(PathBuf::from(z.clone()).join("terp-core"));
        cands.push(PathBuf::from(z));
    }
    if let Ok(cwd) = std::env::current_dir() {
        cands.push(cwd);
    }
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..8 {
        cands.push(dir.clone());
        if !dir.pop() {
            break;
        }
    }
    for p in cands {
        if p.join(marker).is_file() {
            return Ok(p);
        }
    }
    Err("Cannot find terp-core (missing tests/interchaintest/contracts/*.wasm). Set TERP_CORE.".into())
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


/// Wait until `query tx` sees `txhash`, one produced block at a time.
async fn wait_until_tx(
    chain: &dyn Chain,
    txhash: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if txhash.is_empty() {
        return Err("empty txhash".into());
    }
    let start = chain.height().await.unwrap_or(0);
    loop {
        let tx_output = chain
            .chain_exec(&["query", "tx", txhash, "--output", "json"])
            .await;
        if let Ok(out) = tx_output {
            if let Ok(tx_json) = serde_json::from_str::<serde_json::Value>(out.stdout_str().trim()) {
                if tx_json.get("height").is_some() && !tx_json.get("height").unwrap().is_null() {
                    let code = tx_json["code"].as_u64().unwrap_or(0);
                    if code != 0 {
                        return Err(format!(
                            "tx {txhash} code={code}: {}",
                            tx_json["raw_log"].as_str().unwrap_or("")
                        )
                        .into());
                    }
                    return Ok(tx_json);
                }
            }
        }
        wait_for_blocks(chain, 1).await?;
        let h = chain.height().await.unwrap_or(0);
        if h > start + 15 {
            return Err(format!("tx {txhash} not included by height {h} (start {start})").into());
        }
    }
}

fn parse_account_seq(j: &serde_json::Value) -> Option<(u64, u64)> {
    let candidates = [
        j,
        &j["account"],
        &j["account"]["value"],
        &j["account"]["base_account"],
        &j["account"]["value"]["base_vesting_account"]["base_account"],
    ];
    for acc in candidates {
        let num = acc["account_number"]
            .as_u64()
            .or_else(|| acc["account_number"].as_str().and_then(|s| s.parse().ok()));
        // New accounts often omit sequence in amino JSON; that means 0.
        let seq = acc["sequence"]
            .as_u64()
            .or_else(|| acc["sequence"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);
        if let Some(num) = num {
            return Some((num, seq));
        }
    }
    None
}

async fn account_number_and_sequence(
    chain: &dyn Chain,
    addr: &str,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let out = chain
        .chain_exec(&["query", "auth", "account", addr, "--output", "json"])
        .await?;
    let j: serde_json::Value = serde_json::from_str(out.stdout_str().trim())?;
    parse_account_seq(&j).ok_or_else(|| format!("no sequence in account query: {j}").into())
}

/// Store each wasm, waiting for inclusion before the next (signer sequence).
async fn store_contracts_seq(
    chain: &dyn Chain,
    key_name: &str,
    paths: &[(&str, &str)],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut code_ids = Vec::new();
    for (label, path) in paths {
        let output = chain
            .chain_exec(&[
                "tx",
                "wasm",
                "store",
                path,
                "--from",
                key_name,
                "--gas-prices",
                "0uterp",
                "--chain-id",
                chain.chain_id(),
                "--keyring-backend",
                "test",
                "--gas",
                "auto",
                "--gas-adjustment",
                "2.0",
                "--broadcast-mode",
                "sync",
                "--output",
                "json",
                "-y",
            ])
            .await?;
        if output.exit_code != 0 {
            return Err(format!("store {label} failed: {}", output.stderr_str()).into());
        }
        let json: serde_json::Value = serde_json::from_str(output.stdout_str().trim())
            .unwrap_or(serde_json::Value::Null);
        let code = json["code"].as_u64().unwrap_or(999);
        if code != 0 {
            return Err(format!(
                "store {label} rejected (code {code}): {}",
                json["raw_log"].as_str().unwrap_or("unknown")
            )
            .into());
        }
        let txhash = json["txhash"].as_str().unwrap_or("").to_string();
        println!("  Store {label} tx={txhash}");
        let tx_json = wait_until_tx(chain, &txhash).await?;
        let code_id = extract_event_attr(&tx_json, "store_code", "code_id")
            .ok_or_else(|| format!("no code_id for {label} {txhash}"))?;
        println!("  {label} code_id={code_id}");
        code_ids.push(code_id);
    }
    Ok(code_ids)
}

async fn instantiate_contracts_seq(
    chain: &dyn Chain,
    key_name: &str,
    specs: &[(String, String, String)],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut addrs = Vec::new();
    for (code_id, msg, label) in specs {
        let output = chain
            .chain_exec(&[
                "tx",
                "wasm",
                "instantiate",
                code_id,
                msg,
                "--label",
                label,
                "--no-admin",
                "--from",
                key_name,
                "--gas-prices",
                "0uterp",
                "--chain-id",
                chain.chain_id(),
                "--keyring-backend",
                "test",
                "--gas",
                "auto",
                "--gas-adjustment",
                "2.0",
                "--broadcast-mode",
                "sync",
                "--output",
                "json",
                "-y",
            ])
            .await?;
        if output.exit_code != 0 {
            return Err(format!("instantiate {label} failed: {}", output.stderr_str()).into());
        }
        let json: serde_json::Value = serde_json::from_str(output.stdout_str().trim())
            .unwrap_or(serde_json::Value::Null);
        if json["code"].as_u64().unwrap_or(999) != 0 {
            return Err(format!(
                "instantiate {label} rejected: {}",
                json["raw_log"].as_str().unwrap_or("unknown")
            )
            .into());
        }
        let txhash = json["txhash"].as_str().unwrap_or("").to_string();
        println!("  Instantiate {label} tx={txhash}");
        wait_until_tx(chain, &txhash).await?;
        let q_output = chain
            .chain_exec(&[
                "query",
                "wasm",
                "list-contract-by-code",
                code_id,
                "--output",
                "json",
            ])
            .await?;
        let q_json: serde_json::Value = serde_json::from_str(q_output.stdout_str().trim())?;
        let contract_addr = q_json["contracts"]
            .as_array()
            .and_then(|arr| arr.last())
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("no contract for {label} code {code_id}"))?
            .to_string();
        println!("  {label}: {contract_addr}");
        addrs.push(contract_addr);
    }
    Ok(addrs)
}

struct Deployed {
    note_addr: String,
    voice_addr: String,
    tester_addr: String,
}

async fn deploy_one_chain(
    chain: &dyn Chain,
    key_name: &str,
    note_host: &PathBuf,
    voice_host: &PathBuf,
    proxy_host: &PathBuf,
    tester_host: &PathBuf,
) -> Result<(String, Deployed), Box<dyn std::error::Error>> {
    chain.create_key(key_name).await?;
    let user = key_address(chain, key_name).await?;
    chain
        .send_funds(
            "validator",
            &WalletAmount {
                address: user.clone(),
                denom: "uterp".to_string(),
                amount: 10_000_000_000,
            },
        )
        .await?;
    wait_until_funded(chain, &user).await?;

    let (note_p, voice_p, proxy_p, tester_p) = tokio::try_join!(
        copy_wasm_to_chain(chain, note_host, "polytone_note.wasm"),
        copy_wasm_to_chain(chain, voice_host, "polytone_voice.wasm"),
        copy_wasm_to_chain(chain, proxy_host, "polytone_proxy.wasm"),
        copy_wasm_to_chain(chain, tester_host, "polytone_tester.wasm"),
    )?;

    let codes = store_contracts_seq(
        chain,
        key_name,
        &[
            ("note", note_p.as_str()),
            ("voice", voice_p.as_str()),
            ("proxy", proxy_p.as_str()),
            ("tester", tester_p.as_str()),
        ],
    )
    .await?;
    let note_code = &codes[0];
    let voice_code = &codes[1];
    let proxy_code = &codes[2];
    let tester_code = &codes[3];

    let voice_init = format!(
        r#"{{"proxy_code_id":"{proxy_code}","block_max_gas":"100000000","contract_addr_len":32}}"#
    );
    let addrs = instantiate_contracts_seq(
        chain,
        key_name,
        &[
            (
                note_code.clone(),
                r#"{"block_max_gas":"100000000"}"#.to_string(),
                "polytone-note".to_string(),
            ),
            (
                voice_code.clone(),
                voice_init,
                "polytone-voice".to_string(),
            ),
            (tester_code.clone(), "{}".to_string(), "polytone-tester".to_string()),
        ],
    )
    .await?;
    Ok((
        user,
        Deployed {
            note_addr: addrs[0].clone(),
            voice_addr: addrs[1].clone(),
            tester_addr: addrs[2].clone(),
        },
    ))
}

async fn wait_until_funded(
    chain: &dyn Chain,
    addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = chain.height().await.unwrap_or(0);
    loop {
        let out = chain
            .chain_exec(&["query", "bank", "balances", addr, "--output", "json"])
            .await?;
        if out.stdout_str().contains("uterp") {
            return Ok(());
        }
        wait_for_blocks(chain, 1).await?;
        if chain.height().await.unwrap_or(0) > start + 10 {
            return Err(format!("fund to {addr} not visible").into());
        }
    }
}

async fn deploy_polytone_contracts(
    ic: &Interchain,
    note_host: &PathBuf,
    voice_host: &PathBuf,
    proxy_host: &PathBuf,
    tester_host: &PathBuf,
) -> Result<(Deployed, Deployed), Box<dyn std::error::Error>> {
    let chain_a = ic.get_chain("chain-a").unwrap();
    let chain_b = ic.get_chain("chain-b").unwrap();
    println!("--- Deploying contracts (sequential stores per chain) ---");
    let (_, dep_a) = deploy_one_chain(chain_a, "user-a", note_host, voice_host, proxy_host, tester_host).await?;
    let (_, dep_b) = deploy_one_chain(chain_b, "user-b", note_host, voice_host, proxy_host, tester_host).await?;
    Ok((dep_a, dep_b))
}


async fn find_ibc_channel(
    chain: &dyn Chain,
    port_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let out = chain
        .chain_exec(&["query", "ibc", "channel", "channels", "--output", "json"])
        .await?;
    let j: serde_json::Value = serde_json::from_str(out.stdout_str().trim())?;
    let chans = j["channels"]
        .as_array()
        .ok_or_else(|| format!("no channels array: {j}"))?;
    for c in chans {
        if c["port_id"].as_str() == Some(port_id) {
            let id = c["channel_id"].as_str().unwrap_or("").to_string();
            let state = c["state"].as_str().unwrap_or("");
            println!("  on-chain channel {id} port={port_id} state={state}");
            if !id.is_empty() {
                return Ok(id);
            }
        }
    }
    Err(format!("no channel on port {port_id}: {j}").into())
}

/// Run the polytone test.
async fn run_test(ic: &mut Interchain, deployed: Option<(Deployed, Deployed)>) -> Result<(), Box<dyn std::error::Error>> {
    let zk_root = resolve_terp_core()?;

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
    // Contracts were deployed in parallel with relayer setup (see main).
    let (dep_a, dep_b) = match deployed {
        Some(d) => d,
        None => deploy_polytone_contracts(ic, &note_host, &voice_host, &proxy_host, &tester_host).await?,
    };
    let note_addr_a = dep_a.note_addr;
    let tester_addr_a = dep_a.tester_addr;
    let voice_addr_b = dep_b.voice_addr;
    let tester_addr_b = dep_b.tester_addr;

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
    // Hermes query-channels JSON does not deserialize wasm ports reliably.
    let poly_channel_id = find_ibc_channel(chain_a, &src_port).await?;
    // Start Hermes only after the wasm channel exists so packet workers
    // attach to wasm.<note> / wasm.<voice>, not just transfer.
    println!("  Starting Hermes against transfer + polytone channels...");
    ic.start_relayers().await?;
    wait_for_blocks(chain_a, 1).await?;
    println!(
        "  Polytone channel created: {} ({}) <-> {}",
        poly_channel_id, src_port, dst_port
    );
    wait_for_blocks(chain_a, 1).await?;

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
            "timeout_seconds": "600",
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
    let exec_json: serde_json::Value = serde_json::from_str(exec_output.stdout_str().trim())
        .unwrap_or(serde_json::Value::Null);
    let txhash = exec_json["txhash"].as_str().unwrap_or("").to_string();
    if txhash.is_empty() {
        return Err("note execute produced no txhash".into());
    }
    let mut tx_code: Option<u64> = exec_json["code"].as_u64();
    for _ in 0..20 {
        wait_for_blocks(chain_a, 1).await?;
        let q = chain_a
            .chain_exec(&["query", "tx", &txhash, "--output", "json"])
            .await;
        if let Ok(out) = q {
            if let Ok(j) = serde_json::from_str::<serde_json::Value>(out.stdout_str().trim()) {
                tx_code = j["code"].as_u64();
                break;
            }
        }
    }
    if tx_code != Some(0) {
        return Err(format!("note execute tx {txhash} code={tx_code:?}").into());
    }
    println!("  Execute included: {txhash}");
    relayer.flush("polytone-path", &poly_channel_id).await?;

    // Relay both legs (A->B execute, B->A callback). Event-only Hermes
    // (clear_interval used to be 0) will miss send_packet if the websocket
    // drops it; flush the wasm channel after each wait.
    println!("\n--- Waiting for IBC relay ---");
    const WANT_INITIATOR_MSG: &str = "aGVsbG8K"; // base64("hello\n")

    let mut callback_hist = serde_json::Value::Null;
    let mut hello_hist = serde_json::Value::Null;
    let mut got_callback = false;
    let mut got_hello = false;
    for attempt in 1..=15 {
        relayer.flush("polytone-path", &poly_channel_id).await?;
        wait_for_blocks(chain_a, 2).await?;
        wait_for_blocks(chain_b, 2).await?;

        let cb_out = chain_a
            .chain_exec(&[
                "query",
                "wasm",
                "contract-state",
                "smart",
                &tester_addr_a,
                r#"{"history":{}}"#,
                "--output",
                "json",
            ])
            .await?;
        callback_hist = serde_json::from_str(cb_out.stdout_str().trim())?;
        let hello_out = chain_b
            .chain_exec(&[
                "query",
                "wasm",
                "contract-state",
                "smart",
                &tester_addr_b,
                r#"{"hello_history":{}}"#,
                "--output",
                "json",
            ])
            .await?;
        hello_hist = serde_json::from_str(hello_out.stdout_str().trim())?;
        println!("  attempt {attempt} callback={}", callback_hist);
        println!("  attempt {attempt} hello_history={}", hello_hist);

        let hist = callback_hist
            .pointer("/data/history")
            .and_then(|v| v.as_array());
        got_callback = hist.map(|h| !h.is_empty()).unwrap_or(false);
        let hello = hello_hist
            .pointer("/data/history")
            .and_then(|v| v.as_array());
        got_hello = hello.map(|h| !h.is_empty()).unwrap_or(false);
        if got_callback && got_hello {
            break;
        }
    }

    if !got_hello {
        return Err(format!(
            "forward packet never executed on chain-b tester (hello_history empty): {hello_hist}"
        )
        .into());
    }
    if !got_callback {
        return Err(format!(
            "callback packet never returned to chain-a tester (history empty): {callback_hist}"
        )
        .into());
    }
    let last = callback_hist
        .pointer("/data/history")
        .and_then(|v| v.as_array())
        .and_then(|h| h.last())
        .cloned()
        .unwrap();
    let initiator_msg = last
        .get("initiator_msg")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if initiator_msg != WANT_INITIATOR_MSG {
        return Err(format!(
            "callback initiator_msg={initiator_msg:?} want {WANT_INITIATOR_MSG:?} last={last}"
        )
        .into());
    }
    if last
        .pointer("/result/error")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        return Err(format!("callback result.error set: {last}").into());
    }
    println!("  Forward hello_history and callback history both non-empty");
    println!("  Callback initiator_msg={initiator_msg} — polytone roundtrip succeeded");

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

    let opts = InterchainBuildOptions {
        test_name: test_name.to_string(),
        ..Default::default()
    };
    println!("\nStarting chains...");
    ic.start_chains(&opts).await?;
    println!("Chains producing blocks. Deploying contracts || funding Hermes...");

    let zk_root = resolve_terp_core()?;
    let note_host = zk_root.join(NOTE_WASM);
    let voice_host = zk_root.join(VOICE_WASM);
    let proxy_host = zk_root.join(PROXY_WASM);
    let tester_host = zk_root.join(TESTER_WASM);
    for (name, path) in [
        ("note", &note_host),
        ("voice", &voice_host),
        ("proxy", &proxy_host),
        ("tester", &tester_host),
    ] {
        if !path.exists() {
            return Err(format!("Missing {name} wasm: {}", path.display()).into());
        }
    }

    let deploy_f = deploy_polytone_contracts(&ic, &note_host, &voice_host, &proxy_host, &tester_host);
    let relay_f = ic.configure_relayers_and_paths_without_start();
    let (deployed, relay_res) = tokio::join!(deploy_f, relay_f);
    relay_res?;
    let deployed = deployed?;
    ic.mark_built();
    println!("Interchain environment ready + contracts deployed.");

    let result = run_test(&mut ic, Some(deployed)).await;

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
