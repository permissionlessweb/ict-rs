//! ITL consume plane on ict-rs Docker Terp. Refuses ICT_MOCK=1.
//! Halo2 Pasta Poseidon merkle+nullifier. Store cw-ave with circuit VK
//! (`MsgStoreCodeWithCircuit`). Daemon, not MockBech32.
//!
//! Does **not** use FROST / artist-pay terpd home or FROST keys.
//! Chain id: `itl-cashapp-1`.
//!
//! Valid usher consume; replay fail; non-usher fail; no guest roster query.
//!
//! ```sh
//! TERP_IMAGE_VERSION=v6.2.1 cargo run --example itl_consume_e2e -p ict-rs-cw-orch --features docker
//! ```

use ict_rs::cosmos::interchain::wait_for_blocks;
use ict_rs::prelude::*;
use ict_rs_cw_orch::daemon_builder_from_chain;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use hash_market::onboard_authz::itl_cashapp::{synthetic_received_eml, LIST_CENTS, SKU};
use hash_market::onboard_authz::itl_consume::{
    consume_backend_mocked, consume_success_allowed, fp_to_bytes, msg_store_code_with_circuit,
    prove_issued, verify_halo2_proof, ConsumeEngine, K, MSG_STORE_CODE_WITH_CIRCUIT, PROVER_ID,
};
use hash_market::onboard_authz::itl_ingress::{ict_mock_refused, IngressEngine, EVENT_ID};

/// ict-rs lab faucet mnemonic (same as svg_mint_cw_orch_runtime). Not FROST.
const LAB_MNEMONIC: &str =
    "notice oak worry limit wrap speak medal online prefer cluster roof addict wrist behave treat actual wasp year salad speed social layer crew genius";

const CHAIN_ID: &str = "itl-cashapp-1";
/// Factory instantiate fee for itl-cashapp-1 (`av_event_helpers::get_license_fee`).
const LICENSE_FEE: &str = "1000000uthiol";
const LICENSE_DENOM: &str = "uthiol";
const LICENSE_AMOUNT: u128 = 1_000_000;
const LICENSE_ADDR: &str = "terp1tzz4sp3y8l5lf76qy0ydzjwlntcu8zg76zmkhg";

fn ensure_docker_path() {
    let path = std::env::var("PATH").unwrap_or_default();
    let extra = "/usr/local/bin:/Applications/Docker.app/Contents/Resources/bin";
    if !path.split(':').any(|p| p == "/usr/local/bin") {
        std::env::set_var("PATH", format!("{extra}:{path}"));
    }
}

fn docker_bin() -> &'static str {
    if PathBuf::from("/usr/local/bin/docker").is_file() {
        "/usr/local/bin/docker"
    } else {
        "docker"
    }
}

fn refuse_mock() -> Result<(), Box<dyn std::error::Error>> {
    if ict_mock_refused(std::env::var("ICT_MOCK").ok().as_deref()) {
        return Err("itl_consume_e2e refuses ICT_MOCK=1 (not MockBech32 success)".into());
    }
    if consume_backend_mocked("daemon", std::env::var("ICT_MOCK").ok().as_deref()) {
        return Err("itl_consume_e2e refuses mocked backend".into());
    }
    Ok(())
}

fn find_named_wasm(names: &[&str]) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ITL_CWAVE_WASM") {
        let pb = PathBuf::from(&p);
        if pb.is_file() && names.iter().any(|n| p.ends_with(n)) {
            return Some(pb);
        }
    }
    let roots = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../cw-ave"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../itl-lab/cw-ave"),
        PathBuf::from("/Users/returniflost/abstract/itl-lab/cw-ave"),
        PathBuf::from("/Users/returniflost/abstract/terp-core/crates/cw-ave"),
    ];
    let rels = [
        "artifacts/cw_ave.wasm",
        "artifacts/cw420.wasm",
        "artifacts/cw_ave_factory.wasm",
        "artifacts/cw-ave-factory.wasm",
        "target/wasm32-unknown-unknown/release/cw_ave.wasm",
        "target/wasm32-unknown-unknown/release/cw420.wasm",
        "target/wasm32-unknown-unknown/release/cw_ave_factory.wasm",
        "contracts/cw-ave/artifacts/cw_ave.wasm",
        "contracts/cw420/artifacts/cw420.wasm",
        "contracts/cw-ave-factory/artifacts/cw_ave_factory.wasm",
    ];
    for root in roots {
        for rel in rels {
            let p = root.join(rel);
            if p.is_file() && names.iter().any(|n| p.file_name().and_then(|s| s.to_str()) == Some(*n))
            {
                return Some(p);
            }
        }
    }
    None
}


/// zk-wasmvm store-circuit SoT: `[params][cs][vk][CircuitFooter]` from itl-zk-cw-orch.
/// Not the onboard-authz ITL1 10-byte host handle.
fn store_circuit_vk_path() -> PathBuf {
    if let Ok(p) = std::env::var("ITL_CONSUME_VK_PATH") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../itl-zk-cw-orch/artifacts/itl_consume_vk.bin")
}

