//! SSLE load: sanity on a rotating 4-validator set, insanity on the mempool.
//!
//! Sanity: N consecutive committed blocks each carry one STWO SSLE whose
//! ticket is not the proposer, binds the block height, and is unique.
//! Insanity: Dummy DSTW, ticket==proposer, wrong kind/prover, bitflipped
//! proof, and replays are CheckTx-rejected from every node; the chain still
//! produces and still injects valid SSLE afterwards.
//!
//! ```sh
//! cd crates/ict-rs/ict-rs-cw-orch
//! ICT_LEAN_IMAGE=registry.terp.network/terp-core:terpz-lean \
//! ICT_LEAN_VALS=4 ICT_LEAN_SSLE_WINDOW=12 \
//!   cargo run --example lean_ssle_load --features docker
//! ```

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use ict_rs::prelude::*;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn refuse_mock() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("lean_ssle_load refuses ICT_MOCK=1".into());
    }
    Ok(())
}

fn collect_subjects(genesis: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut subjects = Vec::new();
    let mut push_pk = |pk: &serde_json::Value| {
        let key = pk
            .get("key")
            .or_else(|| pk.get("value"))
            .and_then(|k| k.as_str())
            .or_else(|| pk.as_str());
        if let Some(k) = key {
            if !k.is_empty() {
                subjects.push(serde_json::json!({"pubkey": k, "weight": 10}));
            }
        }
    };
    if let Some(vals) = genesis
        .pointer("/app_state/staking/validators")
        .and_then(|v| v.as_array())
    {
        for v in vals {
            if let Some(pk) = v.get("consensus_pubkey").or_else(|| v.get("pubkey")) {
                push_pk(pk);
            }
        }
    }
    if let Some(txs) = genesis
        .pointer("/app_state/genutil/gen_txs")
        .and_then(|v| v.as_array())
    {
        for tx in txs {
            let msgs = tx
                .pointer("/body/messages")
                .and_then(|m| m.as_array())
                .cloned()
                .unwrap_or_default();
            for msg in msgs {
                if let Some(pk) = msg.get("consensus_pubkey").or_else(|| msg.get("pubkey")) {
                    push_pk(pk);
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    subjects.retain(|s| {
        s.get("pubkey")
            .and_then(|p| p.as_str())
            .map(|pk| seen.insert(pk.to_string()))
            .unwrap_or(false)
    });
    subjects
}

fn modify_genesis(_cfg: &ChainConfig, raw: Vec<u8>) -> IctResult<Vec<u8>> {
    let mut genesis: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| IctError::Config(format!("parse genesis: {e}")))?;
    if let Some(params) = genesis.pointer_mut("/app_state/staking/params") {
        params["bond_denom"] = serde_json::json!("uterp");
    }
    if let Some(params) = genesis.pointer_mut("/app_state/mint/params") {
        params["mint_denom"] = serde_json::json!("uterp");
    }
    if let Some(block) = genesis.pointer_mut("/consensus_params/block") {
        block["max_gas"] = serde_json::json!("-1");
    }
    if let Some(block) = genesis.pointer_mut("/consensus/params/block") {
        block["max_gas"] = serde_json::json!("-1");
    }
    let subjects = collect_subjects(&genesis);
    if subjects.is_empty() {
        return Err(IctError::Config("no genesis_subjects from gentxs".into()));
    }
    genesis["app_state"]["leanval"] = serde_json::json!({
        "leanval_owns_valset": true,
        "genesis_subjects": subjects,
    });
    serde_json::to_vec(&genesis).map_err(|e| IctError::Config(format!("encode: {e}")))
}

fn chain_config(n_hint: usize) -> ChainConfig {
    let tag = env_or("ICT_LEAN_IMAGE", "registry.terp.network/terp-core:terpz-lean");
    let (repository, version) = tag
        .rsplit_once(':')
        .map(|(r, v)| (r.to_string(), v.to_string()))
        .unwrap_or_else(|| ("registry.terp.network/terp-core".into(), "terpz-lean".into()));
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: format!("terpz-lean-ssle-{n_hint}"),
        chain_id: "lean-ssle-1".to_string(),
        images: vec![DockerImage {
            repository,
            version,
            uid_gid: None,
        }],
        bin: env_or("ICT_LEAN_BIN", "terpz"),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0.01uterp".to_string(),
        gas_adjustment: 2.0,
        trusting_period: "112h".to_string(),
        block_time: "1s".to_string(),
        genesis: None,
        modify_genesis: Some(Box::new(modify_genesis)),
        pre_genesis: None,
        config_file_overrides: {
            let mut m = std::collections::HashMap::new();
            m.insert(
                "config/config.toml".to_string(),
                serde_json::json!({ "mode": "validator" }),
            );
            m
        },
        additional_start_args: vec!["--wasm.skip_wasmvm_version_check".to_string()],
        env: vec![(
            "LEANVAL_MEMBERSHIP_DIR".to_string(),
            "/terpd/lean-join".to_string(),
        )],
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn decode_pk(b64: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    fn d(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s: Vec<u8> = b64
        .trim()
        .bytes()
        .filter(|c| *c != b'=' && !c.is_ascii_whitespace())
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let a = d(s[i]).ok_or("b64")?;
        let b = if i + 1 < s.len() {
            d(s[i + 1]).ok_or("b64")?
        } else {
            0
        };
        let c = if i + 2 < s.len() {
            d(s[i + 2]).unwrap_or(0)
        } else {
            0
        };
        let e = if i + 3 < s.len() {
            d(s[i + 3]).unwrap_or(0)
        } else {
            0
        };
        out.push((a << 2) | (b >> 4));
        if i + 2 < s.len() {
            out.push((b << 4) | (c >> 2));
        }
        if i + 3 < s.len() {
            out.push((c << 6) | e);
        }
        i += 4;
    }
    Ok(out)
}

fn json_blob(resp: &str) -> &str {
    match (resp.find('{'), resp.rfind('}')) {
        (Some(i), Some(j)) if j > i => &resp[i..=j],
        _ => resp.trim(),
    }
}

fn tx_ok(resp: &str) -> bool {
    let v: serde_json::Value =
        serde_json::from_str(json_blob(resp)).unwrap_or(serde_json::Value::Null);
    v.pointer("/result/code")
        .or_else(|| v.get("code"))
        .and_then(|c| c.as_u64())
        .unwrap_or(99)
        == 0
}

fn decode_hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        let n = |c: u8| match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => 0,
        };
        out.push((n(b[i]) << 4) | n(b[i + 1]));
        i += 2;
    }
    out
}

fn rpc_get(url: &str) -> serde_json::Value {
    let out = std::process::Command::new("curl")
        .args(["-sS", "--max-time", "5", url])
        .output();
    match out {
        Ok(o) => {
            let body = String::from_utf8_lossy(&o.stdout);
            serde_json::from_str(json_blob(&body)).unwrap_or(serde_json::Value::Null)
        }
        Err(_) => serde_json::Value::Null,
    }
}

struct SsleView {
    height: i64,
    period: u64,
    ticket: Vec<u8>,
    proposer: Vec<u8>,
    proof: Vec<u8>,
    raw_len: usize,
}

fn parse_ssle(raw: &[u8]) -> Option<SsleView> {
    const HDR: usize = 4 + 8 + 8 + 32 + 1;
    if raw.len() < HDR || &raw[..4] != b"SSLE" {
        return None;
    }
    let period = u64::from_be_bytes(raw[4..12].try_into().ok()?);
    let height = i64::from_be_bytes(raw[12..20].try_into().ok()?);
    let ticket = raw[20..52].to_vec();
    let plen = raw[52] as usize;
    if 53 + plen > raw.len() {
        return None;
    }
    Some(SsleView {
        height,
        period,
        ticket,
        proposer: raw[53..53 + plen].to_vec(),
        proof: raw[53 + plen..].to_vec(),
        raw_len: raw.len(),
    })
}

fn encode_ssle(period: u64, height: i64, ticket: &[u8], proposer: &[u8], proof: &[u8]) -> Vec<u8> {
    let mut out = b"SSLE".to_vec();
    out.extend_from_slice(&period.to_be_bytes());
    out.extend_from_slice(&(height as u64).to_be_bytes());
    let mut t = [0u8; 32];
    let n = ticket.len().min(32);
    t[..n].copy_from_slice(&ticket[..n]);
    out.extend_from_slice(&t);
    let p = if proposer.len() > 255 {
        &proposer[..255]
    } else {
        proposer
    };
    out.push(p.len() as u8);
    out.extend_from_slice(p);
    out.extend_from_slice(proof);
    out
}

fn ssle_in_block(v: &serde_json::Value) -> Option<SsleView> {
    let txs = v
        .pointer("/result/block/data/txs")
        .or_else(|| v.pointer("/block/data/txs"))
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    for txb in txs {
        let s = txb.as_str().unwrap_or("");
        let Ok(raw) = decode_pk(s) else {
            continue;
        };
        if let Some(ssle) = parse_ssle(&raw) {
            return Some(ssle);
        }
    }
    None
}

fn block_height(v: &serde_json::Value) -> i64 {
    v.pointer("/result/block/header/height")
        .or_else(|| v.pointer("/block/header/height"))
        .and_then(|h| h.as_str().and_then(|s| s.parse().ok()).or_else(|| h.as_i64()))
        .unwrap_or(0)
}

fn block_proposer(v: &serde_json::Value) -> Vec<u8> {
    v.pointer("/result/block/header/proposer_address")
        .or_else(|| v.pointer("/block/header/proposer_address"))
        .and_then(|p| p.as_str())
        .map(decode_hex)
        .unwrap_or_default()
}

fn ssle_sane(ssle: &SsleView, hdr_h: i64, hdr_prop: &[u8]) -> Result<(), String> {
    if ssle.proof.windows(4).any(|w| w == b"DSTW") {
        return Err("Dummy DSTW in committed SSLE".into());
    }
    if ssle.proof.len() < 10 || &ssle.proof[..4] != b"STWO" {
        return Err("committed SSLE missing STWO magic".into());
    }
    if ssle.proof[4] != 2 || ssle.proof[5] != 5 {
        return Err("committed SSLE prover_id/curve_id != 2/5".into());
    }
    if &ssle.proof[6..10] != b"SSLE" {
        return Err("committed STWO kind != SSLE".into());
    }
    if ssle.ticket == ssle.proposer && !ssle.proposer.is_empty() {
        return Err("ticket equals proposer id".into());
    }
    if ssle.height != hdr_h {
        return Err(format!("SSLE height {} != block {hdr_h}", ssle.height));
    }
    let want_period = (hdr_h.max(0) as u64) / 600;
    if ssle.period != want_period {
        return Err(format!(
            "SSLE period {} != height/600 {want_period}",
            ssle.period
        ));
    }
    if !hdr_prop.is_empty() && !ssle.proposer.is_empty() && ssle.proposer != hdr_prop {
        return Err(format!(
            "SSLE proposer {} != header {}",
            to_hex(&ssle.proposer),
            to_hex(hdr_prop)
        ));
    }
    Ok(())
}

async fn wait_height(chain: &CosmosChain, delta: u64, secs: u64) -> Result<u64, Box<dyn std::error::Error>> {
    let start = chain.height().await?;
    let want = start.saturating_add(delta);
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let h = chain.height().await?;
        if h >= want {
            return Ok(h);
        }
        if Instant::now() > deadline {
            return Err(format!("height wait {start}→{want} stuck at {h}").into());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn broadcast_one(url: &str, raw: &[u8]) -> String {
    let hex = to_hex(raw);
    let endpoint = format!("{url}/broadcast_tx_sync?tx=0x{hex}");
    let out = std::process::Command::new("curl")
        .args(["-sS", "--max-time", "5", &endpoint])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(e) => format!("curl {e}"),
    }
}

struct Caps {
    rows: Vec<(&'static str, &'static str, String)>,
}

impl Caps {
    fn new() -> Self {
        Self { rows: Vec::new() }
    }
    fn rec(&mut self, id: &'static str, status: &'static str, note: impl Into<String>) {
        let note = note.into();
        println!("  [cap] {id:28} {status:8} {note}");
        self.rows.push((id, status, note));
    }
    fn print(&self) {
        println!("\n=== SSLE LOAD ===");
        for (id, st, note) in &self.rows {
            println!("  {st:8} {id:28} {note}");
        }
    }
    fn must_failed(&self) -> bool {
        self.rows.iter().any(|(_, st, _)| *st == "FAIL")
    }
}

fn insanity_payloads(replay: Option<&[u8]>) -> Vec<(&'static str, Vec<u8>)> {
    let p20 = vec![0x42u8; 20];
    let t32 = vec![0x42u8; 32];
    let mut stwo_wrong_kind = b"STWO".to_vec();
    stwo_wrong_kind.extend_from_slice(&[2, 5]);
    stwo_wrong_kind.extend_from_slice(b"FOLD");
    stwo_wrong_kind.extend_from_slice(&[0u8; 96]);
    let mut stwo_wrong_prover = b"STWO".to_vec();
    stwo_wrong_prover.extend_from_slice(&[1, 5]);
    stwo_wrong_prover.extend_from_slice(b"SSLE");
    stwo_wrong_prover.extend_from_slice(&[0u8; 48]);
    let mut out = vec![
        ("prefix-only", b"SSLE".to_vec()),
        (
            "dummy-dstw",
            encode_ssle(0, 1, &t32, &p20, b"DSTW"),
        ),
        (
            "ticket-eq-proposer",
            encode_ssle(0, 1, &t32, &t32, b"DSTW"),
        ),
        (
            "wrong-kind-fold",
            encode_ssle(0, 1, &t32, &p20, &stwo_wrong_kind),
        ),
        (
            "wrong-prover-id",
            encode_ssle(0, 1, &t32, &p20, &stwo_wrong_prover),
        ),
        (
            "bitflip-noise",
            encode_ssle(0, 1, &t32, &p20, &(0..200).map(|i| (i ^ 0xa5) as u8).collect::<Vec<_>>()),
        ),
    ];
    if let Some(raw) = replay {
        let mut flipped = raw.to_vec();
        if flipped.len() > 80 {
            let i = flipped.len() / 2;
            flipped[i] ^= 0xff;
        }
        out.push(("replay-bitflip", flipped));
    }
    out
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
    refuse_mock()?;
    let n = env_or("ICT_LEAN_VALS", "4").parse().unwrap_or(4);
    let window: usize = env_or("ICT_LEAN_SSLE_WINDOW", "12").parse().unwrap_or(12);
    println!("=== lean_ssle_load ({n} vals, window={window}) ===");

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let mut chain = CosmosChain::new(chain_config(n), n, 0, runtime);
    let result = run(&mut chain, window).await;
    if let Err(e) = chain.stop().await {
        eprintln!("cleanup: {e}");
    }
    result
}

async fn run(chain: &mut CosmosChain, window: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut caps = Caps::new();
    let ctx = TestContext {
        test_name: "lean-ssle-load".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!(
        "chain up vals={} grpc={:?}",
        chain.validators().len(),
        chain.host_grpc_address()
    );

    let h0 = wait_height(chain, 3, 40).await?;
    println!("  [ssle] start window at height {h0}");

    let rpc = chain.host_rpc_address();
    let mut tickets = BTreeSet::new();
    let mut proposers = BTreeSet::new();
    let mut last_raw: Option<Vec<u8>> = None;
    let mut ok_n = 0usize;
    let mut first_err: Option<String> = None;
    let deadline = Instant::now() + Duration::from_secs(window as u64 * 4 + 20);
    let mut seen_h = BTreeSet::new();
    while seen_h.len() < window && Instant::now() < deadline {
        let v = rpc_get(&format!("{rpc}/block"));
        let hdr_h = block_height(&v);
        if hdr_h <= 0 || !seen_h.insert(hdr_h) {
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        let hdr_prop = block_proposer(&v);
        match ssle_in_block(&v) {
            Some(ssle) => match ssle_sane(&ssle, hdr_h, &hdr_prop) {
                Ok(()) => {
                    tickets.insert(ssle.ticket.clone());
                    if !ssle.proposer.is_empty() {
                        proposers.insert(ssle.proposer.clone());
                    }
                    last_raw = Some({
                        let txs = v
                            .pointer("/result/block/data/txs")
                            .and_then(|t| t.as_array())
                            .cloned()
                            .unwrap_or_default();
                        txs.iter()
                            .filter_map(|t| t.as_str().and_then(|s| decode_pk(s).ok()))
                            .find(|r| r.starts_with(b"SSLE"))
                            .unwrap_or_default()
                    });
                    ok_n += 1;
                    println!(
                        "  [ssle] h={hdr_h} ticket={} proposer={} bytes={}",
                        to_hex(&ssle.ticket[..8.min(ssle.ticket.len())]),
                        to_hex(&ssle.proposer[..8.min(ssle.proposer.len())]),
                        ssle.raw_len
                    );
                }
                Err(e) => {
                    first_err.get_or_insert(format!("h={hdr_h}: {e}"));
                }
            },
            None => {
                first_err.get_or_insert(format!("h={hdr_h}: no SSLE in block"));
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    if ok_n >= window && first_err.is_none() {
        caps.rec(
            "ssle_window",
            "PASS",
            format!("{ok_n}/{window} consecutive blocks carry STWO SSLE"),
        );
    } else {
        caps.rec(
            "ssle_window",
            "FAIL",
            format!(
                "sane={ok_n}/{window} err={}",
                first_err.unwrap_or_else(|| "short window".into())
            ),
        );
    }
    if tickets.len() == ok_n && ok_n > 1 {
        caps.rec(
            "ssle_unique_tickets",
            "PASS",
            format!("{} distinct tickets across {ok_n} heights", tickets.len()),
        );
    } else {
        caps.rec(
            "ssle_unique_tickets",
            "FAIL",
            format!("tickets={} blocks={ok_n} (hide-until-block must not reuse)", tickets.len()),
        );
    }
    if proposers.len() >= 2 {
        caps.rec(
            "ssle_proposer_rotation",
            "PASS",
            format!("{} distinct SSLE proposers under load", proposers.len()),
        );
    } else if ok_n >= 4 {
        caps.rec(
            "ssle_proposer_rotation",
            "FAIL",
            format!("only {} proposer(s) in {ok_n} blocks", proposers.len()),
        );
    } else {
        caps.rec(
            "ssle_proposer_rotation",
            "PASS",
            format!("window too short to rotate ({ok_n} blocks, {} proposers)", proposers.len()),
        );
    }

    let urls: Vec<String> = chain
        .validators()
        .iter()
        .filter_map(|n| n.host_rpc_address())
        .collect();
    let payloads = insanity_payloads(last_raw.as_deref());
    let h_before = chain.height().await?;
    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for _round in 0..2 {
        for url in &urls {
            for (_name, raw) in &payloads {
                let resp = broadcast_one(url, raw);
                if tx_ok(&resp) {
                    accepted += 1;
                } else {
                    rejected += 1;
                }
            }
        }
    }
    if accepted == 0 && rejected > 0 {
        caps.rec(
            "ssle_mempool_flood",
            "PASS",
            format!("rejected {rejected} insane SSLE (dummy/ticket=proposer/wrong-kind/bitflip) across {} nodes × 2 rounds", urls.len()),
        );
    } else {
        caps.rec(
            "ssle_mempool_flood",
            "FAIL",
            format!("accepted={accepted} rejected={rejected}"),
        );
    }

    match wait_height(chain, 3, 30).await {
        Ok(h) => caps.rec(
            "ssle_liveness_after_flood",
            "PASS",
            format!("height {h_before}→{h} after {rejected} rejected SSLE"),
        ),
        Err(e) => caps.rec("ssle_liveness_after_flood", "FAIL", e.to_string()),
    }

    let v = rpc_get(&format!("{rpc}/block"));
    let hdr_h = block_height(&v);
    let hdr_prop = block_proposer(&v);
    match ssle_in_block(&v) {
        Some(ssle) => match ssle_sane(&ssle, hdr_h, &hdr_prop) {
            Ok(()) => caps.rec(
                "ssle_post_flood_inject",
                "PASS",
                format!("h={hdr_h} still injects STWO SSLE bytes={}", ssle.raw_len),
            ),
            Err(e) => caps.rec("ssle_post_flood_inject", "FAIL", e),
        },
        None => caps.rec("ssle_post_flood_inject", "FAIL", "no SSLE after flood"),
    }

    caps.print();
    if caps.must_failed() {
        return Err("lean_ssle_load MUST caps failed".into());
    }
    println!("lean_ssle_load PASSED");
    Ok(())
}
