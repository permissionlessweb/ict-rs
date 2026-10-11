//! Local Docker exercise of the headstash claim epoch.
//!
//! Three Terp validators each pull `GET /vote-extension` from their own
//! sidecar on the host. Hashmerchant confirms a root only when two of the
//! three agree. A single runtime, a split vote, a lied aggregate, and a
//! pending-lane quorum must not replace the accepted root or pay a note.
//! The agreed batch is paid once, then the public Pedersen path pays a
//! different note. Replay, a mismatched batch, a public-path collision, an
//! unreserved note, and a bad opening are rejected. A flood of mismatched
//! batches, replays, and garbage sidecar bodies must not move the root or
//! pay, and a later two-of-three batch still pays once.
//!
//! ```sh
//! cd crates/headstash
//! cargo build -p cw-headstash --target wasm32-unknown-unknown --release
//! wasm-opt --enable-reference-types -O0 \
//!   target/wasm32-unknown-unknown/release/cw_headstash.wasm \
//!   -o artifacts/cw_headstash.wasm
//! cd ../ict-rs/ict-rs
//! cargo run --example epoch_claim --features epoch-claim
//! ```
//!
//! `HEADSTASH_WASM` overrides the wasm path. `KEEP_CHAIN=1` leaves the
//! container up. This does not broadcast to a public network.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use sha2::Digest;

use base64::Engine;
use cosmwasm_std::Binary;
use cw_headstash::epoch::{
    accepted_root, attested_root, custody_message, pedersen_commit, EpochPayout, PublicOpen,
    ALGO_ACCEPTED, ALGO_PENDING, BATCH_ATTEST_HEIGHT, BATCH_ATTEST_TIMESTAMP, BATCH_SCOPE,
    BATCH_SOURCE_ID, CHAIN_UID,
};
use cw_headstash::msg::{ExecuteMsg, InstantiateMsg, QueryMsg};
use cw_headstash::tokenfactory::{HeadstashTokenObject, TokenStrategy};
use cw_headstash::wavs::generate_test_wavs_proof;
use ed25519_dalek::{Signer, SigningKey};
use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, FaucetConfig, TestContext};
use ict_rs::cosmwasm::CosmWasmExt;
use ict_rs::runtime::{DockerConfig, DockerImage, IctRuntime};
use ict_rs::spec::builtin_chain_config;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

const DENOM: &str = "uterp";
const PAY: u64 = 15;
const VALIDATORS: usize = 3;

#[derive(Clone)]
struct Served {
    body: String,
    /// When non-empty, each vote-extension GET takes the next body.
    /// Used to spray distinct invalid and disagreeing batches.
    rotate: Vec<String>,
    cursor: usize,
}

fn boot_body() -> Served {
    Served {
        body: r#"{"status":"booting"}"#.into(),
        rotate: Vec::new(),
        cursor: 0,
    }
}

fn body_for(served: &mut Served) -> String {
    if served.rotate.is_empty() {
        return served.body.clone();
    }
    let i = served.cursor % served.rotate.len();
    served.cursor += 1;
    served.rotate[i].clone()
}

fn wasm_path() -> PathBuf {
    if let Ok(path) = std::env::var("HEADSTASH_WASM") {
        return PathBuf::from(path);
    }
    let headstash = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../headstash");
    let artifacts = headstash.join("artifacts/cw_headstash.wasm");
    if artifacts.exists() {
        return artifacts;
    }
    headstash.join("target/wasm32-unknown-unknown/release/cw_headstash.wasm")
}

