//! Polytone IBC → `CosmosMsg::Any` → zk_no_rick `proove` (Docker).
//!
//! Chain A note sends `WasmMsg::Execute` to `polytone-any-proxy` on B.
//! The any-proxy emits protobuf `MsgExecuteContract` (`CosmosMsg::Any`)
//! targeting `zk_no_rick` with a fixture Halo2 proof.
//!
//! ```sh
//! make build-zk-local   # terpnetwork/terp-core:local-zk
//! cargo test -p polytone-any-proxy
//! cargo build -p polytone-any-proxy --target wasm32-unknown-unknown --release --lib
//! # wasm → terp-rs/artifacts/polytone_any_proxy.wasm
//! cargo run -p ict-rs --example polytone_any_norick --features docker
//! ```
//!
//! See `terp-rs/docs/ibc/POLYTONE-ANY-PROXY.md`.

use std::collections::HashMap;
use std::path::PathBuf;

use ict_rs::prelude::*;

const NOTE_WASM: &str = "tests/interchaintest/contracts/polytone_note.wasm";
const VOICE_WASM: &str = "tests/interchaintest/contracts/polytone_voice.wasm";
const PROXY_WASM: &str = "tests/interchaintest/contracts/polytone_proxy.wasm";
const TESTER_WASM: &str = "tests/interchaintest/contracts/polytone_tester.wasm";
const NORICK_WASM: &str = "tests/interchaintest/contracts/zk_no_rick.wasm";
const NORICK_VK: &str = "tests/interchaintest/circuits/no_rick.bin";
const NORICK_PROOF: &str = "tests/interchaintest/circuits/no_rick_proof.json";

fn zk_image() -> DockerImage {
    DockerImage {
        repository: std::env::var("TERP_IMAGE_REPO")
            .unwrap_or_else(|_| "terpnetwork/terp-core".into()),
        version: std::env::var("TERP_IMAGE_VERSION").unwrap_or_else(|_| "local-zk".into()),
        uid_gid: None,
    }
}

fn terp_chain_config(chain_id: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".into(),
        chain_id: chain_id.into(),
        images: vec![zk_image()],
        bin: "terpd".into(),
        bech32_prefix: "terp".into(),
        denom: "uterp".into(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".into(),
        gas_adjustment: 2.0,
        trusting_period: "112h".into(),
        block_time: "2s".into(),
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

fn resolve_terp_core() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let marker = NOTE_WASM;
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
    Err("Cannot find terp-core (polytone wasm). Set TERP_CORE.".into())
}

fn resolve_any_proxy_wasm() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(p) = std::env::var("POLYTONE_ANY_PROXY_WASM") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Ok(pb);
        }
    }
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..8 {
        let cand = dir.join("terp-rs/artifacts/polytone_any_proxy.wasm");
        if cand.is_file() {
            return Ok(cand);
        }
        let cand = dir.join("crates/terp-rs/artifacts/polytone_any_proxy.wasm");
        if cand.is_file() {
            return Ok(cand);
        }
        if !dir.pop() {
            break;
        }
    }
    Err(
        "missing polytone_any_proxy.wasm — build -p polytone-any-proxy (wasm32) into terp-rs/artifacts/ or set POLYTONE_ANY_PROXY_WASM"
            .into(),
    )
}

async fn key_address(
    chain: &dyn Chain,
    key_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let output = chain
        .chain_exec(&["keys", "show", key_name, "-a", "--keyring-backend", "test"])
        .await?;
    let addr = output.stdout_str().trim().to_string();
    if addr.is_empty() {
        return Err(format!("empty address for '{key_name}'").into());
    }
    Ok(addr)
}

