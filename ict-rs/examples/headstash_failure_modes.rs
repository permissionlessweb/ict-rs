//! Real 4-validator terpd chain via ict-rs Docker.
//! Host ports are published on 127.0.0.1. Sidecars listen on 127.0.0.1.
//! The custody seed is process memory only and is not printed.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use ed25519_dalek::{Signer, SigningKey};
use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm, TestContext};
use ict_rs::error::IctError;
use ict_rs::prelude::IctResult;
use ict_rs::runtime::{DockerConfig, DockerImage, IctRuntime};
use sha2::{Digest, Sha256};

const N: usize = 4;
const UID: &str = "headstash-claim";
const ALGO: &str = "headstash-accepted";
const SOURCE: &str = "headstash-batch";
const SCOPE: &str = "headstash";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Honest,
    Wrong,
    Down,
}

fn seed() -> &'static [u8; 32] {
    static SEED: OnceLock<[u8; 32]> = OnceLock::new();
    SEED.get_or_init(|| {
        let mut buf = [0u8; 32];
        let mut f = std::fs::File::open("/dev/urandom").expect("urandom");
        f.read_exact(&mut buf).expect("urandom read");
        buf
    })
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(seed())
}

fn b64(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | bytes[i + 2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    if i < bytes.len() {
        let b0 = bytes[i] as u32;
        let b1 = if i + 1 < bytes.len() { bytes[i + 1] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if i + 1 < bytes.len() {
            out.push(T[((n >> 6) & 63) as usize] as char);
            out.push('=');
        } else {
            out.push('=');
            out.push('=');
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    const H: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 0xf) as usize] as char);
    }
    s
}

fn value_for(mode: Mode) -> [u8; 32] {
    match mode {
        Mode::Wrong => [0x22; 32],
        _ => [0x11; 32],
    }
}

fn custody_payload(value: &[u8]) -> Vec<u8> {
    let domain = Sha256::digest(format!("hashmerchant/custody/{SCOPE}/{SOURCE}").as_bytes());
    let mut payload = Vec::new();
    payload.extend_from_slice(&domain);
    payload.extend_from_slice(value);
    payload.extend_from_slice(&1u64.to_le_bytes());
    payload.extend_from_slice(&1u64.to_le_bytes());
    payload
}

fn aggregate(value: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(SOURCE.as_bytes());
    h.update(value);
    h.update(1u64.to_le_bytes());
    h.finalize().into()
}

fn vote_json(mode: Mode) -> String {
    let value = value_for(mode);
    let root = aggregate(&value);
    let sig = signing_key().sign(&custody_payload(&value));
    format!(
        r#"{{"runtime_id":"headstash","chain_uid":"{UID}","algo":"{ALGO}","root":"{root}","foreign_height":1,"foreign_block_time":1,"attestations":[{{"source_id":"{SOURCE}","value":"{value}","height":1,"timestamp":1,"custody_signature":"{sig}"}}]}}"#,
        root = hex(&root),
        value = hex(&value),
        sig = hex(&sig.to_bytes()),
    )
}

fn honest_root() -> String {
    hex(&aggregate(&value_for(Mode::Honest)))
}

fn serve(port: u16, mode: Arc<Mutex<Mode>>) {
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind sidecar");
    for conn in listener.incoming() {
        let mut conn = match conn {
            Ok(c) => c,
            Err(_) => continue,
        };
        let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buf = [0u8; 2048];
        let _ = conn.read(&mut buf);
        let mode = *mode.lock().unwrap();
        if mode == Mode::Down {
            let body = "down";
            let resp = format!(
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = conn.write_all(resp.as_bytes());
            continue;
        }
        let body = vote_json(mode);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = conn.write_all(resp.as_bytes());
    }
}

fn modify_genesis(_cfg: &ChainConfig, raw: Vec<u8>) -> IctResult<Vec<u8>> {
    let mut genesis: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| IctError::Config(format!("parse genesis: {e}")))?;
    if let Some(abci) = genesis.pointer_mut("/consensus/params/abci") {
        abci["vote_extensions_enable_height"] = serde_json::json!("1");
    }
    let pk = b64(&signing_key().verifying_key().to_bytes());
    if let Some(hm) = genesis.pointer_mut("/app_state/hashmerchant") {
        hm["registered_chains"] = serde_json::json!([{
            "chain_uid": UID,
            "name": "headstash",
            "enabled": true,
            "oracle_sources": [{
                "source_id": SOURCE,
                "enabled": true,
                "authenticator": {
                    "pubkey": pk,
                    "algorithm": "ed25519",
                    "scope": SCOPE
                }
            }]
        }]);
    }
    serde_json::to_vec(&genesis).map_err(|e| IctError::Config(format!("encode: {e}")))
}

fn chain_config() -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "headstash-fm".into(),
        chain_id: "headstash-fm-1".into(),
        images: vec![DockerImage {
            repository: "terp-core".into(),
            version: "local-arm64".into(),
            uid_gid: None,
        }],
        bin: "terpd".into(),
        bech32_prefix: "terp".into(),
        denom: "uterp".into(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0.025uterp".into(),
        gas_adjustment: 1.5,
        trusting_period: "336h".into(),
        block_time: "1s".into(),
        genesis: None,
        modify_genesis: Some(Box::new(modify_genesis)),
        pre_genesis: None,
        config_file_overrides: Default::default(),
        additional_start_args: vec!["--wasm.skip_wasmvm_version_check".into()],
        env: vec![],
        sidecar_configs: vec![],
        faucet: None,
        genesis_style: Default::default(),
    }
}

