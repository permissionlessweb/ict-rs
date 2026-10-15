//! ITL FROST local prod: ict-rs Docker Terp + rotate Execute (not Authenticate).
//! Store/instantiate terp-frost wasm on the SAME chain (itl-frost-1).
//! Refuses ICT_MOCK=1. Pay jobs RedPallas→Zcash only.
//! ZEC confirmed notes stay with svg_mint_local_prod sidecars — do not duplicate Zakura spawn here.
//!
//! ```sh
//! cargo run --example itl_frost_local_prod -p ict-rs-cw-orch --features docker
//! ```

use ict_rs::prelude::*;
use ict_rs_cw_orch::chain_info_from_cosmos;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use sha2::{Digest, Sha256};

fn vk_commit(group_vk: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"itl-frost-rotate-v1");
    h.update(group_vk.as_bytes());
    let bytes = h.finalize();
    const HEX: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

fn refuse_mock() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("itl_frost_local_prod refuses ICT_MOCK=1".into());
    }
    Ok(())
}

fn http_json(method: &str, path: &str, body: &str) -> Result<(u16, String), Box<dyn std::error::Error>> {
    let mut s = TcpStream::connect("127.0.0.1:19091")?;
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes())?;
    let mut buf = String::new();
    s.read_to_string(&mut buf)?;
    let code = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    Ok((code, body))
}

fn frost_wasm_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(p) = std::env::var("ITL_FROST_WASM") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Ok(pb);
        }
        return Err(format!("ITL_FROST_WASM not a file: {}", pb.display()).into());
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cands = [
        manifest.join("../../terp-rs/contracts/smart-accounts/terp-frost/artifacts/terp_frost.wasm"),
        manifest.join("../../terp-rs/target/wasm32-unknown-unknown/release/terp_frost.wasm"),
    ];
    for p in cands {
        if p.is_file() {
            return Ok(p);
        }
    }
    Err("terp_frost.wasm missing — just wasm-dev in contracts/smart-accounts/terp-frost".into())
}

fn extract_code_id(tx: &Value) -> Option<u64> {
    let events = tx.get("events")?.as_array()?;
    for ev in events {
        if ev.get("type").and_then(|t| t.as_str()) != Some("store_code") {
            continue;
        }
        for attr in ev.get("attributes")?.as_array()? {
            if attr.get("key").and_then(|k| k.as_str()) == Some("code_id") {
                if let Some(s) = attr.get("value").and_then(|v| v.as_str()) {
                    return s.parse().ok();
                }
            }
        }
    }
    None
}

fn qdata(v: &Value) -> &Value {
    v.get("data").unwrap_or(v)
}

async fn store_code_wait(chain: &CosmosChain, container_wasm: &str) -> Result<(u64, String), String> {
    let opts = chain.default_tx_opts().from("validator").gas_adjustment(2.0);
    let out = chain
        .chain_exec_tx_with(&["tx", "wasm", "store", container_wasm], opts)
        .await
        .map_err(|e| format!("broadcast: {e}"))?;
    let raw = out.stdout_str();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "empty store stdout stderr={}",
            out.stderr_str().trim()
        ));
    }
    let v: Value = serde_json::from_str(trimmed).map_err(|e| format!("tx json: {e} body={trimmed}"))?;
    if let Some(code) = v.get("code").and_then(|c| c.as_u64()) {
        if code != 0 {
            return Err(format!(
                "tx code={code} raw={}",
                v.get("raw_log").and_then(|x| x.as_str()).unwrap_or("")
            ));
        }
    }
    let hash = v
        .get("txhash")
        .and_then(|h| h.as_str())
        .ok_or_else(|| format!("no txhash in {trimmed}"))?;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let q = chain
            .chain_exec(&["query", "tx", hash, "--output", "json"])
            .await;
        let Ok(q) = q else { continue };
        let body = q.stdout_str();
        let Ok(tv) = serde_json::from_str::<Value>(body.trim()) else {
            continue;
        };
        if let Some(id) = extract_code_id(&tv) {
            return Ok((id, hash.to_string()));
        }
    }
    Err(format!("timeout waiting for store tx {hash}"))
}