fn keep_chain() -> bool {
    matches!(
        std::env::var("KEEP_CHAIN").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

fn custody_key() -> SigningKey {
    let dig = sha2::Sha256::digest(b"terp-headstash-epoch-ict-custody-v1");
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&dig);
    SigningKey::from_bytes(&seed)
}

fn serve(batch: &[u8; 32], key: &SigningKey, algo: &str, lie: bool) -> Served {
    let aggregate = attested_root(batch);
    let mut advertised = aggregate;
    if lie {
        advertised[0] ^= 0xff;
    }
    let sig = key.sign(&custody_message(batch));
    let body = serde_json::json!({
        "chain_uid": CHAIN_UID,
        "algo": algo,
        "root": hex::encode(advertised),
        "foreign_height": 1,
        "foreign_block_time": BATCH_ATTEST_TIMESTAMP,
        "attestations": [{
            "source_id": BATCH_SOURCE_ID,
            "value": hex::encode(batch),
            "height": BATCH_ATTEST_HEIGHT,
            "timestamp": BATCH_ATTEST_TIMESTAMP,
            "custody_signature": hex::encode(sig.to_bytes()),
        }],
    })
    .to_string();
    Served {
        body,
        rotate: Vec::new(),
        cursor: 0,
    }
}

fn nf(byte: u8) -> Binary {
    Binary::from(vec![byte; 32])
}

fn payout(byte: u8, dest: &str, amount: u64) -> EpochPayout {
    EpochPayout {
        nullifier: nf(byte),
        denom: HeadstashTokenObject::new(DENOM.into()).proof,
        destination: dest.into(),
        amount,
    }
}

fn publish(lanes: &[Arc<Mutex<Served>>], bodies: &[Served]) {
    for (lane, body) in lanes.iter().zip(bodies) {
        *lane.lock().expect("served") = body.clone();
    }
}

fn chain_config(pubkey_b64: String) -> Result<ChainConfig, Box<dyn std::error::Error>> {
    let mut cfg = builtin_chain_config("terp")?;
    let repo = std::env::var("CORRIDOR_ICT_IMAGE").unwrap_or_else(|_| "terpnetwork/terp-core".into());
    let tag = std::env::var("CORRIDOR_ICT_IMAGE_TAG").unwrap_or_else(|_| "local-zk".into());
    cfg.images = vec![DockerImage {
        repository: repo,
        version: tag,
        uid_gid: None,
    }];
    cfg.gas_prices = format!("0.025{DENOM}");
    cfg.block_time = "2s".into();
    cfg.env.clear();
    cfg.config_file_overrides.insert(
        "config/app.toml".into(),
        serde_json::json!({
            "hashmerchant": {
                "sidecar-timeout": "2s"
            }
        }),
    );
    cfg.faucet = Some(FaucetConfig {
        key_name: "deployer".into(),
        port: 5000,
        start_cmd: vec![],
        env: vec![],
        mnemonic: Some(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
                .into(),
        ),
        coins: Some(format!("100000000000{DENOM}")),
    });
    cfg.modify_genesis = Some(Box::new(move |_cfg, genesis_bytes| {
        let mut genesis: Value = serde_json::from_slice(&genesis_bytes)
            .map_err(|e| ict_rs::error::IctError::Config(e.to_string()))?;
        genesis["consensus"]["params"]["abci"]["vote_extensions_enable_height"] =
            serde_json::json!("2");
        genesis["app_state"]["hashmerchant"]["registered_chains"] = serde_json::json!([{
            "chain_uid": CHAIN_UID,
            "name": "Headstash claim",
            "rpc_endpoints": [],
            "hash_algos": [ALGO_ACCEPTED, "headstash-pending"],
            "enabled": true,
            "oracle_sources": [{
                "source_id": BATCH_SOURCE_ID,
                "enabled": true,
                "authenticator": {
                    "pubkey": pubkey_b64,
                    "algorithm": "ed25519",
                    "scope": BATCH_SCOPE
                }
            }]
        }]);
        // 0.667 of three equal stakes truncates to just above two shares, so
        // two agreeing runtimes would never confirm. 0.6 confirms two shares
        // and still ignores one share.
        genesis["app_state"]["hashmerchant"]["params"]["quorum_fraction"] =
            serde_json::json!("0.600000000000000000");
        Ok(serde_json::to_vec_pretty(&genesis)
            .map_err(|e| ict_rs::error::IctError::Config(e.to_string()))?)
    }));
    Ok(cfg)
}

async fn start_sidecar(served: Arc<Mutex<Served>>) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("0.0.0.0:0").await.expect("bind sidecar");
    let port = listener.local_addr().expect("sidecar port").port();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let served = served.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.contains("GET /vote-extension") {
                    body_for(&mut served.lock().expect("served"))
                } else {
                    r#"{"status":"ok"}"#.to_string()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).await.ok();
            });
        }
    });
    (port, handle)
}

fn text_of(out: &ict_rs::tx::ExecOutput) -> String {
    format!("{}\n{}", out.stdout_str(), out.stderr_str())
}

async fn query_tx(chain: &CosmosChain, txhash: &str) -> Result<Value, String> {
    let mut stalled = chain.height().await.unwrap_or(0);
    let mut still = 0u32;
    let mut last = String::new();
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let height = chain.height().await.unwrap_or(0);
        if height == stalled {
            still += 1;
        } else {
            still = 0;
            stalled = height;
        }
        if still >= 15 {
            return Err(format!("chain height stalled at {height} while indexing {txhash}"));
        }
        let queried = chain
            .chain_exec(&["query", "tx", txhash, "--output", "json"])
            .await
            .map_err(|e| e.to_string())?;
        if queried.exit_code != 0 {
            last = text_of(&queried);
            if last.contains("not found") {
                continue;
            }
            return Err(last);
        }
        return serde_json::from_str(queried.stdout_str().trim())
            .map_err(|e| format!("query tx is not json: {e}: {}", queried.stdout_str()));
    }
    Err(format!("tx {txhash} was not indexed ({last})"))
}