fn split_store_circuit(blob: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    const FOOT: usize = 80;
    if blob.len() < FOOT {
        return Err("store-circuit blob shorter than CircuitFooter".into());
    }
    let f = &blob[blob.len() - FOOT..];
    let prover = f[0];
    let curve = f[1];
    let k = f[2];
    let i = f[3];
    let pl = u32::from_le_bytes(f[4..8].try_into().unwrap()) as usize;
    let cs = u32::from_le_bytes(f[8..12].try_into().unwrap()) as usize;
    let vl = u32::from_le_bytes(f[12..16].try_into().unwrap()) as usize;
    if prover != PROVER_ID || curve != 0 || k != K as u8 || i != 3 {
        return Err(format!(
            "footer expected Plonkish/Pasta k={} i=3, got prover={prover} curve={curve} k={k} i={i}",
            K
        ));
    }
    let expect = pl + cs + vl + FOOT;
    if blob.len() != expect {
        return Err(format!(
            "blob len {} != param+cs+vk+footer {expect}",
            blob.len()
        ));
    }
    if cs == 0 {
        return Err("cs_len=0 is not a Plonkish combined blob".into());
    }
    // v6.2.1 `store-full-circuit [params] [vk body]`: vk body is cs||vk||CircuitFooter.
    Ok((blob[..pl].to_vec(), blob[pl..].to_vec()))
}


fn encode_msg_store_code_with_circuit(sender: &str, wasm: &[u8], vk: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&msg_store_code_with_circuit(sender, wasm, vk)).unwrap_or_default()
}

fn load_store_circuit_vk() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let path = store_circuit_vk_path();
    let bytes = std::fs::read(&path).map_err(|e| {
        format!(
            "store-circuit VK missing at {} ({e}); run: cd crates/itl-zk-cw-orch && cargo run --bin itl-write-vk --release",
            path.display()
        )
    })?;
    let _ = split_store_circuit(&bytes)?;
    Ok(bytes)
}

fn find_ave_wasm() -> Option<PathBuf> {
    find_named_wasm(&["cw_ave.wasm", "cw-ave.wasm"])
}

fn find_cw420_wasm() -> Option<PathBuf> {
    find_named_wasm(&["cw420.wasm"])
}

fn find_factory_wasm() -> Option<PathBuf> {
    find_named_wasm(&["cw_ave_factory.wasm", "cw-ave-factory.wasm"])
}

fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i];
        let b1 = if i + 1 < data.len() { data[i + 1] } else { 0 };
        let b2 = if i + 2 < data.len() { data[i + 2] } else { 0 };
        let triple = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(T[((triple >> 18) & 63) as usize] as char);
        out.push(T[((triple >> 12) & 63) as usize] as char);
        out.push(if i + 1 < data.len() {
            T[((triple >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if i + 2 < data.len() {
            T[(triple & 63) as usize] as char
        } else {
            '='
        });
        i += 3;
    }
    out
}

mod hex {
    pub fn encode_opt(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}

fn first_json(s: &str) -> Option<Value> {
    if let Some(i) = s.find('{') {
        if let Ok(v) = serde_json::from_str::<Value>(s[i..].trim()) {
            return Some(v);
        }
    }
    for line in s.lines() {
        let t = line.trim();
        if t.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(t) {
                return Some(v);
            }
        }
    }
    None
}

fn run_docker(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(docker_bin()).args(args).output();
    match out {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        ),
        Err(e) => (false, String::new(), e.to_string()),
    }
}

fn terpd(container: &str, args: &[&str]) -> (bool, String, String) {
    // ict-rs node home (not default ~/.terpd, never FROST/artist-pay).
    let home = format!("/var/cosmos-chain/{CHAIN_ID}");
    let mut v = vec!["exec", container, "terpd"];
    v.extend_from_slice(args);
    if !args.iter().any(|a| *a == "--home") {
        v.extend_from_slice(&["--home", &home]);
    }
    run_docker(&v)
}

fn wait_tx(container: &str, hash: &str) -> Option<Value> {
    if hash.is_empty() {
        return None;
    }
    for _ in 0..30 {
        let (ok, stdout, _) = terpd(container, &["query", "tx", hash, "--output", "json"]);
        if ok {
            return first_json(&stdout);
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    None
}

fn txhash_of(v: &Value) -> Option<String> {
    v.get("txhash")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn txhash_in_text(s: &str) -> Option<String> {
    for key in ["\"txhash\":\"", "txhash: ", "txhash="] {
        if let Some(i) = s.find(key) {
            let rest = &s[i + key.len()..];
            let h: String = rest
                .chars()
                .take_while(|c| c.is_ascii_hexdigit())
                .collect();
            if h.len() >= 32 {
                return Some(h);
            }
        }
    }
    None
}

fn code_ids_from_list(v: &Value) -> Vec<String> {
    v.get("code_infos")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    c.get("code_id")
                        .and_then(|id| id.as_str().map(|s| s.to_string()))
                        .or_else(|| c.get("code_id").and_then(|id| id.as_u64()).map(|n| n.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn list_code(container: &str) -> (bool, Vec<String>, Option<Value>) {
    let (ok, stdout, _) = terpd(container, &["query", "wasm", "list-code", "--output", "json"]);
    let v = first_json(&stdout);
    let ids = v.as_ref().map(code_ids_from_list).unwrap_or_default();
    (ok, ids, v)
}

fn tx_ok(v: &Value) -> bool {
    match v.get("code") {
        Some(Value::Number(n)) => n.as_i64() == Some(0),
        Some(Value::String(s)) => s == "0",
        _ => false,
    }
}

fn tx_rawlog(v: &Value) -> String {
    v.get("raw_log")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .chars()
        .take(500)
        .collect()
}

fn store_wasm(container: &str, host: &PathBuf, dest: &str, from: &str) -> (Option<String>, Option<String>, Vec<String>) {
    let _ = run_docker(&["cp", host.to_str().unwrap_or(""), &format!("{container}:{dest}")]);
    let (ok, stdout, stderr) = terpd(
        container,
        &[
            "tx",
            "wasm",
            "store",
            dest,
            "--from",
            from,
            "--chain-id",
            CHAIN_ID,
            "--keyring-backend",
            "test",
            "--gas",
            "auto",
            "--gas-adjustment",
            "1.6",
            "--gas-prices",
            "0.025uterp",
            "--yes",
            "--output",
            "json",
        ],
    );
    let parsed = first_json(&stdout).or_else(|| first_json(&stderr));
    let hash = parsed.as_ref().and_then(txhash_of);
    if let Some(h) = hash.as_deref() {
        let _ = wait_tx(container, h);
    }
    std::thread::sleep(Duration::from_millis(800));
    let (_, ids, _) = list_code(container);
    let err = if ok {
        None
    } else {
        Some(format!("store {dest}: {stderr} {stdout}"))
    };
    (hash, err, ids)
}


fn tx_flags<'a>(from: &'a str) -> Vec<&'a str> {
    vec![
        "--from",
        from,
        "--chain-id",
        CHAIN_ID,
        "--keyring-backend",
        "test",
        "--gas",
        "auto",
        "--gas-adjustment",
        "1.6",
        "--gas-prices",
        "0.025uterp",
        "--yes",
        "--output",
        "json",
    ]
}

/// Store cw-ave wasm + Halo2 VK as MsgStoreCodeWithCircuit (or lab CLI aliases).
/// Not plain `MsgStoreCode` as the success path.

fn zk_id_of(v: &Value) -> Option<u64> {
    fn parse_id(val: &Value) -> Option<u64> {
        if let Some(n) = val.as_u64() {
            if n > 0 {
                return Some(n);
            }
        }
        if let Some(s) = val.as_str() {
            if let Ok(n) = s.parse::<u64>() {
                if n > 0 {
                    return Some(n);
                }
            }
        }
        None
    }
    fn walk(v: &Value) -> Option<u64> {
        match v {
            Value::Object(map) => {
                for (k, val) in map {
                    let key = k.replace('-', "_").to_ascii_lowercase();
                    if key == "zk_id" || key == "zkid" {
                        if let Some(n) = parse_id(val) {
                            return Some(n);
                        }
                    }
                }
                if let (Some(k), Some(val)) = (map.get("key"), map.get("value")) {
                    let key = k
                        .as_str()
                        .unwrap_or("")
                        .replace('-', "_")
                        .to_ascii_lowercase();
                    if key == "zk_id" || key == "zkid" {
                        if let Some(n) = parse_id(val) {
                            return Some(n);
                        }
                    }
                }
                for val in map.values() {
                    if let Some(n) = walk(val) {
                        return Some(n);
                    }
                }
                None
            }
            Value::Array(arr) => {
                for val in arr {
                    if let Some(n) = walk(val) {
                        return Some(n);
                    }
                }
                None
            }
            _ => None,
        }
    }
    walk(v)
}

fn store_code_with_circuit(
    container: &str,
    wasm_host: &PathBuf,
    vk: &[u8],
    from: &str,
) -> (Option<String>, Option<String>, Vec<String>, String, Option<u64>) {
    let _ = run_docker(&[
        "cp",
        wasm_host.to_str().unwrap_or(""),
        &format!("{container}:/tmp/cw_ave.wasm"),
    ]);
    let vk_host = std::env::temp_dir().join("itl-consume.vk");
    let _ = std::fs::write(&vk_host, vk);
    let _ = run_docker(&[
        "cp",
        vk_host.to_str().unwrap_or(""),
        &format!("{container}:/tmp/itl-consume.vk"),
    ]);
    if let Ok((params, body)) = split_store_circuit(vk) {
        let ph = std::env::temp_dir().join("itl-consume.params");
        let bh = std::env::temp_dir().join("itl-consume.vkbody");
        let _ = std::fs::write(&ph, &params);
        let _ = std::fs::write(&bh, &body);
        let _ = run_docker(&["cp", ph.to_str().unwrap(), &format!("{container}:/tmp/itl-consume.params")]);
        let _ = run_docker(&["cp", bh.to_str().unwrap(), &format!("{container}:/tmp/itl-consume.vkbody")]);
    }

    let flags = tx_flags(from);
    let kflag = K.to_string();
    let mut full = vec![
        "tx".into(),
        "wasm".into(),
        "store-full-circuit".into(),
        "/tmp/itl-consume.params".into(),
        "/tmp/itl-consume.vkbody".into(),
        "--k".into(),
        kflag,
        "--circuit-type".into(),
        "0".into(),
        "--curve-type".into(),
        "0".into(),
    ];
    full.extend(flags.iter().map(|s| s.to_string()));
    let arg_refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
    let (ok, stdout, stderr) = terpd(container, &arg_refs);
    let parsed = first_json(&stdout).or_else(|| first_json(&stderr));
    let hash = parsed.as_ref().and_then(txhash_of);
    let mut last_err = None;
    let mut circuit_hash = None;
    let mut circuit_zkid = None;
    let mut used = "store-full-circuit".to_string();
    if let Some(h) = hash.as_deref() {
        let body = wait_tx(container, h);
        if body.as_ref().map(tx_ok).unwrap_or(ok) {
            circuit_hash = hash.clone();
            circuit_zkid = body.as_ref().and_then(zk_id_of).or_else(|| parsed.as_ref().and_then(zk_id_of));
        } else {
            let raw = body.as_ref().or(parsed.as_ref()).map(tx_rawlog).unwrap_or_default();
            last_err = Some(format!("store-full-circuit: {raw}"));
        }
    } else if !ok {
        last_err = Some(format!("store-full-circuit: {stderr} {stdout}"));
    }

    // v6.2.1: wasm store is a separate tx. Do not try store-code-with-circuit.
    let (whash, err, ids) = store_wasm(container, wasm_host, "/tmp/cw_ave.wasm", from);
    if circuit_hash.is_none() {
        let (ok, stdout, stderr) = terpd(container, &arg_refs);
        let parsed = first_json(&stdout).or_else(|| first_json(&stderr));
        if let Some(h) = parsed.as_ref().and_then(txhash_of) {
            let body = wait_tx(container, &h);
            circuit_hash = Some(h.to_string());
            circuit_zkid = body.as_ref().and_then(zk_id_of).or_else(|| parsed.as_ref().and_then(zk_id_of));
            used = format!("{used}+retry");
        } else if !ok {
            last_err = Some(format!(
                "{} store-full-circuit: {stderr} {stdout}",
                last_err.unwrap_or_default()
            ));
        }
    } else {
        used = "store-full-circuit+store".to_string();
    }
    let combined_err = match (err, last_err) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (Some(a), None) => Some(a),
        (None, b) => b,
    };
    (whash.or(circuit_hash), combined_err, ids, used, circuit_zkid)
}

fn exec_msg(
    container: &str,
    from: &str,
    contract: &str,
    msg: &str,
) -> (Option<String>, Option<Value>, String) {
    // Fixed gas so expected-fail consumes still broadcast (not dropped by --gas auto sim).
    let (ok, stdout, stderr) = terpd(
        container,
        &[
            "tx",
            "wasm",
            "execute",
            contract,
            msg,
            "--from",
            from,
            "--chain-id",
            CHAIN_ID,
            "--keyring-backend",
            "test",
            "--gas",
            "500000",
            "--gas-prices",
            "0.025uterp",
            "--broadcast-mode",
            "sync",
            "--yes",
            "--output",
            "json",
        ],
    );
    let parsed = first_json(&stdout).or_else(|| first_json(&stderr));
    let hash = parsed
        .as_ref()
        .and_then(txhash_of)
        .or_else(|| txhash_in_text(&stdout))
        .or_else(|| txhash_in_text(&stderr));
    let included = hash.as_deref().and_then(|h| wait_tx(container, h));
    let body = included.or(parsed);
    let detail = if ok {
        body.as_ref().map(tx_rawlog).unwrap_or_default()
    } else {
        format!("{stderr} {stdout}")
    };
    (hash, body, detail)
}

fn query_smart(container: &str, contract: &str, msg: &str) -> Option<Value> {
    let (ok, stdout, _) = terpd(
        container,
        &[
            "query",
            "wasm",
            "contract-state",
            "smart",
            contract,
            msg,
            "--output",
            "json",
        ],
    );
    if ok {
        first_json(&stdout)
    } else {
        None
    }
}

fn keys_show(container: &str, name: &str) -> Option<String> {
    let (ok, stdout, _) = terpd(
        container,
        &["keys", "show", name, "--keyring-backend", "test", "-a"],
    );
    if ok {
        let a = stdout.trim().to_string();
        if a.starts_with("terp1") {
            return Some(a);
        }
    }
    None
}


fn bank_denom_amount(container: &str, addr: &str, denom: &str) -> u128 {
    if addr.is_empty() {
        return 0;
    }
    let (ok, stdout, _) = terpd(
        container,
        &["query", "bank", "balances", addr, "--output", "json"],
    );
    if !ok {
        return 0;
    }
    let Some(v) = first_json(&stdout) else {
        return 0;
    };
    let Some(arr) = v.get("balances").and_then(|b| b.as_array()) else {
        return 0;
    };
    for c in arr {
        if c.get("denom").and_then(|d| d.as_str()) == Some(denom) {
            if let Some(s) = c.get("amount").and_then(|a| a.as_str()) {
                return s.parse().unwrap_or(0);
            }
            if let Some(n) = c.get("amount").and_then(|a| a.as_u64()) {
                return n as u128;
            }
        }
    }
    0
}

/// Confirm the lab license denom is registered in bank (genesis faucet coins)
/// and the deployer can pay `get_license_fee(itl-cashapp-1)`.
fn ensure_license_fee(container: &str, deployer: &str) -> Result<u128, String> {
    let have = bank_denom_amount(container, deployer, LICENSE_DENOM);
    if have < LICENSE_AMOUNT {
        return Err(format!(
            "license fee {LICENSE_FEE} not registered for {deployer}: have {have}{LICENSE_DENOM} (need {LICENSE_AMOUNT}); faucet genesis must include {LICENSE_DENOM}"
        ));
    }
    Ok(have)
}

fn local_prove() -> Result<(Value, Vec<u8>, [u8; 32], [u8; 32], [u8; 32]), Box<dyn std::error::Error>> {
    let mut ing = IngressEngine::default();
    let o = ing.create_cash_order("cred-guest");
    let eml = synthetic_received_eml(&o.pay_ref, LIST_CENTS, "D-CONSUME01");
    let issued = ing.ingest_cashapp_eml(&eml)?;
    let (proof, inst, nf) = prove_issued(
        ing.tree(),
        &issued.ticket_secret,
        SKU,
        EVENT_ID,
        issued.leaf_index,
    )?;
    if !verify_halo2_proof(&proof, &inst)? {
        return Err("local halo2 pasta verify failed".into());
    }
    let mut door = ConsumeEngine::default();
    door.add_usher("terp1usher");
    door.set_guest_root(issued.guest_root);
    let eid = fp_to_bytes(inst.event_id);
    if door.verify_consume("terp1guest", &proof, nf, eid).is_ok() {
        return Err("guest execute must fail (usher-only)".into());
    }
    door.verify_consume("terp1usher", &proof, nf, eid)
        .map_err(|e| format!("usher consume: {e}"))?;
    if door.verify_consume("terp1usher", &proof, nf, eid).is_ok() {
        return Err("replay must fail".into());
    }
    if door.guest_list_members().is_some() {
        return Err("guest ListMembers must be absent".into());
    }
    let vk = load_store_circuit_vk()?;
    let wasm = find_ave_wasm()
        .and_then(|p| std::fs::read(p).ok())
        .unwrap_or_else(|| b"\0asm".to_vec());
    let store = msg_store_code_with_circuit("terp1curator", &wasm, &vk);
    let local = json!({
        "proved": true,
        "prover_id": PROVER_ID,
        "guest_root": hex::encode_opt(&issued.guest_root),
        "nullifier": hex::encode_opt(&nf),
        "event_id": hex::encode_opt(&eid),
        "vk_bytes": vk.len(),
        "wasm_bytes": wasm.len(),
        "store_msg_type": store["@type"],
        "usher_ok": true,
        "replay_rejected": true,
        "non_usher_rejected": true,
        "guest_list_members": serde_json::Value::Null,
        "roster_query": false,
    });
    Ok((local, proof, nf, eid, issued.guest_root))
}

async fn spawn_terp_and_store(
    local: Value,
    proof: Vec<u8>,
    nf: [u8; 32],
    eid: [u8; 32],
    guest_root: [u8; 32],
) -> Result<Value, Box<dyn std::error::Error>> {
    ensure_docker_path();
    if std::env::var("TERP_IMAGE_VERSION")
        .ok()
        .filter(|s| !s.is_empty())
        .is_none()
        && std::env::var("ICT_IMAGE_VERSION")
            .ok()
            .filter(|s| !s.is_empty())
            .is_none()
    {
        std::env::set_var("TERP_IMAGE_VERSION", "v6.2.1");
    }
    let rt = Arc::new(DockerBackend::new(Default::default()).await?);
    let mut cfg = terp_chain_config();
    cfg.chain_id = CHAIN_ID.into();
    cfg.name = "itl-consume".into();
    cfg.genesis_style = GenesisStyle::Legacy;
    cfg.faucet = Some(FaucetConfig {
        key_name: "deployer".into(),
        port: 4500,
        start_cmd: vec![],
        env: vec![],
        mnemonic: Some(LAB_MNEMONIC.into()),
        coins: Some("100000000000uterp,100000000000uthiol".into()),
    });
    let guess = "ict-itl-consume-itl-cashapp-1-val-0";
    let _ = run_docker(&["rm", "-f", guess]);
    let mut chain = CosmosChain::new(cfg, 1, 0, rt);
    let ctx = TestContext {
        test_name: "itl-consume".into(),
        network_id: "ict-itl-consume".into(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    wait_for_blocks(&chain, 2).await?;

    let container = chain.primary_node()?.container_name();
    let builder = daemon_builder_from_chain(&chain, Some(LAB_MNEMONIC))?;
    let info = ict_rs_cw_orch::chain_info_from_cosmos(&chain)?;
    let backend = format!("{:?}", info.kind);
    if backend.to_ascii_lowercase().contains("mock") {
        let _ = chain.stop().await;
        return Err("Daemon kind looks mocked — consume success forbidden".into());
    }
    if !consume_success_allowed("daemon", None) {
        let _ = chain.stop().await;
        return Err("consume_success_allowed(daemon) false".into());
    }

    chain.create_key("usher").await?;
    chain.create_key("rando").await?;
    let deployer = keys_show(&container, "deployer").unwrap_or_default();
    let usher = keys_show(&container, "usher").unwrap_or_default();
    let rando = keys_show(&container, "rando").unwrap_or_default();
    if !usher.is_empty() {
        let _ = chain
            .send_funds(
                "deployer",
                &WalletAmount {
                    address: usher.clone(),
                    denom: "uterp".into(),
                    amount: 50_000_000,
                },
            )
            .await;
    }
    if !rando.is_empty() {
        let _ = chain
            .send_funds(
                "deployer",
                &WalletAmount {
                    address: rando.clone(),
                    denom: "uterp".into(),
                    amount: 50_000_000,
                },
            )
            .await;
    }
    wait_for_blocks(&chain, 1).await?;

    let license_have = match ensure_license_fee(&container, &deployer) {
        Ok(n) => n,
        Err(e) => {
            let _ = chain.stop().await;
            return Err(e.into());
        }
    };

    let vk = load_store_circuit_vk()?;
    let vk_path = std::env::temp_dir().join("itl-consume.vk");
    std::fs::write(&vk_path, &vk)?;
    let store_msg = msg_store_code_with_circuit("deployer", &[], &vk);
    let tmp = std::env::temp_dir().join("itl-consume-store-code-with-circuit.json");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&store_msg)?)?;

    let mut code_ids: Vec<String> = Vec::new();
    let mut txs: Vec<Value> = Vec::new();
    let mut store_err: Option<String> = None;
    let mut cw420_id: Option<String> = None;
    let mut ave_id: Option<String> = None;

    if let Some(wp) = find_cw420_wasm() {
        let (h, err, ids) = store_wasm(&container, &wp, "/tmp/cw420.wasm", "deployer");
        if let Some(hh) = h {
            txs.push(json!({"kind": "store_cw420", "txhash": hh}));
        }
        if let Some(e) = err {
            store_err = Some(e);
        }
        code_ids = ids.clone();
        cw420_id = ids.last().cloned();
    } else {
        store_err = Some("cw420.wasm missing".into());
    }

    let mut factory_id: Option<String> = None;
    let mut store_kind = MSG_STORE_CODE_WITH_CIRCUIT.to_string();
    let mut stored_zkid: Option<u64> = None;
    if let Some(wp) = find_factory_wasm() {
        let (h, err, ids) = store_wasm(&container, &wp, "/tmp/cw_ave_factory.wasm", "deployer");
        if let Some(hh) = h {
            txs.push(json!({"kind": "store_factory", "txhash": hh}));
        }
        if let Some(e) = err {
            store_err = Some(store_err.map(|p| format!("{p}; {e}")).unwrap_or(e));
        }
        code_ids = ids.clone();
        factory_id = ids.last().cloned();
    }

    let ave_path = find_ave_wasm();
    if let Some(wp) = &ave_path {
        let wasm_bytes = std::fs::read(wp).unwrap_or_default();
        let store_msg = msg_store_code_with_circuit(&deployer, &wasm_bytes, &vk);
        std::fs::write(&tmp, serde_json::to_vec_pretty(&store_msg)?)?;
        let (h, err, ids, kind, zkid) = store_code_with_circuit(&container, wp, &vk, "deployer");
        store_kind = kind;
        stored_zkid = zkid;
        if let Some(hh) = h {
            txs.push(json!({"kind": "store_cw_ave_with_circuit", "txhash": hh, "msg": MSG_STORE_CODE_WITH_CIRCUIT}));
        }
        if let Some(e) = err {
            store_err = Some(store_err.map(|p| format!("{p}; {e}")).unwrap_or(e));
        }
        code_ids = ids.clone();
        ave_id = ids.last().cloned();
    } else if store_err.is_none() {
        store_err = Some(
            "cw_ave.wasm not built; MsgStoreCodeWithCircuit JSON written, not MockBech32".into(),
        );
    }
    let _ = &vk_path;

    let mut contract: Option<String> = None;
    let mut instantiate_err: Option<String> = None;
    let mut factory_addr: Option<String> = None;
    if let (Some(fac), Some(ave), true) = (factory_id.clone(), ave_id.clone(), !deployer.is_empty()) {
        let fac_msg = json!({"owner": None::<String>, "cw_ave_id": ave.parse::<u64>().unwrap_or(1)}).to_string();
        let (ok, stdout, stderr) = terpd(
            &container,
            &[
                "tx", "wasm", "instantiate", &fac, &fac_msg,
                "--label", "itl-consume-factory",
                "--from", "deployer",
                "--chain-id", CHAIN_ID,
                "--keyring-backend", "test",
                "--no-admin",
                "--amount", LICENSE_FEE,
                "--gas", "auto",
                "--gas-adjustment", "2.0",
                "--gas-prices", "0.025uterp",
                "--yes",
                "--output", "json",
            ],
        );
        let parsed = first_json(&stdout).or_else(|| first_json(&stderr));
        if let Some(h) = parsed.as_ref().and_then(txhash_of) {
            txs.push(json!({"kind": "instantiate_factory", "txhash": h, "license_fee": LICENSE_FEE, "license_addr": LICENSE_ADDR}));
            if let Some(tx) = wait_tx(&container, &h) {
                if !tx_ok(&tx) {
                    instantiate_err = Some(format!("factory instantiate: {}", tx_rawlog(&tx)));
                }
                if let Some(events) = tx.get("events").and_then(|e| e.as_array()) {
                    for ev in events {
                        if ev.get("type").and_then(|t| t.as_str()) == Some("instantiate") {
                            if let Some(attrs) = ev.get("attributes").and_then(|a| a.as_array()) {
                                for attr in attrs {
                                    if attr.get("key").and_then(|k| k.as_str()) == Some("_contract_address") {
                                        if let Some(addr) = attr.get("value").and_then(|v| v.as_str()) {
                                            factory_addr = Some(addr.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        } else if !ok {
            instantiate_err = Some(format!("factory: {stderr} {stdout}"));
        }
        if let Some(faddr) = factory_addr.clone() {
            let create = json!({
                "create_native_av_event_contract": {
                    "instantiate_msg": {
                        "event_curator": deployer,
                        "title": "itl-consume",
                        "description": "ict-rs docker consume e2e",
                        "usher_admins": [{"addr": usher, "weight": 1}],
                        "guest_details": [{
                            "guest_type": "ga",
                            "guest_weight": 1,
                            "max_ticket_limit": 8,
                            "total_ticket_limit": 32,
                            "ticket_cost": [{"denom": "uterp", "amount": "1"}],
                            "event_segment_access": {"single_segment": {"id": 0}}
                        }],
                        "cw420": cw420_id.clone().and_then(|s| s.parse::<u64>().ok()).unwrap_or(1),
                        "zkid": stored_zkid.unwrap_or(0),
                        "event_timeline": [{
                            "stage_description": "door",
                            "start": "1000000000",
                            "end": "2000000000"
                        }]
                    },
                    "label": "itl-consume"
                }
            }).to_string();
            let (ok, stdout, stderr) = terpd(
                &container,
                &[
                    "tx", "wasm", "execute", &faddr, &create,
                    "--from", "deployer",
                    "--chain-id", CHAIN_ID,
                    "--keyring-backend", "test",
                    "--amount", "1000000uterp",
                    "--gas", "auto",
                    "--gas-adjustment", "2.0",
                    "--gas-prices", "0.025uterp",
                    "--yes",
                    "--output", "json",
                ],
            );
            let parsed = first_json(&stdout).or_else(|| first_json(&stderr));
            let h = parsed.as_ref().and_then(txhash_of);
            let body = h.as_deref().and_then(|hh| wait_tx(&container, hh)).or(parsed);
            let detail = if ok {
                body.as_ref().map(tx_rawlog).unwrap_or_default()
            } else {
                format!("{stderr} {stdout}")
            };
            if let Some(hh) = h {
                txs.push(json!({"kind": "factory_create_ave", "txhash": hh}));
            } else if !ok {
                instantiate_err = Some(format!("factory create: {detail}"));
            }
            if let Some(b) = &body {
                if tx_ok(b) {
                    if let Some(events) = b.get("events").and_then(|e| e.as_array()) {
                        for ev in events {
                            if let Some(attrs) = ev.get("attributes").and_then(|a| a.as_array()) {
                                for attr in attrs {
                                    if matches!(attr.get("key").and_then(|k| k.as_str()), Some("new_ave_contract" | "_contract_address")) {
                                        if let Some(addr) = attr.get("value").and_then(|v| v.as_str()) {
                                            contract = Some(addr.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    instantiate_err = Some(format!("factory create: {detail}"));
                }
            }
        }
        wait_for_blocks(&chain, 1).await?;
    }
    if contract.is_none() && factory_addr.is_none() {
    if let (Some(ave), Some(cw4), true) = (ave_id.clone(), cw420_id.clone(), !deployer.is_empty()) {
        let inst_msg = json!({
            "event_curator": deployer,
            "title": "itl-consume",
            "description": "ict-rs docker consume e2e",
            "usher_admins": [{"addr": usher, "weight": 1}],
            "guest_details": [{
                "guest_type": "ga",
                "guest_weight": 1,
                "max_ticket_limit": 8,
                "total_ticket_limit": 32,
                "ticket_cost": [{"denom": "uterp", "amount": "1"}],
                "event_segment_access": {"single_segment": {"id": 0}}
            }],
            "cw420": cw4.parse::<u64>().unwrap_or(1),
            "zkid": stored_zkid.unwrap_or(0),
            "event_timeline": [{
                "stage_description": "door",
                "start": "1000000000",
                "end": "2000000000"
            }]
        });
        let msg_s = inst_msg.to_string();
        let (ok, stdout, stderr) = terpd(
            &container,
            &[
                "tx",
                "wasm",
                "instantiate",
                &ave,
                &msg_s,
                "--label",
                "itl-consume",
                "--from",
                "deployer",
                "--chain-id",
                CHAIN_ID,
                "--keyring-backend",
                "test",
                "--no-admin",
                "--gas",
                "auto",
                "--gas-adjustment",
                "2.0",
                "--gas-prices",
                "0.025uterp",
                "--yes",
                "--output",
                "json",
            ],
        );
        let parsed = first_json(&stdout).or_else(|| first_json(&stderr));
        let hash = parsed.as_ref().and_then(txhash_of);
        if let Some(h) = hash.as_deref() {
            txs.push(json!({"kind": "instantiate", "txhash": h}));
            if let Some(tx) = wait_tx(&container, h) {
                if !tx_ok(&tx) {
                    instantiate_err = Some(tx_rawlog(&tx));
                }
                if let Some(events) = tx.get("events").and_then(|e| e.as_array()) {
                    for ev in events {
                        if ev.get("type").and_then(|t| t.as_str()) == Some("instantiate") {
                            if let Some(attrs) = ev.get("attributes").and_then(|a| a.as_array()) {
                                for attr in attrs {
                                    if attr.get("key").and_then(|k| k.as_str())
                                        == Some("_contract_address")
                                    {
                                        if let Some(addr) =
                                            attr.get("value").and_then(|v| v.as_str())
                                        {
                                            // first instantiate is cw-ave
                                            if contract.is_none() {
                                                contract = Some(addr.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        } else if !ok {
            instantiate_err = Some(format!("{stderr} {stdout}"));
        }
        wait_for_blocks(&chain, 1).await?;
    }
    } // contract.is_none fallback to direct instantiate

    let mut on_chain_usher: Option<String> = None;
    let mut on_chain_usher_ok = false;
    let mut on_chain_replay_fail = false;
    let mut on_chain_non_usher_fail = false;
    let mut on_chain_notes = Vec::new();
    let mut queried_list_members = false;

    if let Some(c) = contract.as_deref() {
        let root_hex = hex::encode_opt(&guest_root);
        let nf_hex = hex::encode_opt(&nf);
        let eid_hex = hex::encode_opt(&eid);
        let proof_b64 = b64(&proof);
        let (h, body, detail) = exec_msg(
            &container,
            "deployer",
            c,
            &json!({"update_guest_root": {"guest_root": root_hex}}).to_string(),
        );
        if let Some(hh) = h {
            txs.push(json!({"kind": "update_guest_root", "txhash": hh}));
        }
        if let Some(b) = &body {
            if !tx_ok(b) {
                on_chain_notes.push(format!("update_guest_root: {detail}"));
            }
        }

        let consume_json = json!({
            "verify_consume": {
                "proof": proof_b64,
                "nullifier": nf_hex,
                "event_id": eid_hex
            }
        })
        .to_string();

        let (h, body, detail) = exec_msg(&container, "rando", c, &consume_json);
        if let Some(hh) = h {
            txs.push(json!({"kind": "consume_non_usher", "txhash": hh}));
        }
        on_chain_non_usher_fail = body.as_ref().map(|b| !tx_ok(b)).unwrap_or(true);
        if !on_chain_non_usher_fail {
            on_chain_notes.push(format!("non-usher unexpectedly ok: {detail}"));
        }

        let (h, body, detail) = exec_msg(&container, "usher", c, &consume_json);
        if let Some(hh) = h.clone() {
            txs.push(json!({"kind": "consume_usher", "txhash": hh}));
            on_chain_usher = Some(hh);
        }
        on_chain_usher_ok = body.as_ref().map(tx_ok).unwrap_or(false);
        if !on_chain_usher_ok {
            on_chain_notes.push(format!("usher consume: {detail}"));
        }

        let (h, body, _detail) = exec_msg(&container, "usher", c, &consume_json);
        if let Some(hh) = h {
            txs.push(json!({"kind": "consume_replay", "txhash": hh}));
        }
        on_chain_replay_fail = body.as_ref().map(|b| !tx_ok(b)).unwrap_or(true);

        let _ = query_smart(&container, c, r#"{"guest_root":{}}"#);
        let _ = query_smart(&container, c, &json!({"is_usher":{"addr": usher}}).to_string());
        let _ = query_smart(
            &container,
            c,
            &json!({"nullifier_spent":{"nullifier": hex::encode_opt(&nf)}}).to_string(),
        );
        // Intentionally do **not** query guest cw420 ListMembers.
        queried_list_members = false;
    }

    let grpc = info.grpc_urls.clone();
    let _ = chain.stop().await;

    let mut out = local;
    out["live"] = json!(true);
    out["mocked"] = json!(false);
    out["chain_id"] = json!(CHAIN_ID);
    out["grpc"] = json!(grpc);
    out["engine"] = json!("ict-rs-docker+cw-orch-Daemon");
    out["msg"] = json!(MSG_STORE_CODE_WITH_CIRCUIT);
    out["store_msg_path"] = json!(tmp.display().to_string());
    out["list_code_ok"] = json!(!code_ids.is_empty());
    out["code_ids"] = json!(code_ids);
    out["cw420_code_id"] = json!(cw420_id);
    out["cw_ave_code_id"] = json!(ave_id);
    out["factory_code_id"] = json!(factory_id);
    out["factory"] = json!(factory_addr);
    out["license_fee"] = json!(LICENSE_FEE);
    out["license_addr"] = json!(LICENSE_ADDR);
    out["license_uthiol"] = json!(license_have);
    out["factory_ok"] = json!(factory_addr.as_ref().map(|s| !s.is_empty()).unwrap_or(false));
    out["store_kind"] = json!(store_kind);
    out["zkid"] = json!(stored_zkid);
    out["contract"] = json!(contract);
    out["txs"] = json!(txs);
    out["store_err"] = json!(store_err);
    out["instantiate_err"] = json!(instantiate_err);
    out["daemon"] = json!(true);
    out["kind"] = json!(backend);
    out["deployer"] = json!(deployer);
    out["usher"] = json!(usher);
    out["rando"] = json!(rando);
    out["on_chain_usher_ok"] = json!(on_chain_usher_ok);
    out["on_chain_usher_tx"] = json!(on_chain_usher);
    out["on_chain_replay_fail"] = json!(on_chain_replay_fail);
    out["on_chain_non_usher_fail"] = json!(on_chain_non_usher_fail);
    out["on_chain_notes"] = json!(on_chain_notes);
    out["guest_list_members"] = json!(null);
    out["roster_query"] = json!(queried_list_members);
    out["container"] = json!(container);
    let _ = builder;
    if code_ids.is_empty() {
        return Err(format!(
            "no code_ids on {CHAIN_ID}: {}",
            out.get("store_err").cloned().unwrap_or(json!(null))
        )
        .into());
    }
    Ok(out)
}

#[tokio::main]
async fn main() {
    ensure_docker_path();
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .init();
    if let Err(e) = refuse_mock() {
        eprintln!("{e}");
        println!(
            "{}",
            json!({ "live": false, "mocked": true, "error": e.to_string() })
        );
        std::process::exit(2);
    }
    let (local, proof, nf, eid, root) = match local_prove() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            println!(
                "{}",
                json!({ "live": false, "mocked": false, "error": e.to_string(), "stage": "halo2-pasta" })
            );
            std::process::exit(2);
        }
    };
    match spawn_terp_and_store(local, proof, nf, eid, root).await {
        Ok(v) => {
            println!("{v}");
            let live = v.get("live") == Some(&json!(true));
            let mocked = v.get("mocked") == Some(&json!(true));
            let usher = v.get("on_chain_usher_ok") == Some(&json!(true));
            let replay = v.get("on_chain_replay_fail") == Some(&json!(true));
            let non = v.get("on_chain_non_usher_fail") == Some(&json!(true));
            let roster = v.get("guest_list_members") == Some(&json!(null))
                && v.get("roster_query") != Some(&json!(true));
            let codes = v
                .get("code_ids")
                .and_then(|c| c.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            let zkid_ok = v.get("zkid").and_then(|z| z.as_u64()).unwrap_or(0) > 0;
            let factory_ok = v.get("factory_ok") == Some(&json!(true));
            if !(live && !mocked && usher && replay && non && roster && codes && zkid_ok && factory_ok) {
                std::process::exit(2);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            println!(
                "{}",
                json!({
                    "live": false,
                    "mocked": false,
                    "error": e.to_string(),
                    "stage": "ict-rs-docker",
                    "msg": MSG_STORE_CODE_WITH_CIRCUIT,
                })
            );
            std::process::exit(2);
        }
    }
}
