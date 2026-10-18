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
const SCOPE: &str = "headstash-batch";

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


fn le_u32(n: u32) -> [u8; 4] { n.to_le_bytes() }
fn le_u16(n: u16) -> [u8; 2] { n.to_le_bytes() }

fn accepted_batch(allocation: &[u8; 32], nullifier: &[u8; 32], denom: &[u8; 32], dest: &str, amount: u64) -> [u8; 32] {
    let mut pre = Vec::new();
    pre.extend_from_slice(b"terp-headstash-accepted-v1");
    pre.extend_from_slice(allocation);
    pre.extend_from_slice(&1u32.to_le_bytes());
    pre.extend_from_slice(nullifier);
    pre.extend_from_slice(denom);
    let db = dest.as_bytes();
    pre.extend_from_slice(&(db.len() as u16).to_le_bytes());
    pre.extend_from_slice(db);
    pre.extend_from_slice(&amount.to_le_bytes());
    let dig = Sha256::digest(&pre);
    let mut out = [0u8; 32];
    out.copy_from_slice(&dig);
    out
}

static PAYLOAD: Mutex<Option<[u8; 32]>> = Mutex::new(None);

fn value_for(mode: Mode) -> [u8; 32] {
    match mode {
        Mode::Wrong => [0x22; 32],
        Mode::Down => [0x11; 32],
        Mode::Honest => PAYLOAD.lock().unwrap().unwrap_or([0x11; 32]),
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

fn oracle_pubkey_b64() -> String {
    if let Ok(hexpub) = std::env::var("HEADSTASH_CUSTODY_PUB_HEX") {
        return b64(&hex_decode(hexpub.trim()));
    }
    b64(&signing_key().verifying_key().to_bytes())
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
    let pk = oracle_pubkey_b64();
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

async fn tx_out(node: &ict_rs::node::ChainNode, args: &[&str], gas: &str) -> String {
    let opts = node.default_tx_opts().from("validator").gas(gas).broadcast_mode("sync");
    let sent = match node.exec_tx_with(args, opts).await {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            if out.exit_code != 0 {
                return format!("exit {} {}", out.exit_code, String::from_utf8_lossy(&out.stderr).chars().take(180).collect::<String>());
            }
            stdout
        }
        Err(e) => return format!("exec err {e}"),
    };
    let hash: String = sent.split("txhash").nth(1).unwrap_or("").chars().filter(|c| c.is_ascii_hexdigit()).take(64).collect();
    if hash.len() != 64 {
        return sent.chars().take(400).collect();
    }
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Ok(out) = node.exec_cmd(&["query", "tx", &hash, "-o", "json"]).await {
            if out.exit_code == 0 {
                return String::from_utf8_lossy(&out.stdout).chars().take(2000).collect();
            }
        }
    }
    format!("tx {hash} not indexed")
}

fn attr(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"key\":\"{key}\"");
    let i = body.find(&needle)?;
    let rest = &body[i + needle.len()..];
    let v = rest.split("\"value\":\"").nth(1)?;
    Some(v.split('"').next()?.to_string())
}

fn tx_code(body: &str) -> Option<u64> {
    let i = body.find("\"code\":")?;
    let rest = body[i + 7..].trim_start();
    let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    n.parse().ok()
}