async fn submit(chain: &CosmosChain, args: &[&str]) -> Result<(), String> {
    let opts = chain.default_tx_opts().from("validator");
    let out = chain
        .chain_exec_tx_with(args, opts)
        .await
        .map_err(|e| e.to_string())?;
    if out.exit_code != 0 {
        return Err(text_of(&out));
    }
    let broadcast: Value = serde_json::from_str(out.stdout_str().trim())
        .map_err(|e| format!("broadcast is not json: {e}: {}", out.stdout_str()))?;
    if broadcast["code"].as_u64().unwrap_or(0) != 0 {
        return Err(broadcast.to_string());
    }
    let Some(txhash) = broadcast["txhash"].as_str() else {
        return Ok(());
    };
    let txhash = txhash.to_string();
    let tx = query_tx(chain, &txhash).await?;
    if tx["code"].as_u64().unwrap_or(0) != 0 {
        return Err(tx.to_string());
    }
    Ok(())
}

async fn submit_ok(chain: &CosmosChain, args: &[&str], what: &str) -> Result<(), String> {
    submit(chain, args)
        .await
        .map_err(|e| format!("{what} failed: {e}"))
}

async fn submit_err(chain: &CosmosChain, args: &[&str], needle: &str, what: &str) -> Result<(), String> {
    match submit(chain, args).await {
        Ok(()) => Err(format!("{what} was accepted")),
        Err(body) => {
            if body.contains(needle) {
                println!("  {what} rejected ({needle})");
                Ok(())
            } else {
                Err(format!("{what} rejected without {needle}: {body}"))
            }
        }
    }
}

fn attr<'a>(tx: &'a Value, event: &str, key: &str) -> Option<&'a str> {
    let events = tx["events"]
        .as_array()
        .or_else(|| tx["tx_response"]["events"].as_array())?;
    for ev in events {
        if ev["type"].as_str() != Some(event) {
            continue;
        }
        for a in ev["attributes"].as_array()? {
            if a["key"].as_str() == Some(key) {
                return a["value"].as_str();
            }
        }
    }
    None
}

async fn tx_json(chain: &CosmosChain, args: &[&str]) -> Result<Value, String> {
    let opts = chain.default_tx_opts().from("validator");
    let out = chain
        .chain_exec_tx_with(args, opts)
        .await
        .map_err(|e| e.to_string())?;
    if out.exit_code != 0 {
        return Err(text_of(&out));
    }
    let broadcast: Value = serde_json::from_str(out.stdout_str().trim())
        .map_err(|e| format!("{e}: {}", out.stdout_str()))?;
    if broadcast["code"].as_u64().unwrap_or(0) != 0 {
        return Err(broadcast.to_string());
    }
    let txhash = broadcast["txhash"]
        .as_str()
        .ok_or_else(|| format!("no txhash: {broadcast}"))?
        .to_string();
    query_tx(chain, &txhash).await
}

async fn balance(chain: &CosmosChain, addr: &str) -> Result<u128, String> {
    let out = chain
        .chain_exec(&["query", "bank", "balances", addr, "--output", "json"])
        .await
        .map_err(|e| e.to_string())?;
    if out.exit_code != 0 {
        return Err(text_of(&out));
    }
    let v: Value = serde_json::from_str(out.stdout_str().trim())
        .map_err(|e| format!("{e}: {}", out.stdout_str()))?;
    let coins = v["balances"].as_array().or_else(|| v.as_array());
    let amount = coins
        .and_then(|rows| {
            rows.iter().find_map(|c| {
                (c["denom"].as_str() == Some(DENOM)).then(|| c["amount"].as_str().unwrap_or("0"))
            })
        })
        .unwrap_or("0");
    amount.parse().map_err(|e| format!("amount {amount}: {e}"))
}

async fn epoch_root(chain: &CosmosChain, contract: &str, algo: &str) -> Result<Vec<u8>, String> {
    let query = serde_json::to_string(&QueryMsg::EpochRoot { algo: algo.into() })
        .map_err(|e| e.to_string())?;
    let v = chain
        .query_contract(contract, &query)
        .await
        .map_err(|e| e.to_string())?;
    let root = v
        .get("root")
        .or_else(|| v.get("data").and_then(|d| d.get("root")))
        .and_then(|r| r.as_str())
        .ok_or_else(|| format!("epoch root missing: {v}"))?;
    base64::engine::general_purpose::STANDARD
        .decode(root)
        .map_err(|e| format!("root base64: {e} ({root})"))
}