fn height(rpc: u16) -> Option<u64> {
    let url = format!("http://127.0.0.1:{rpc}/status");
    let body = std::process::Command::new("curl")
        .args(["-fsS", "--max-time", "3", &url])
        .output()
        .ok()?;
    if !body.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&body.stdout).ok()?;
    v["result"]["sync_info"]["latest_block_height"]
        .as_str()
        .and_then(|s| s.parse().ok())
}

fn wait_height(rpc: u16, target: u64, secs: u64) -> Result<u64, String> {
    let start = Instant::now();
    loop {
        if let Some(h) = height(rpc) {
            if h >= target {
                return Ok(h);
            }
        }
        if start.elapsed() > Duration::from_secs(secs) {
            return Err(format!("timeout waiting height {target}"));
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn query_root(node: &ict_rs::node::ChainNode) -> String {
    match futures::executor::block_on(node.exec_cmd(&[
        "query", "hashmerchant", "root", UID, ALGO, "-o", "json",
    ])) {
        Ok(out) => {
            if out.exit_code != 0 {
                format!("exit {}", out.exit_code)
            } else {
                String::from_utf8_lossy(&out.stdout).chars().take(500).collect()
            }
        }
        Err(e) => format!("exec err {e}"),
    }
}

fn count_of(body: &str) -> Option<u64> {
    let key = "\"attestation_count\":";
    let i = body.find(key)?;
    let rest = body[i + key.len()..].trim_start().trim_start_matches(':').trim();
    let rest = rest.trim_start_matches('"');
    let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    n.parse().ok()
}

fn report(name: &str, pass: bool, ms: u128, detail: &str) {
    println!(
        "{} {} {}ms {}",
        if pass { "PASS" } else { "FAIL" },
        name,
        ms,
        detail.chars().take(220).collect::<String>()
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("ICT_PUBLISH_HOST", "127.0.0.1");
    let modes: Vec<Arc<Mutex<Mode>>> = (0..N).map(|_| Arc::new(Mutex::new(Mode::Honest))).collect();
    for i in 0..N {
        let mode = modes[i].clone();
        let port = 18421 + i as u16;
        thread::spawn(move || serve(port, mode));
    }
    let urls = (0..N)
        .map(|i| format!("http://host.docker.internal:{}", 18421 + i))
        .collect::<Vec<_>>()
        .join(",");
    std::env::set_var("HASHMERCHANT_SIDECAR_URLS", &urls);

    let runtime = IctRuntime::Docker(DockerConfig::default()).into_backend().await?;
    let mut chain = CosmosChain::new(chain_config(), N, 0, runtime);
    let ctx = TestContext {
        test_name: "headstash-fm".into(),
        network_id: String::new(),
    };
    let boot = Instant::now();
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    let rpc = chain.validators()[0].host_rpc_port.ok_or("no rpc port")?;
    println!("chain_up rpc=127.0.0.1:{rpc} boot_ms={}", boot.elapsed().as_millis());
    let h = wait_height(rpc, 8, 180)?;
    println!("height {h}");
    let t = Instant::now();
    let base = query_root(&chain.validators()[0]);
    let base_n = count_of(&base).unwrap_or(0);
    report("baseline_four_up", base_n == 4, t.elapsed().as_millis(), &format!("count={base_n}"));

    *modes[3].lock().unwrap() = Mode::Down;
    thread::sleep(Duration::from_secs(10));
    let t = Instant::now();
    let one = query_root(&chain.validators()[0]);
    let one_n = count_of(&one).unwrap_or(0);
    report("one_sidecar_down_still_pays", one_n == 3, t.elapsed().as_millis(), &format!("count={one_n}"));

    *modes[2].lock().unwrap() = Mode::Down;
    let h0 = height(rpc).unwrap_or(0);
    thread::sleep(Duration::from_secs(12));
    let t = Instant::now();
    let two = query_root(&chain.validators()[0]);
    let two_n = count_of(&two).unwrap_or(0);
    let moved = height(rpc).unwrap_or(0) > h0;
    report(
        "two_sidecars_down_no_quorum",
        moved && two_n == 3,
        t.elapsed().as_millis(),
        &format!("count={two_n} height_moved={moved}"),
    );

    for m in &modes {
        *m.lock().unwrap() = Mode::Honest;
    }
    thread::sleep(Duration::from_secs(12));
    let t = Instant::now();
    let rec = query_root(&chain.validators()[0]);
    let rec_n = count_of(&rec).unwrap_or(0);
    report("recovers_next_epoch", rec_n == 4, t.elapsed().as_millis(), &format!("count={rec_n}"));

    *modes[3].lock().unwrap() = Mode::Wrong;
    thread::sleep(Duration::from_secs(12));
    let t = Instant::now();
    let wrong = query_root(&chain.validators()[0]);
    let wrong_n = count_of(&wrong).unwrap_or(0);
    report("wrong_root_outvoted", wrong_n == 3, t.elapsed().as_millis(), &format!("count={wrong_n}"));

    *modes[3].lock().unwrap() = Mode::Honest;
    *modes[1].lock().unwrap() = Mode::Down;
    thread::sleep(Duration::from_secs(6));
    *modes[1].lock().unwrap() = Mode::Honest;
    thread::sleep(Duration::from_secs(15));
    let t = Instant::now();
    let restart = query_root(&chain.validators()[0]);
    let restart_n = count_of(&restart).unwrap_or(0);
    report(
        "sidecar_restart_mid_epoch",
        restart_n == 4,
        t.elapsed().as_millis(),
        &format!("count={restart_n}"),
    );

    report(
        "replayed_nullifier_across_epochs",
        false,
        0,
        "cw-headstash not instantiated; spent nullifier not on this chain",
    );
    report(
        "unclaimed_funds_sweep",
        false,
        0,
        "cw-headstash escrow not instantiated; sweep not on this chain",
    );

    let _ = chain.stop().await;
    Ok(())
}