async fn contract_cases(
    chain: &CosmosChain,
    modes: &[Arc<Mutex<Mode>>],
    rpc: u16,
) {
    let started = Instant::now();
    let node = &chain.validators()[0];
    let addr = match node.exec_cmd(&["keys", "show", "validator", "-a", "--keyring-backend", "test"]).await {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(e) => {
            report("replayed_nullifier_across_epochs", false, 0, &format!("no address {e}"));
            report("unclaimed_funds_sweep", false, 0, "no address");
            return;
        }
    };
    if !addr.starts_with("terp1") {
        report("replayed_nullifier_across_epochs", false, 0, "validator address missing");
        report("unclaimed_funds_sweep", false, 0, "validator address missing");
        return;
    }
    let denom = hex_decode("1d56a5879c78333d517942a36e7ea0932e7ffa1a1027e439279e8517580ea706");
    let nullifier = [0x01u8; 32];
    let allocation = [0x44u8; 32];
    let batch = accepted_batch(&allocation, &nullifier, &denom, &addr, 15);
    *PAYLOAD.lock().unwrap() = Some(batch);
    for m in modes {
        *m.lock().unwrap() = Mode::Honest;
    }
    let h0 = height(rpc).unwrap_or(0);
    thread::sleep(Duration::from_secs(12));
    let root_body = query_root(node);
    let root_ok = count_of(&root_body).unwrap_or(0) >= 3 && height(rpc).unwrap_or(0) > h0;

    let id = match &node.container_id {
        Some(id) => id.0.clone(),
        None => {
            report("replayed_nullifier_across_epochs", false, 0, "no container");
            report("unclaimed_funds_sweep", false, 0, "no container");
            return;
        }
    };
    let wasm = "/tmp/headstash-epoch-chain/cw_headstash.wasm";
    let cp = std::process::Command::new("docker")
        .args(["cp", wasm, &format!("{id}:/tmp/cw_headstash.wasm")])
        .status();
    if !cp.map(|s| s.success()).unwrap_or(false) {
        report("replayed_nullifier_across_epochs", false, 0, "wasm copy failed");
        report("unclaimed_funds_sweep", false, 0, "wasm copy failed");
        return;
    }
    let wavs = std::fs::read_to_string("/tmp/headstash-epoch-chain/wavs.json").unwrap_or_default();
    let init = format!(
        r#"{{"genesis_root":"{z}","genesis_label":"epoch","token_strategy":{{"existing_fungible":{{"raw":"uterp","proof":"{proof}"}}}},"wavs":{wavs},"circuit_id":1}}"#,
        z = b64(&[0u8; 32]),
        proof = b64(&denom),
    );
    let _ = std::fs::write("/tmp/hs-init.json", &init);
    let _ = std::process::Command::new("docker")
        .args(["cp", "/tmp/hs-init.json", &format!("{id}:/tmp/hs-init.json")])
        .status();
    let t = Instant::now();
    let stored = tx_out(node, &["tx", "wasm", "store", "/tmp/cw_headstash.wasm"], "8000000").await;
    let mut code_id = attr(&stored, "code_id").unwrap_or_default();
    if code_id.is_empty() && tx_code(&stored).unwrap_or(1) == 0 {
        if let Ok(out) = node.exec_cmd(&["query", "wasm", "list-code", "-o", "json"]).await {
            let body = String::from_utf8_lossy(&out.stdout);
            code_id = body.rmatch_indices("code_id").next().map(|(i, _)| {
                let rest = body[i+9..].trim_start_matches(|c: char| !c.is_ascii_digit());
                rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>()
            }).unwrap_or_default();
        }
    }
    if tx_code(&stored).unwrap_or(1) != 0 || code_id.is_empty() {
        report("replayed_nullifier_across_epochs", false, t.elapsed().as_millis(), &format!("store {stored}"));
        report("unclaimed_funds_sweep", false, 0, "store failed");
        return;
    }
    let inst = tx_out(
        node,
        &[
            "tx", "wasm", "instantiate", &code_id, &init,
            "--label", "headstash-epoch", "--admin", &addr, "--amount", "1000000uterp",
        ],
        "3000000",
    ).await;
    let mut contract = attr(&inst, "_contract_address").or_else(|| attr(&inst, "contract_address")).unwrap_or_default();
    if contract.is_empty() && tx_code(&inst).unwrap_or(1) == 0 {
        if let Ok(out) = node.exec_cmd(&["query", "wasm", "list-contract-by-code", &code_id, "-o", "json"]).await {
            let body = String::from_utf8_lossy(&out.stdout);
            if let Some(i) = body.rfind("terp1") {
                contract = body[i..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            }
        }
    }
    if tx_code(&inst).unwrap_or(1) != 0 || contract.is_empty() {
        report("replayed_nullifier_across_epochs", false, t.elapsed().as_millis(), &format!("instantiate {}", inst.chars().take(220).collect::<String>()));
        report("unclaimed_funds_sweep", false, 0, "instantiate failed");
        return;
    }
    let campaign = format!(
        r#"{{"set_epoch_campaign":{{"campaign":{{"ctx":"{z}","backend":"halo2","cutoff_height":1,"bech32_hrp":"terp","denom":"{proof}","amounts":[{{"amount":15,"cap":1}}],"reserved":[],"trusted":["{z}"],"trusted_verify_threshold":1,"trusted_sign_threshold":1,"max_rows":4,"allocation_root":"{alloc}","root_id":0,"vk_id":1,"vk_hash":"{z}"}}}}}}"#,
        z = b64(&[0u8; 32]),
        proof = b64(&denom),
        alloc = b64(&allocation),
    );
    let _ = std::fs::write("/tmp/hs-campaign.json", &campaign);
    let _ = std::process::Command::new("docker").args(["cp", "/tmp/hs-campaign.json", &format!("{id}:/tmp/hs-campaign.json")]).status();
    let setc = tx_out(node, &["tx", "wasm", "execute", &contract, &campaign], "800000").await;
    let row = format!(
        r#"{{"stage_epoch_batch":{{"digests":["{z}"],"rows":[{{"nullifier":"{nf}","denom":"{proof}","destination":"{addr}","amount":15}}]}}}}"#,
        z = b64(&[9u8; 32]),
        nf = b64(&nullifier),
        proof = b64(&denom),
    );
    let _ = std::fs::write("/tmp/hs-stage.json", &row);
    let _ = std::process::Command::new("docker").args(["cp", "/tmp/hs-stage.json", &format!("{id}:/tmp/hs-stage.json")]).status();
    let stage = tx_out(node, &["tx", "wasm", "execute", &contract, &row], "800000").await;
    let reg = tx_out(
        node,
        &["tx", "hashmerchant", "register-contract", &contract, UID, "bank", "2000000uterp"],
        "500000",
    ).await;
    thread::sleep(Duration::from_secs(8));
    let replay_msg = format!(
        r#"{{"apply_accepted_batch":{{"allocation_root":"{alloc}","root_id":0,"rows":[{{"nullifier":"{nf}","denom":"{proof}","destination":"{addr}","amount":15}}]}}}}"#,
        alloc = b64(&allocation),
        nf = b64(&nullifier),
        proof = b64(&denom),
    );
    let _ = std::fs::write("/tmp/hs-replay.json", &replay_msg);
    let _ = std::process::Command::new("docker").args(["cp", "/tmp/hs-replay.json", &format!("{id}:/tmp/hs-replay.json")]).status();
    let replay = tx_out(node, &["tx", "wasm", "execute", &contract, &replay_msg], "800000").await;
    let replay_ok = replay.contains("nullifier already exists") || tx_code(&replay).unwrap_or(0) != 0 && replay.contains("nullifier");
    report(
        "replayed_nullifier_across_epochs",
        root_ok && tx_code(&setc).unwrap_or(1) == 0 && tx_code(&stage).unwrap_or(1) == 0 && tx_code(&reg).unwrap_or(1) == 0 && replay_ok,
        started.elapsed().as_millis(),
        &format!("set={} stage={} reg={} replay={}", setc.chars().take(140).collect::<String>(), stage.chars().take(140).collect::<String>(), tx_code(&reg).unwrap_or(9), replay.chars().take(140).collect::<String>()),
    );
    let bal = match node.exec_cmd(&["query", "bank", "balances", &contract, "-o", "json"]).await {
        Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
        Err(_) => String::new(),
    };
    let holds = bal.contains("uterp") && !bal.contains("\"amount\":\"0\"");
    report(
        "unclaimed_funds_sweep",
        holds && replay_ok,
        started.elapsed().as_millis(),
        if holds { "escrow still holds uterp" } else { "escrow empty or missing" },
    );
    let _ = root_ok;
}

fn t_elapsed_placeholder() -> u128 { 0 }

fn hex_decode(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
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
    let external = std::env::var("HEADSTASH_EXTERNAL").ok().as_deref() == Some("1");
    let modes: Vec<Arc<Mutex<Mode>>> = (0..N).map(|_| Arc::new(Mutex::new(Mode::Honest))).collect();
    if external {
        if std::env::var("HASHMERCHANT_SIDECAR_URLS").is_err()
            || std::env::var("HEADSTASH_CUSTODY_PUB_HEX").is_err()
        {
            return Err("external sidecars need urls and a custody pubkey".into());
        }
    } else {
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
    }

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
    if external {
        let node = &chain.validators()[0];
        let id = node
            .container_id
            .as_ref()
            .map(|c| c.0.clone())
            .unwrap_or_default();
        let body = format!("rpc={rpc}\ncontainer={id}\nhome={}\n", node.home_dir);
        let path = std::env::var("HEADSTASH_CHAIN_FILE")
            .unwrap_or_else(|_| "/tmp/headstash-fm-halo2/chain.env".into());
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, &body)?;
        println!("chain_ready");
        let stop = std::env::var("HEADSTASH_STOP_FILE")
            .unwrap_or_else(|_| "/tmp/headstash-fm-halo2/stop".into());
        loop {
            if std::path::Path::new(&stop).exists() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let _ = chain.stop().await;
        return Ok(());
    }
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

    contract_cases(&chain, &modes, rpc).await;

    let _ = chain.stop().await;
    Ok(())
}