async fn wait_root(chain: &CosmosChain, contract: &str, expect: &[u8; 32]) -> Result<(), String> {
    let mut last = String::new();
    let mut stalled = chain.height().await.unwrap_or(0);
    let mut still = 0u32;
    for _ in 0..45 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let height = chain.height().await.unwrap_or(0);
        if height == stalled {
            still += 1;
        } else {
            still = 0;
            stalled = height;
        }
        if still >= 8 {
            return Err(format!("chain height stalled at {height}"));
        }
        match epoch_root(chain, contract, ALGO_ACCEPTED).await {
            Ok(got) if got.as_slice() == expect => {
                println!("  accepted aggregate stored at height {height}");
                return Ok(());
            }
            Ok(got) => last = hex::encode(got),
            Err(e) => last = e,
        }
        println!("  height {height} waiting for aggregate ({last})");
    }
    Err(format!("accepted aggregate did not land ({last})"))
}

async fn wait_algo(
    chain: &CosmosChain,
    contract: &str,
    algo: &str,
    expect: &[u8; 32],
) -> Result<(), String> {
    let mut last = String::new();
    let mut stalled = chain.height().await.unwrap_or(0);
    let mut still = 0u32;
    for _ in 0..45 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let height = chain.height().await.unwrap_or(0);
        if height == stalled {
            still += 1;
        } else {
            still = 0;
            stalled = height;
        }
        if still >= 8 {
            return Err(format!("chain height stalled at {height}"));
        }
        match epoch_root(chain, contract, algo).await {
            Ok(got) if got.as_slice() == expect => {
                println!("  {algo} aggregate stored at height {height}");
                return Ok(());
            }
            Ok(got) => last = hex::encode(got),
            Err(e) => last = e,
        }
    }
    Err(format!("{algo} aggregate did not land ({last})"))
}

/// The accepted root must stay put while height keeps moving.
async fn assert_root_holds(
    chain: &CosmosChain,
    contract: &str,
    expect: &[u8],
    blocks: u64,
) -> Result<(), String> {
    let start = chain.height().await.map_err(|e| e.to_string())?;
    let mut height = start;
    let mut idle = 0u32;
    while height < start.saturating_add(blocks) {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let now = chain.height().await.unwrap_or(height);
        if now == height {
            idle += 1;
        } else {
            idle = 0;
            height = now;
        }
        if idle >= 8 {
            return Err(format!("chain height stalled at {height}"));
        }
        match epoch_root(chain, contract, ALGO_ACCEPTED).await {
            Ok(got) if got.as_slice() == expect => {}
            Ok(got) => {
                return Err(format!(
                    "accepted root moved to {} without a quorum",
                    hex::encode(got)
                ));
            }
            Err(e) => return Err(e),
        }
    }
    println!("  accepted root held for {blocks} blocks through height {height}");
    Ok(())
}