fn msg_is_rotate(msg: Option<&Value>) -> bool {
    match msg {
        Some(Value::Object(o)) => o.contains_key("rotate_frost_pubkey"),
        Some(Value::String(s)) => {
            if s.contains("rotate_frost_pubkey") {
                return true;
            }
            base64_decode(s)
                .map(|b| String::from_utf8_lossy(&b).contains("rotate_frost_pubkey"))
                .unwrap_or(false)
        }
        _ => false,
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let bytes = s.trim().as_bytes();
    let mut out = Vec::new();
    let mut buf: u32 = 0;
    let mut n = 0;
    for &c in bytes {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        let v = val(c)?;
        buf = (buf << 6) | u32::from(v);
        n += 1;
        if n == 4 {
            out.push((buf >> 16) as u8);
            out.push((buf >> 8) as u8);
            out.push(buf as u8);
            buf = 0;
            n = 0;
        }
    }
    if n == 2 {
        out.push((buf >> 4) as u8);
    } else if n == 3 {
        out.push((buf >> 10) as u8);
        out.push((buf >> 2) as u8);
    }
    Some(out)
}

/// True only for MsgExecuteContract rotate_frost_pubkey on this contract from this sender.
fn is_committee_rotate(tx: &Value, contract: &str, sender: &str) -> bool {
    let Some(msgs) = tx.pointer("/tx/body/messages").and_then(|v| v.as_array()) else {
        return false;
    };
    msgs.iter().any(|m| {
        let ty = m.get("@type").and_then(|t| t.as_str()).unwrap_or("");
        ty.ends_with("MsgExecuteContract")
            && m.get("sender").and_then(|s| s.as_str()) == Some(sender)
            && m.get("contract").and_then(|c| c.as_str()) == Some(contract)
            && msg_is_rotate(m.get("msg"))
    })
}

async fn query_tx_json(chain: &CosmosChain, hash: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let q = chain
        .chain_exec(&["query", "tx", hash, "--output", "json"])
        .await?;
    let body = q.stdout_str();
    let v: Value = serde_json::from_str(body.trim())?;
    Ok(v)
}

async fn query_status(chain: &CosmosChain, contract: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let v = chain
        .query_contract(contract, r#"{"status":{}}"#)
        .await?;
    Ok(qdata(&v).clone())
}

async fn rotate_execute(chain: &CosmosChain) -> Result<Value, Box<dyn std::error::Error>> {
    let wasm = frost_wasm_path()?;
    let dest = "/tmp/terp_frost.wasm";
    chain
        .primary_node()?
        .copy_file_from_host(&wasm, dest)
        .await?;
    let (code_id, store_hash) = store_code_wait(chain, dest)
        .await
        .map_err(|e| format!("store_code: {e}"))?;
    wait_for_blocks(chain, 1).await?;

    let old_vk = "vk-old";
    let new_vk = "vk-new";
    let init = json!({
        "group_vk": old_vk,
        "t": 2,
        "n": 3,
        "seats": ["desk", "steward", "spare"],
    })
    .to_string();
    let contract = chain
        .instantiate_contract(
            "validator",
            &code_id.to_string(),
            &init,
            "itl-frost-rotate",
            None,
        )
        .await?;
    wait_for_blocks(chain, 1).await?;

    let q0 = query_status(chain, &contract).await?;
    let commit = vk_commit(new_vk);
    let invoke = json!({
        "rotate_frost_pubkey": {
            "new_vk_commit": commit,
            "hkdf_commit": "hkdf-lab",
        }
    })
    .to_string();
    let rotate_tx = chain
        .execute_contract("validator", &contract, &invoke, None)
        .await?;
    let rotate_hash = rotate_tx.tx_hash.clone();
    wait_for_blocks(chain, 1).await?;
    let q_pending = query_status(chain, &contract).await?;

    let share1 = json!({
        "add_rotate_share": {
            "new_vk": new_vk,
            "hkdf_commit": "hkdf-lab",
            "blob": "S:0:a",
            "admin_id": "desk",
        }
    })
    .to_string();
    chain
        .execute_contract("validator", &contract, &share1, None)
        .await?;
    wait_for_blocks(chain, 1).await?;
    let q_share1 = query_status(chain, &contract).await?;

    let share2 = json!({
        "add_rotate_share": {
            "new_vk": new_vk,
            "hkdf_commit": "hkdf-lab",
            "blob": "S:0:b",
            "admin_id": "steward",
        }
    })
    .to_string();
    chain
        .execute_contract("validator", &contract, &share2, None)
        .await?;
    wait_for_blocks(chain, 1).await?;
    let q_applied = query_status(chain, &contract).await?;

    let gvk = q_applied.get("group_vk").and_then(|v| v.as_str()).unwrap_or("");
    if gvk != new_vk {
        return Err(format!("group_vk not rotated: {q_applied}").into());
    }
    if q_pending.get("pending") != Some(&json!(true)) {
        return Err(format!("invoke did not pending: {q_pending}").into());
    }
    if q_share1.get("group_vk").and_then(|v| v.as_str()) != Some(old_vk) {
        return Err(format!("vk changed before t: {q_share1}").into());
    }

    // Epoch 2: add usher (2-of-4). Incoming player cannot share until apply.
    let newer = "vk-newer";
    let invoke_roster = json!({
        "rotate_frost_pubkey": {
            "new_vk_commit": vk_commit(newer),
            "hkdf_commit": "hkdf-lab",
            "next_seats": ["desk", "steward", "spare", "usher"],
            "next_t": 2,
            "next_n": 4,
        }
    })
    .to_string();
    chain
        .execute_contract("validator", &contract, &invoke_roster, None)
        .await?;
    wait_for_blocks(chain, 1).await?;

    let usher_early = json!({
        "add_rotate_share": {
            "new_vk": newer,
            "hkdf_commit": "hkdf-lab",
            "blob": "S:0:z",
            "admin_id": "usher",
        }
    })
    .to_string();
    chain
        .execute_contract("validator", &contract, &usher_early, None)
        .await?;
    wait_for_blocks(chain, 1).await?;
    let q_usher = query_status(chain, &contract).await?;
    if q_usher.get("group_vk").and_then(|v| v.as_str()) != Some(new_vk) {
        return Err(format!("usher share moved vk: {q_usher}").into());
    }
    if q_usher.get("last_reason").and_then(|v| v.as_str()) != Some("not_admin") {
        return Err(format!("expected usher not_admin: {q_usher}").into());
    }
    if q_usher.get("n").and_then(|v| v.as_u64()) != Some(3) {
        return Err(format!("n changed before apply: {q_usher}").into());
    }

    let r1 = json!({
        "add_rotate_share": {
            "new_vk": newer,
            "hkdf_commit": "hkdf-lab",
            "blob": "S:1:a",
            "admin_id": "desk",
        }
    })
    .to_string();
    chain
        .execute_contract("validator", &contract, &r1, None)
        .await?;
    wait_for_blocks(chain, 1).await?;
    let r2 = json!({
        "add_rotate_share": {
            "new_vk": newer,
            "hkdf_commit": "hkdf-lab",
            "blob": "S:1:b",
            "admin_id": "steward",
        }
    })
    .to_string();
    chain
        .execute_contract("validator", &contract, &r2, None)
        .await?;
    wait_for_blocks(chain, 1).await?;
    let q_roster = query_status(chain, &contract).await?;
    let seats = q_roster
        .get("seats")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if q_roster.get("group_vk").and_then(|v| v.as_str()) != Some(newer) {
        return Err(format!("roster rotate did not change vk: {q_roster}").into());
    }
    if q_roster.get("n").and_then(|v| v.as_u64()) != Some(4) {
        return Err(format!("n not 4 after add: {q_roster}").into());
    }
    if !seats.iter().any(|s| s.as_str() == Some("usher")) {
        return Err(format!("usher missing from seats: {q_roster}").into());
    }

    let rotate_q = query_tx_json(chain, &rotate_hash).await?;
    let store_q = query_tx_json(chain, &store_hash).await?;
    let sender = rotate_q
        .pointer("/tx/body/messages/0/sender")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    if sender.is_empty() {
        return Err("rotate tx has no sender".into());
    }
    if !is_committee_rotate(&rotate_q, &contract, &sender) {
        return Err("rotate tx is not the old committee rotate_frost_pubkey for this contract".into());
    }
    if is_committee_rotate(&store_q, &contract, &sender) {
        return Err("an unrelated confirmed tx was accepted as the committee rotate".into());
    }
    let api = chain
        .primary_node()?
        .host_api_port
        .ok_or("lcd port missing")?;

    Ok(json!({
        "lcd": format!("http://127.0.0.1:{api}"),
        "rotate_tx": rotate_hash,
        "other_tx": store_hash,
        "sender": sender,
        "code_id": code_id,
        "contract": contract,
        "old_vk": old_vk,
        "new_vk": new_vk,
        "group_vk": q_roster.get("group_vk").cloned().unwrap_or(json!(gvk)),
        "n": q_roster.get("n"),
        "seats": seats,
        "query": {
            "after_instantiate": q0,
            "after_invoke_pending": q_pending,
            "after_share_1": q_share1,
            "after_share_t": q_applied,
            "usher_before_apply": q_usher,
            "after_add_member": q_roster,
        },
        "rotate": "MsgExecuteContract rotate_frost_pubkey",
        "authenticate_sudo": "forbidden_for_inbox",
        "roster": "add usher 2-of-4; incoming player not_admin until apply",
    }))
}

async fn spawn_terp() -> Result<(CosmosChain, Value), Box<dyn std::error::Error>> {
    let rt = Arc::new(DockerBackend::new(Default::default()).await?);
    let mut cfg = terp_chain_config();
    cfg.chain_id = "itl-frost-1".into();
    cfg.name = "itl-frost".into();
    cfg.genesis_style = GenesisStyle::Legacy;
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", "ict-itl-frost-itl-frost-1-val-0"])
        .status();
    let mut chain = CosmosChain::new(cfg, 1, 0, rt);
    let ctx = TestContext {
        test_name: "itl-frost".into(),
        network_id: "ict-itl-frost".into(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    wait_for_blocks(&chain, 2).await?;
    let info = chain_info_from_cosmos(&chain)?;
    let grpc = info.grpc_urls.clone();
    Ok((
        chain,
        json!({
            "live": true,
            "chain_id": "itl-frost-1",
            "grpc": grpc,
            "engine": "ict-rs+cw-orch",
        }),
    ))
}

fn inbox_e2e() -> Result<(), Box<dyn std::error::Error>> {
    // Host must already bind frost HTTP on 19091 (onboard-runtime or this process).
    let (c, b) = http_json(
        "POST",
        "/itl/frost/jobs",
        r#"{"pay_code":"pay-20-ink","scheme":"RedPallas","target":"Zcash"}"#,
    )?;
    if c != 200 {
        return Err(format!("create job {c} {b}").into());
    }
    let v: Value = serde_json::from_str(&b)?;
    let id = v["id"].as_str().ok_or("no id")?;
    let (c2, _) = http_json(
        "POST",
        &format!("/itl/frost/jobs/{id}/share"),
        r#"{"admin_id":"desk","blob":"S:0:a"}"#,
    )?;
    if c2 != 200 {
        return Err("share1".into());
    }
    let (c3, b3) = http_json(
        "POST",
        &format!("/itl/frost/jobs/{id}/broadcast"),
        r#"{"txid":"premature"}"#,
    )?;
    if c3 == 200 {
        return Err("broadcast before threshold".into());
    }
    let _ = b3;
    let (c4, _) = http_json(
        "POST",
        &format!("/itl/frost/jobs/{id}/share"),
        r#"{"admin_id":"steward","blob":"S:0:b"}"#,
    )?;
    if c4 != 200 {
        return Err("share2".into());
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    if let Err(e) = refuse_mock() {
        eprintln!("{e}");
        println!("{}", json!({ "live": false, "error": e.to_string() }));
        std::process::exit(2);
    }

    let (mut chain, mut meta) = match spawn_terp().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            println!(
                "{}",
                json!({ "live": false, "error": e.to_string(), "stage": "ict-rs-docker" })
            );
            std::process::exit(2);
        }
    };

    let rotate = match rotate_execute(&chain).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("rotate: {e}");
            if std::env::var("ICT_KEEP_CONTAINERS").ok().as_deref() != Some("1") {
                let _ = chain.stop().await;
            }
            println!(
                "{}",
                json!({
                    "live": false,
                    "error": e.to_string(),
                    "stage": "rotate-wasm",
                    "chain_id": "itl-frost-1",
                })
            );
            std::process::exit(2);
        }
    };

    if let Some(obj) = meta.as_object_mut() {
        for (k, v) in rotate.as_object().cloned().unwrap_or_default() {
            obj.insert(k, v);
        }
    }

    thread::sleep(Duration::from_millis(50));
    let _ = inbox_e2e();

    if std::env::var("ICT_KEEP_CONTAINERS").ok().as_deref() != Some("1") {
        let _ = chain.stop().await;
    }
    println!("{meta}");
    if meta.get("live") != Some(&json!(true)) {
        std::process::exit(2);
    }
}