fn extract_event_attr(tx_json: &serde_json::Value, event_type: &str, attr_key: &str) -> Option<String> {
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

fn proto_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push(((v as u8) & 0x7f) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

fn proto_ld(field: u32, data: &[u8]) -> Vec<u8> {
    let tag = (field << 3) | 2;
    let mut out = Vec::new();
    proto_varint(&mut out, tag as u64);
    proto_varint(&mut out, data.len() as u64);
    out.extend_from_slice(data);
    out
}

fn encode_msg_execute_contract(sender: &str, contract: &str, msg: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(proto_ld(1, sender.as_bytes()));
    body.extend(proto_ld(2, contract.as_bytes()));
    body.extend(proto_ld(3, msg));
    body
}

async fn copy_wasm_to_chain(
    chain: &dyn Chain,
    host_path: &PathBuf,
    filename: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let container_path = format!("/tmp/{filename}");
    let content = std::fs::read(host_path)
        .map_err(|e| format!("failed to read {}: {e}", host_path.display()))?;
    let b64 = base64_encode(&content);
    let b64_tmp = format!("{container_path}.b64");
    chain
        .exec(&["sh", "-c", &format!("rm -f '{b64_tmp}'")], &[])
        .await?;
    const CHUNK_SIZE: usize = 65536;
    for chunk in b64.as_bytes().chunks(CHUNK_SIZE) {
        let chunk_str = std::str::from_utf8(chunk).unwrap_or("");
        chain
            .exec(
                &[
                    "sh",
                    "-c",
                    &format!("printf '%s' '{chunk_str}' >> '{b64_tmp}'"),
                ],
                &[],
            )
            .await?;
    }
    chain
        .exec(
            &[
                "sh",
                "-c",
                &format!("base64 -d '{b64_tmp}' > '{container_path}' && rm '{b64_tmp}'"),
            ],
            &[],
        )
        .await?;
    Ok(container_path)
}

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
        if h > start + 20 {
            return Err(format!("tx {txhash} not included by height {h}").into());
        }
    }
}

async fn store_one(
    chain: &dyn Chain,
    key_name: &str,
    label: &str,
    path: &str,
) -> Result<String, Box<dyn std::error::Error>> {
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
    let json: serde_json::Value =
        serde_json::from_str(output.stdout_str().trim()).unwrap_or(serde_json::Value::Null);
    if json["code"].as_u64().unwrap_or(999) != 0 {
        return Err(format!(
            "store {label} rejected: {}",
            json["raw_log"].as_str().unwrap_or("unknown")
        )
        .into());
    }
    let txhash = json["txhash"].as_str().unwrap_or("").to_string();
    let tx_json = wait_until_tx(chain, &txhash).await?;
    let code_id = extract_event_attr(&tx_json, "store_code", "code_id")
        .ok_or_else(|| format!("no code_id for {label}"))?;
    println!("  {label} code_id={code_id}");
    Ok(code_id)
}

async fn instantiate_one(
    chain: &dyn Chain,
    key_name: &str,
    code_id: &str,
    msg: &str,
    label: &str,
) -> Result<String, Box<dyn std::error::Error>> {
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
    let json: serde_json::Value =
        serde_json::from_str(output.stdout_str().trim()).unwrap_or(serde_json::Value::Null);
    if json["code"].as_u64().unwrap_or(999) != 0 {
        return Err(format!(
            "instantiate {label} rejected: {}",
            json["raw_log"].as_str().unwrap_or("unknown")
        )
        .into());
    }
    wait_until_tx(chain, json["txhash"].as_str().unwrap_or("")).await?;
    let q = chain
        .chain_exec(&[
            "query",
            "wasm",
            "list-contract-by-code",
            code_id,
            "--output",
            "json",
        ])
        .await?;
    let qj: serde_json::Value = serde_json::from_str(q.stdout_str().trim())?;
    let addr = qj["contracts"]
        .as_array()
        .and_then(|a| a.last())
        .and_then(|v| v.as_str())
        .ok_or("no contract addr")?
        .to_string();
    println!("  {label}: {addr}");
    Ok(addr)
}

async fn find_ibc_channel(
    chain: &dyn Chain,
    port_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let out = chain
        .chain_exec(&["query", "ibc", "channel", "channels", "--output", "json"])
        .await?;
    let j: serde_json::Value = serde_json::from_str(out.stdout_str().trim())?;
    for c in j["channels"].as_array().ok_or("no channels")? {
        if c["port_id"].as_str() == Some(port_id) {
            if let Some(id) = c["channel_id"].as_str() {
                if !id.is_empty() {
                    return Ok(id.to_string());
                }
            }
        }
    }
    Err(format!("no channel on port {port_id}").into())
}