/// Each sidecar sprays bodies no other sidecar will sign the same way:
/// garbage JSON and a lied aggregate become empty vote extensions, and the
/// honest batches use nullifiers that belong to this sidecar only.
fn flood_lane(key: &SigningKey, dest: &str, sidecar: u8) -> Served {
    let mut rotate = vec![
        format!(r#"{{"status":"flood","sidecar":{sidecar}}}"#),
        boot_body().body,
    ];
    for n in 0..4u8 {
        let row = payout(80 + sidecar * 8 + n, dest, PAY);
        let batch = accepted_root(&[0u8; 32], &[row]).expect("flood batch");
        rotate.push(serve(&batch, key, ALGO_ACCEPTED, false).body);
        rotate.push(serve(&batch, key, ALGO_ACCEPTED, true).body);
    }
    Served {
        body: boot_body().body,
        rotate,
        cursor: 0,
    }
}

/// Many user batches while the three sidecars disagree. Nothing in `junk`
/// may pay, and the accepted root must stay put as height advances.
async fn reject_flood(
    chain: &CosmosChain,
    contract: &str,
    dest: &str,
    held: &[u8],
    junk: Vec<(ExecuteMsg, &'static str, &'static str)>,
) -> Result<(), String> {
    let start_height = chain.height().await.map_err(|e| e.to_string())?;
    let start_balance = balance(chain, dest).await?;
    let started = Instant::now();
    for (msg, needle, what) in &junk {
        exec_err(chain, contract, msg, needle, what).await?;
        match epoch_root(chain, contract, ALGO_ACCEPTED).await {
            Ok(got) if got.as_slice() == held => {}
            Ok(got) => {
                return Err(format!(
                    "flood moved the accepted root to {}",
                    hex::encode(got)
                ));
            }
            Err(e) => return Err(e),
        }
    }
    let end_height = chain.height().await.map_err(|e| e.to_string())?;
    let end_balance = balance(chain, dest).await?;
    if end_balance != start_balance {
        return Err(format!(
            "flood paid the destination {start_balance} -> {end_balance}"
        ));
    }
    if end_height <= start_height {
        return Err(format!(
            "chain height did not advance during the flood ({start_height})"
        ));
    }
    println!(
        "  flood rejected {} batches in {:.1}s, height {start_height} -> {end_height}, balance unchanged",
        junk.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn exec_args(contract: &str, msg: &ExecuteMsg) -> Result<Vec<String>, String> {
    let json = serde_json::to_string(msg).map_err(|e| e.to_string())?;
    Ok(vec![
        "tx".into(),
        "wasm".into(),
        "execute".into(),
        contract.into(),
        json,
    ])
}

async fn exec_ok(chain: &CosmosChain, contract: &str, msg: &ExecuteMsg, what: &str) -> Result<(), String> {
    let owned = exec_args(contract, msg)?;
    let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    submit_ok(chain, &refs, what).await
}

async fn exec_err(
    chain: &CosmosChain,
    contract: &str,
    msg: &ExecuteMsg,
    needle: &str,
    what: &str,
) -> Result<(), String> {
    let owned = exec_args(contract, msg)?;
    let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    submit_err(chain, &refs, needle, what).await
}

fn main() {
    if let Err(e) = run() {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
        )
        .try_init();
    let wasm = wasm_path();
    println!("== epoch_claim_ict ==");
    println!("  wasm={}", wasm.display());
    let bytes = std::fs::read(&wasm).map_err(|e| {
        format!(
            "wasm missing at {} ({e}). Build it with:\n  cd crates/headstash && cargo build -p cw-headstash --target wasm32-unknown-unknown --release",
            wasm.display()
        )
    })?;
    if !bytes.windows(b"apply_accepted_batch".len()).any(|w| w == b"apply_accepted_batch") {
        return Err("wasm has no apply_accepted_batch export surface; rebuild cw-headstash".into());
    }

    let key = custody_key();
    let pubkey = key.verifying_key().to_bytes();
    let pubkey_b64 = base64::engine::general_purpose::STANDARD.encode(pubkey);

    let rt = Runtime::new()?;
    let mut lanes = Vec::with_capacity(VALIDATORS);
    let mut sidecars = Vec::with_capacity(VALIDATORS);
    let mut ports = Vec::with_capacity(VALIDATORS);
    for i in 0..VALIDATORS {
        let served = Arc::new(Mutex::new(boot_body()));
        let (port, sidecar) = rt.block_on(start_sidecar(served.clone()));
        println!("  sidecar {i}=http://127.0.0.1:{port}/vote-extension");
        lanes.push(served);
        sidecars.push(sidecar);
        ports.push(port);
    }

    let outcome = rt.block_on(exercise(ports, pubkey_b64, lanes, wasm));
    for sidecar in sidecars {
        sidecar.abort();
    }
    outcome
}

async fn exercise(
    ports: Vec<u16>,
    pubkey_b64: String,
    lanes: Vec<Arc<Mutex<Served>>>,
    wasm: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await
        .map_err(|e| format!("Docker runtime: {e}"))?;
    let cfg = chain_config(pubkey_b64)?;
    println!(
        "  image={}:{} validators={VALIDATORS}",
        cfg.images[0].repository, cfg.images[0].version
    );
    let mut chain = CosmosChain::new(cfg, VALIDATORS, 0, runtime);
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let ctx = TestContext {
        test_name: format!("epoch-claim-ict-{n}"),
        network_id: String::new(),
    };
    let result = async {
        chain.initialize(&ctx).await?;
        {
            let nodes = chain.validators_mut();
            if nodes.len() != VALIDATORS {
                return Err(format!("expected {VALIDATORS} validators, got {}", nodes.len()).into());
            }
            for (node, port) in nodes.iter_mut().zip(ports.iter()) {
                node.extra_env
                    .retain(|(key, _)| key != "HASHMERCHANT_SIDECAR_URL");
                node.extra_env.push((
                    "HASHMERCHANT_SIDECAR_URL".into(),
                    format!("http://host.docker.internal:{port}"),
                ));
            }
        }
        chain.start(&[]).await?;
        println!("  chain height {}", chain.height().await?);
        run_claims(&chain, &lanes, &wasm).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    if keep_chain() {
        println!("  KEEP_CHAIN=1, container left running");
    } else if let Err(e) = chain.stop().await {
        eprintln!("  chain stop: {e}");
    }
    result
}

async fn run_claims(
    chain: &CosmosChain,
    lanes: &[Arc<Mutex<Served>>],
    wasm: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let deployer = chain
        .chain_exec(&["keys", "show", "deployer", "-a", "--keyring-backend", "test"])
        .await?;
    if deployer.exit_code != 0 {
        return Err(text_of(&deployer).into());
    }
    let dest = deployer.stdout_str().trim().to_string();
    println!("  destination={dest}");

    let node = chain.primary_node()?;
    node.copy_file_from_host(wasm, "/tmp/cw_headstash.wasm")
        .await?;
    let stored = tx_json(
        chain,
        &["tx", "wasm", "store", "/tmp/cw_headstash.wasm"],
    )
    .await?;
    if stored["code"].as_u64().unwrap_or(0) != 0 {
        return Err(format!("wasm store failed: {stored}").into());
    }
    let code_id = attr(&stored, "store_code", "code_id")
        .ok_or("store_code event has no code_id")?
        .to_string();
    println!("  code_id={code_id}");

    let init = InstantiateMsg {
        genesis_root: Binary::from(vec![0u8; 32]),
        distro_hash_domain: Default::default(),
        genesis_label: Some("epoch".into()),
        token_strategy: TokenStrategy::ExistingFungible(HeadstashTokenObject::new(DENOM.into())),
        wavs: generate_test_wavs_proof(1),
        circuit_id: None,
    };
    let init_json = serde_json::to_string(&init)?;
    let opts = chain
        .default_tx_opts()
        .from("validator")
        .flag("--label", "headstash-epoch")
        .flag("--no-admin", "")
        .flag("--amount", &format!("1000000{DENOM}"));
    let out = chain
        .chain_exec_tx_with(
            &["tx", "wasm", "instantiate", &code_id, &init_json],
            opts,
        )
        .await?;
    if out.exit_code != 0 {
        return Err(text_of(&out).into());
    }
    let broadcast: Value = serde_json::from_str(out.stdout_str().trim())?;
    if broadcast["code"].as_u64().unwrap_or(0) != 0 {
        return Err(format!("instantiate rejected: {broadcast}").into());
    }
    let txhash = broadcast["txhash"].as_str().unwrap_or("").to_string();
    let init_tx = query_tx(chain, &txhash).await?;
    if init_tx["code"].as_u64().unwrap_or(0) != 0 {
        return Err(format!("instantiate failed: {init_tx}").into());
    }
    let contract = attr(&init_tx, "instantiate", "_contract_address")
        .ok_or("instantiate event has no contract address")?
        .to_string();
    println!("  contract={contract}");

    submit_ok(
        chain,
        &[
            "tx",
            "hashmerchant",
            "register-contract",
            &contract,
            CHAIN_UID,
            "bank",
            &format!("1000000{DENOM}"),
        ],
        "register-contract",
    )
    .await?;
    println!("  contract registered with escrow");

    let key = custody_key();
    let row = payout(1, &dest, PAY);
    let batch = accepted_root(&[0u8; 32], &[row.clone()]).map_err(|e| e.to_string())?;
    let aggregate = attested_root(&batch);
    let agreed = serve(&batch, &key, ALGO_ACCEPTED, false);
    publish(lanes, &[agreed.clone(), agreed.clone(), agreed.clone()]);
    println!(
        "  three runtimes serving accepted aggregate {}",
        hex::encode(aggregate)
    );
    wait_root(chain, &contract, &aggregate).await?;

    let dissent_row = payout(9, &dest, PAY);
    let dissent_batch =
        accepted_root(&[0u8; 32], &[dissent_row.clone()]).map_err(|e| e.to_string())?;
    let dissent = serve(&dissent_batch, &key, ALGO_ACCEPTED, false);
    publish(lanes, &[agreed.clone(), agreed.clone(), dissent]);
    println!("  one runtime dissenting; accepted root must hold");
    assert_root_holds(chain, &contract, &aggregate, 4).await?;
    exec_err(
        chain,
        &contract,
        &ExecuteMsg::ApplyAcceptedBatch {
            allocation_root: Binary::from(vec![0u8; 32]),
            root_id: 0,
            rows: vec![dissent_row],
        },
        "does not match",
        "minority batch",
    )
    .await?;

    let before = balance(chain, &dest).await?;
    exec_err(
        chain,
        &contract,
        &ExecuteMsg::ApplyAcceptedBatch {
            allocation_root: Binary::from(vec![0u8; 32]),
            root_id: 0,
            rows: vec![payout(2, &dest, PAY)],
        },
        "does not match",
        "mismatched batch",
    )
    .await?;
    exec_ok(
        chain,
        &contract,
        &ExecuteMsg::ApplyAcceptedBatch {
            allocation_root: Binary::from(vec![0u8; 32]),
            root_id: 0,
            rows: vec![row],
        },
        "accepted batch",
    )
    .await?;
    let after = balance(chain, &dest).await?;
    if after != before + PAY as u128 {
        return Err(format!("destination balance {before} -> {after}, want +{PAY}").into());
    }
    println!("  paid {PAY}{DENOM} once");
    exec_err(
        chain,
        &contract,
        &ExecuteMsg::ApplyAcceptedBatch {
            allocation_root: Binary::from(vec![0u8; 32]),
            root_id: 0,
            rows: vec![payout(1, &dest, PAY)],
        },
        "nullifier already exists",
        "replay",
    )
    .await?;

    let lone_row = payout(7, &dest, PAY);
    let lone_batch = accepted_root(&[0u8; 32], &[lone_row.clone()]).map_err(|e| e.to_string())?;
    let lone = serve(&lone_batch, &key, ALGO_ACCEPTED, false);
    let lie = serve(&lone_batch, &key, ALGO_ACCEPTED, true);
    publish(lanes, &[lie.clone(), lie, lone.clone()]);
    println!("  two runtimes lying about the aggregate, one publishing alone");
    assert_root_holds(chain, &contract, &aggregate, 4).await?;
    publish(lanes, &[lone, boot_body(), boot_body()]);
    println!("  one runtime publishing, two dark");
    assert_root_holds(chain, &contract, &aggregate, 4).await?;
    exec_err(
        chain,
        &contract,
        &ExecuteMsg::ApplyAcceptedBatch {
            allocation_root: Binary::from(vec![0u8; 32]),
            root_id: 0,
            rows: vec![lone_row],
        },
        "does not match",
        "single-runtime batch",
    )
    .await?;

    let split: Vec<Served> = [11u8, 12, 13]
        .into_iter()
        .map(|byte| {
            let row = payout(byte, &dest, PAY);
            let batch = accepted_root(&[0u8; 32], &[row]).expect("split batch");
            serve(&batch, &key, ALGO_ACCEPTED, false)
        })
        .collect();
    publish(lanes, &split);
    println!("  three runtimes publishing three batches");
    assert_root_holds(chain, &contract, &aggregate, 4).await?;

    let pending_row = payout(14, &dest, PAY);
    let pending_batch =
        accepted_root(&[0u8; 32], &[pending_row.clone()]).map_err(|e| e.to_string())?;
    let pending_aggregate = attested_root(&pending_batch);
    let pending = serve(&pending_batch, &key, ALGO_PENDING, false);
    publish(lanes, &[pending.clone(), pending.clone(), pending]);
    println!(
        "  three runtimes serving pending aggregate {}",
        hex::encode(pending_aggregate)
    );
    wait_algo(chain, &contract, ALGO_PENDING, &pending_aggregate).await?;
    let still = epoch_root(chain, &contract, ALGO_ACCEPTED).await?;
    if still.as_slice() != aggregate {
        return Err(format!(
            "pending lane replaced the accepted root with {}",
            hex::encode(still)
        )
        .into());
    }
    println!("  pending lane left the accepted root in place");
    exec_err(
        chain,
        &contract,
        &ExecuteMsg::ApplyAcceptedBatch {
            allocation_root: Binary::from(vec![0u8; 32]),
            root_id: 0,
            rows: vec![pending_row],
        },
        "does not match",
        "pending batch",
    )
    .await?;

    exec_ok(
        chain,
        &contract,
        &ExecuteMsg::SetDenomTable {
            amounts: vec![10, 20],
        },
        "denom table",
    )
    .await?;
    exec_ok(
        chain,
        &contract,
        &ExecuteMsg::AssignPublicNotes {
            nullifiers: vec![nf(4)],
        },
        "assign public note",
    )
    .await?;
    let public_row = payout(4, &dest, 10);
    let public_batch = accepted_root(&[0u8; 32], &[public_row.clone()]).map_err(|e| e.to_string())?;
    let public_aggregate = attested_root(&public_batch);
    let public_feed = serve(&public_batch, &key, ALGO_ACCEPTED, false);
    // Two agreeing runtimes are enough. The third stays dark.
    publish(lanes, &[public_feed.clone(), public_feed, boot_body()]);
    println!(
        "  serving public-note aggregate {}",
        hex::encode(public_aggregate)
    );
    wait_root(chain, &contract, &public_aggregate).await?;
    exec_err(
        chain,
        &contract,
        &ExecuteMsg::ApplyAcceptedBatch {
            allocation_root: Binary::from(vec![0u8; 32]),
            root_id: 0,
            rows: vec![public_row.clone()],
        },
        "public path",
        "public-path collision",
    )
    .await?;

    let blind = [8u8; 32];
    let commitment = pedersen_commit(&dest, &blind).map_err(|e| e.to_string())?;
    let open = PublicOpen {
        index: 1,
        destination: dest.clone(),
        blind: Binary::from(blind.to_vec()),
        commitment: Binary::from(commitment.to_vec()),
        tx_hash: "0xabc".into(),
        log_index: 2,
        nullifier: nf(4),
    };
    let mid = balance(chain, &dest).await?;
    exec_ok(
        chain,
        &contract,
        &ExecuteMsg::ApplyPublicBatch {
            rows: vec![open.clone()],
        },
        "public batch",
    )
    .await?;
    let paid_public = balance(chain, &dest).await?;
    if paid_public != mid + 20 {
        return Err(format!("public pay {mid} -> {paid_public}, want +20").into());
    }
    println!("  public index 1 paid 20{DENOM}");
    exec_err(
        chain,
        &contract,
        &ExecuteMsg::ApplyPublicBatch { rows: vec![open] },
        "already",
        "ethereum log replay",
    )
    .await?;
    exec_err(
        chain,
        &contract,
        &ExecuteMsg::ApplyPublicBatch {
            rows: vec![PublicOpen {
                index: 0,
                destination: dest.clone(),
                blind: Binary::from(blind.to_vec()),
                commitment: Binary::from(
                    pedersen_commit(&dest, &blind)
                        .map_err(|e| e.to_string())?
                        .to_vec(),
                ),
                tx_hash: "0xdef".into(),
                log_index: 0,
                nullifier: nf(5),
            }],
        },
        "not on the public path",
        "unreserved note",
    )
    .await?;
    let mut bad = commitment;
    bad[0] ^= 0xff;
    exec_err(
        chain,
        &contract,
        &ExecuteMsg::ApplyPublicBatch {
            rows: vec![PublicOpen {
                index: 0,
                destination: dest.clone(),
                blind: Binary::from(blind.to_vec()),
                commitment: Binary::from(bad.to_vec()),
                tx_hash: "0xbad".into(),
                log_index: 1,
                nullifier: nf(4),
            }],
        },
        "pedersen",
        "bad opening",
    )
    .await?;

    publish(
        lanes,
        &[
            flood_lane(&key, &dest, 0),
            flood_lane(&key, &dest, 1),
            flood_lane(&key, &dest, 2),
        ],
    );
    println!("  sidecars spraying garbage, lies, and disagreeing batches");
    let mut wide = Vec::new();
    for byte in 160..176 {
        wide.push(payout(byte, &dest, PAY));
    }
    let mut junk = vec![
        (
            ExecuteMsg::ApplyAcceptedBatch {
                allocation_root: Binary::from(vec![0u8; 32]),
                root_id: 0,
                rows: wide,
            },
            "does not match",
            "wide mismatched batch",
        ),
        (
            ExecuteMsg::ApplyAcceptedBatch {
                allocation_root: Binary::from(vec![0u8; 32]),
                root_id: 0,
                rows: vec![public_row.clone()],
            },
            "public path",
            "root-matching public note",
        ),
        (
            ExecuteMsg::ApplyPublicBatch { rows: vec![] },
            "empty",
            "empty public batch",
        ),
        (
            ExecuteMsg::ApplyPublicBatch {
                rows: vec![PublicOpen {
                    index: 1,
                    destination: dest.clone(),
                    blind: Binary::from(blind.to_vec()),
                    commitment: Binary::from(commitment.to_vec()),
                    tx_hash: "0xabc".into(),
                    log_index: 2,
                    nullifier: nf(4),
                }],
            },
            "already",
            "flooded ethereum log replay",
        ),
    ];
    for byte in 30..38 {
        junk.push((
            ExecuteMsg::ApplyAcceptedBatch {
                allocation_root: Binary::from(vec![0u8; 32]),
                root_id: 0,
                rows: vec![payout(byte, &dest, PAY)],
            },
            "does not match",
            "mismatched flood batch",
        ));
    }
    reject_flood(chain, &contract, &dest, &public_aggregate, junk).await?;

    let load_row = payout(21, &dest, PAY);
    let load_batch = accepted_root(&[0u8; 32], &[load_row.clone()]).map_err(|e| e.to_string())?;
    let load_aggregate = attested_root(&load_batch);
    let load_feed = serve(&load_batch, &key, ALGO_ACCEPTED, false);
    publish(lanes, &[load_feed.clone(), load_feed, boot_body()]);
    println!(
        "  two runtimes serving post-flood aggregate {}",
        hex::encode(load_aggregate)
    );
    wait_root(chain, &contract, &load_aggregate).await?;
    let before_load = balance(chain, &dest).await?;
    exec_ok(
        chain,
        &contract,
        &ExecuteMsg::ApplyAcceptedBatch {
            allocation_root: Binary::from(vec![0u8; 32]),
            root_id: 0,
            rows: vec![load_row.clone()],
        },
        "post-flood batch",
    )
    .await?;
    let after_load = balance(chain, &dest).await?;
    if after_load != before_load + PAY as u128 {
        return Err(format!(
            "post-flood balance {before_load} -> {after_load}, want +{PAY}"
        )
        .into());
    }
    println!("  post-flood quorum paid {PAY}{DENOM} once");
    exec_err(
        chain,
        &contract,
        &ExecuteMsg::ApplyAcceptedBatch {
            allocation_root: Binary::from(vec![0u8; 32]),
            root_id: 0,
            rows: vec![load_row],
        },
        "nullifier already exists",
        "post-flood replay",
    )
    .await?;
    println!("  claim workflows passed");
    Ok(())
}