async fn run_test(ic: &mut Interchain) -> Result<(), Box<dyn std::error::Error>> {
    let terp_core = resolve_terp_core()?;
    let any_wasm = resolve_any_proxy_wasm()?;
    let proof_path = terp_core.join(NORICK_PROOF);
    let proof_data = std::fs::read_to_string(&proof_path).map_err(|_| {
        format!("missing {proof_path:?} — real Halo2 fixture required (no skip / no ADD_)")
    })?;
    let proof_json: serde_json::Value = serde_json::from_str(&proof_data)?;
    let rick_proof = proof_json["rick"]["proof"].as_str().unwrap_or("");
    if rick_proof.is_empty() || rick_proof.starts_with("ADD_") {
        return Err("no-rick proof fixture empty or placeholder".into());
    }

    let chain_a = ic.get_chain("chain-a").unwrap();
    let chain_b = ic.get_chain("chain-b").unwrap();

    for (chain, key) in [(chain_a, "user-a"), (chain_b, "user-b")] {
        chain.create_key(key).await?;
        let user = key_address(chain, key).await?;
        chain
            .send_funds(
                "validator",
                &WalletAmount {
                    address: user.clone(),
                    denom: "uterp".into(),
                    amount: 10_000_000_000,
                },
            )
            .await?;
        wait_for_blocks(chain, 2).await?;
    }

    println!("--- Store polytone on A and B ---");
    let note_p = copy_wasm_to_chain(chain_a, &terp_core.join(NOTE_WASM), "polytone_note.wasm").await?;
    let voice_p = copy_wasm_to_chain(chain_b, &terp_core.join(VOICE_WASM), "polytone_voice.wasm").await?;
    let proxy_p = copy_wasm_to_chain(chain_b, &terp_core.join(PROXY_WASM), "polytone_proxy.wasm").await?;
    let tester_p =
        copy_wasm_to_chain(chain_a, &terp_core.join(TESTER_WASM), "polytone_tester.wasm").await?;
    let any_p = copy_wasm_to_chain(chain_b, &any_wasm, "polytone_any_proxy.wasm").await?;
    let norick_wasm = copy_wasm_to_chain(chain_b, &terp_core.join(NORICK_WASM), "zk_no_rick.wasm").await?;
    let norick_vk = copy_wasm_to_chain(chain_b, &terp_core.join(NORICK_VK), "no_rick.bin").await?;

    let note_code = store_one(chain_a, "user-a", "note", &note_p).await?;
    let tester_code = store_one(chain_a, "user-a", "tester", &tester_p).await?;
    let voice_code = store_one(chain_b, "user-b", "voice", &voice_p).await?;
    let proxy_code = store_one(chain_b, "user-b", "proxy", &proxy_p).await?;
    let any_code = store_one(chain_b, "user-b", "any-proxy", &any_p).await?;

    println!("--- headstash zk_no_rick on B ---");
    let hs = chain_b
        .chain_exec(&[
            "tx",
            "wasm",
            "headstash",
            &norick_wasm,
            &norick_vk,
            "--from",
            "user-b",
            "--gas-prices",
            "0uterp",
            "--chain-id",
            chain_b.chain_id(),
            "--keyring-backend",
            "test",
            "--gas",
            "auto",
            "--gas-adjustment",
            "1.5",
            "--broadcast-mode",
            "sync",
            "--output",
            "json",
            "-y",
        ])
        .await?;
    let hsj: serde_json::Value =
        serde_json::from_str(hs.stdout_str().trim()).unwrap_or(serde_json::Value::Null);
    if hsj["code"].as_u64().unwrap_or(999) != 0 {
        return Err(format!("headstash: {}", hsj["raw_log"]).into());
    }
    wait_until_tx(chain_b, hsj["txhash"].as_str().unwrap_or("")).await?;
    let nr_list = chain_b
        .chain_exec(&[
            "query",
            "wasm",
            "list-code",
            "--output",
            "json",
        ])
        .await?;
    println!("  list-code: {}", nr_list.stdout_str().trim());

    let norick_inst = instantiate_one(chain_b, "user-b", "1", "{}", "no-rick").await;
    let norick_addr = match norick_inst {
        Ok(a) => a,
        Err(_) => instantiate_one(chain_b, "user-b", "2", "{}", "no-rick").await?,
    };

    let voice_init = format!(
        r#"{{"proxy_code_id":"{proxy_code}","block_max_gas":"100000000","contract_addr_len":32}}"#
    );
    let note_addr = instantiate_one(
        chain_a,
        "user-a",
        &note_code,
        r#"{"block_max_gas":"100000000"}"#,
        "polytone-note",
    )
    .await?;
    let voice_addr =
        instantiate_one(chain_b, "user-b", &voice_code, &voice_init, "polytone-voice").await?;
    let tester_a =
        instantiate_one(chain_a, "user-a", &tester_code, "{}", "polytone-tester").await?;
    let any_addr = instantiate_one(
        chain_b,
        "user-b",
        &any_code,
        r#"{"unrestricted":true}"#,
        "polytone-any-proxy",
    )
    .await?;

    println!("--- polytone IBC channel ---");
    let src_port = format!("wasm.{note_addr}");
    let dst_port = format!("wasm.{voice_addr}");
    let relayer = ic.get_relayer("hermes").unwrap();
    relayer
        .create_channel(
            "polytone-path",
            &ChannelOptions {
                src_port: src_port.clone(),
                dst_port: dst_port.clone(),
                ordering: ict_rs::ibc::ChannelOrdering::Unordered,
                version: "polytone-1".into(),
            },
        )
        .await?;
    let poly_channel_id = find_ibc_channel(chain_a, &src_port).await?;
    ic.start_relayers().await?;
    wait_for_blocks(chain_a, 1).await?;

    let prove_json = format!(
        r#"{{"proove":{{"cid":1,"forbidden":"rick","proof":"{rick_proof}"}}}}"#
    );
    // ICA sender is unknown until first packet; Any.sender is the any-proxy
    // contract — wasmd overwrites sender on MsgExecuteContract from CosmWasm Any.
    let proto = encode_msg_execute_contract(&any_addr, &norick_addr, prove_json.as_bytes());
    let any_value = base64_encode(&proto);
    let dispatch = serde_json::json!({
        "dispatch": {
            "msgs": [{
                "type_url": "/cosmwasm.wasm.v1.MsgExecuteContract",
                "value": any_value
            }]
        }
    });
    let inner = base64_encode(dispatch.to_string().as_bytes());
    let callback_msg = base64_encode(b"norick-any\n");
    let execute_msg = serde_json::json!({
        "execute": {
            "msgs": [{
                "wasm": {
                    "execute": {
                        "contract_addr": any_addr,
                        "msg": inner,
                        "funds": []
                    }
                }
            }],
            "timeout_seconds": "600",
            "callback": {
                "receiver": tester_a,
                "msg": callback_msg
            }
        }
    });

    println!("--- note execute Any→no-rick ---");
    let exec = chain_a
        .chain_exec(&[
            "tx",
            "wasm",
            "execute",
            &note_addr,
            &execute_msg.to_string(),
            "--from",
            "user-a",
            "--gas-prices",
            "0uterp",
            "--chain-id",
            chain_a.chain_id(),
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
    let ej: serde_json::Value =
        serde_json::from_str(exec.stdout_str().trim()).unwrap_or(serde_json::Value::Null);
    let txhash = ej["txhash"].as_str().unwrap_or("");
    wait_until_tx(chain_a, txhash).await?;
    relayer.flush("polytone-path", &poly_channel_id).await?;

    let mut ok = false;
    for attempt in 1..=15 {
        relayer.flush("polytone-path", &poly_channel_id).await?;
        wait_for_blocks(chain_a, 2).await?;
        wait_for_blocks(chain_b, 2).await?;
        let cb = chain_a
            .chain_exec(&[
                "query",
                "wasm",
                "contract-state",
                "smart",
                &tester_a,
                r#"{"history":{}}"#,
                "--output",
                "json",
            ])
            .await?;
        let hist: serde_json::Value = serde_json::from_str(cb.stdout_str().trim())?;
        println!("  attempt {attempt} callback={hist}");
        if let Some(h) = hist.pointer("/data/history").and_then(|v| v.as_array()) {
            if let Some(last) = h.last() {
                if last
                    .pointer("/result/error")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
                {
                    return Err(format!("callback error: {last}").into());
                }
                if !h.is_empty() {
                    ok = true;
                    break;
                }
            }
        }
    }
    if !ok {
        return Err("no polytone callback — Any/no-rick packet did not complete".into());
    }
    println!("polytone Any → no-rick proof path returned callback (success ack)");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    println!("=== Polytone CosmosMsg::Any × no-rick (Docker) ===\n");

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let test_name = "polytone-any-norick";
    let network_id = format!("ict-{test_name}");
    runtime.create_network(&network_id).await?;

    let chain_a = CosmosChain::new(terp_chain_config("chain-a"), 1, 0, runtime.clone());
    let chain_b = CosmosChain::new(terp_chain_config("chain-b"), 1, 0, runtime.clone());
    let relayer = build_relayer(RelayerType::Hermes, runtime.clone(), test_name, &network_id).await?;

    let mut ic = Interchain::new(runtime)
        .add_chain(Box::new(chain_a))
        .add_chain(Box::new(chain_b))
        .add_relayer("hermes", relayer)
        .add_link(InterchainLink {
            chain1: "chain-a".into(),
            chain2: "chain-b".into(),
            relayer: "hermes".into(),
            path: "polytone-path".into(),
        });

    ic.start_chains(&InterchainBuildOptions {
        test_name: test_name.into(),
        ..Default::default()
    })
    .await?;

    let result = run_test(&mut ic).await;
    if let Err(e) = ic.stop().await {
        eprintln!("cleanup: {e}");
    }
    match result {
        Ok(()) => {
            println!("PASSED");
            Ok(())
        }
        Err(e) => {
            eprintln!("FAILED: {e}");
            Err(e)
        }
    }
}
