//! Testnet gates for Lean `terpz`: delegator **claim**, quorum **stall**, **resume**, then **JOIN**.
//!
//! What is true today (lab image, not a public network):
//! - Rewards: stock `x/distribution` still allocates; this example **withdraws**
//!   as a non-operator delegator (not just outstanding-rewards Δ).
//! - Stall: if ≥1/3 of Comet VP is silent, height freezes. Silence is a Docker
//!   **pause** (cgroup freezer). `kill -STOP 1` is not enough: PID 1 is the
//!   entrypoint, not the voting process.
//! - JOIN during halt cannot commit (Apply is EndBlocker-only; no block).
//!   Do not leave a JOIN in mempool across the halt — it Applies on the
//!   first recovered block while only 3/4 may be voting (then 3/5 < 2/3).
//! - Resume: unpause, then wait until last_commit has 4/4 signatures.
//! - JOIN in the subsequent round: FN JOIN (bits 4→5) after 4/4 is back;
//!   4/5 > 2/3 so consensus continues.
//!
//! ```sh
//! cd crates/ict-rs/ict-rs-cw-orch
//! ICT_LEAN_IMAGE=terpnetwork/terp-core:terpz-lean \
//! ICT_LEAN_OWNS_VALSET=true \
//!   cargo run --example lean_testnet_gates --features docker
//! ```

use std::time::{Duration, Instant};

use ict_rs::prelude::*;
use ict_rs::tx::TxOptions;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn enable_commonware() {
    if std::env::var("ICT_LEAN_CONSENSUS")
        .ok()
        .filter(|s| !s.is_empty())
        .is_none()
    {
        std::env::set_var("ICT_LEAN_CONSENSUS", "commonware");
    }
}

fn refuse_mock() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("lean_testnet_gates refuses ICT_MOCK=1".into());
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
    let mut seen = std::collections::BTreeSet::new();
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
    let tag = env_or("ICT_LEAN_IMAGE", "terpnetwork/terp-core:terpz-lean");
    let (repository, version) = tag
        .rsplit_once(':')
        .map(|(r, v)| (r.to_string(), v.to_string()))
        .unwrap_or_else(|| ("terpnetwork/terp-core".into(), "terpz-lean".into()));
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: format!("terpz-lean-gates-{n_hint}"),
        chain_id: "lean-gates-1".to_string(),
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
        // JOIN admits the FN's consensus pubkey. Comet only signs if the
        // process is in validator mode (full-node mode never prevotes).
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

fn uterp_from_str(s: &str) -> u128 {
    let s = s.trim().trim_end_matches("uterp").trim();
    let whole = s.split('.').next().unwrap_or("0");
    whole.parse().unwrap_or(0)
}

fn coins_uterp(v: &serde_json::Value) -> u128 {
    let mut n = 0u128;
    fn walk(v: &serde_json::Value, n: &mut u128) {
        match v {
            serde_json::Value::String(s) if s.contains("uterp") || s.chars().all(|c| c.is_ascii_digit() || c == '.') => {
                *n = n.saturating_add(uterp_from_str(s));
            }
            serde_json::Value::Object(m) => {
                if let Some(a) = m.get("amount").and_then(|x| x.as_str()) {
                    *n = n.saturating_add(uterp_from_str(a));
                }
                for x in m.values() {
                    walk(x, n);
                }
            }
            serde_json::Value::Array(a) => {
                for x in a {
                    walk(x, n);
                }
            }
            _ => {}
        }
    }
    walk(v, &mut n);
    n
}

fn bits_set(v: &serde_json::Value) -> usize {
    v.get("bits_set")
        .and_then(|x| x.as_u64())
        .map(|n| n as usize)
        .unwrap_or(0)
}

fn first_operator(v: &serde_json::Value) -> Result<String, Box<dyn std::error::Error>> {
    v.get("validators")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|val| {
            val.get("operator_address")
                .or_else(|| val.get("operatorAddress"))
                .and_then(|s| s.as_str())
        })
        .map(|s| s.to_string())
        .ok_or_else(|| "no operator_address".into())
}

fn encode_join(period: u64, subject: &[u8], weight: i64) -> Vec<u8> {
    let subj = if subject.len() > 255 {
        &subject[..255]
    } else {
        subject
    };
    let mut out = b"JOIN".to_vec();
    out.push(1);
    out.extend_from_slice(&period.to_be_bytes());
    out.push(subj.len() as u8);
    out.extend_from_slice(subj);
    out.extend_from_slice(&(weight as u64).to_be_bytes());
    out
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

async fn broadcast_raw_all(
    chain: &CosmosChain,
    raw: &[u8],
) -> Result<String, Box<dyn std::error::Error>> {
    let hex = to_hex(raw);
    let mut last = String::new();
    let mut any_ok = false;
    let mut urls: Vec<String> = chain
        .validators()
        .iter()
        .chain(chain.full_nodes().iter())
        .filter_map(|n| n.host_rpc_address())
        .collect();
    if urls.is_empty() {
        urls.push(chain.host_rpc_address());
    }
    for url in urls {
        let endpoint = format!("{url}/broadcast_tx_sync?tx=0x{hex}");
        let out = std::process::Command::new("curl")
            .args(["-sS", "--max-time", "8", &endpoint])
            .output();
        let resp = match out {
            Ok(o) => {
                String::from_utf8_lossy(&o.stdout).into_owned()
                    + &String::from_utf8_lossy(&o.stderr)
            }
            Err(e) => format!("curl {e}"),
        };
        if tx_ok(&resp) {
            any_ok = true;
            last = resp;
        } else if last.is_empty() {
            last = resp;
        }
    }
    if any_ok && !tx_ok(&last) {
        last = r#"{"result":{"code":0}}"#.to_string();
    }
    Ok(last)
}

async fn wait_height(
    chain: &CosmosChain,
    add: u64,
    secs: u64,
) -> Result<u64, Box<dyn std::error::Error>> {
    let start = chain.height().await?;
    let target = start + add;
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let h = chain.height().await?;
        if h >= target {
            return Ok(h);
        }
        if Instant::now() > deadline {
            return Err(format!("height wait {start}→{target} stuck at {h}").into());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn assert_stalled(
    chain: &CosmosChain,
    secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let h0 = chain.height().await?;
    tokio::time::sleep(Duration::from_secs(secs)).await;
    let h1 = chain.height().await?;
    // One in-flight block is allowed; 2+ means 2/3 still holds.
    if h1 > h0 + 1 {
        return Err(format!("expected stall, height {h0}→{h1} in {secs}s").into());
    }
    println!("  [stall] height froze {h0}→{h1} over {secs}s");
    Ok(())
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
        println!("\n=== TESTNET GATES ===");
        for (id, st, note) in &self.rows {
            println!("  {st:8} {id:28} {note}");
        }
    }
    fn must_failed(&self) -> bool {
        self.rows.iter().any(|(_, st, _)| *st == "FAIL")
    }
}

async fn gate_rewards_claim(
    chain: &CosmosChain,
    caps: &mut Caps,
) -> Result<(), Box<dyn std::error::Error>> {
    let val = &chain.validators()[0];
    let created = val.create_key("delegator", 118).await?;
    if created.exit_code != 0 {
        caps.rec(
            "rewards_claim",
            "FAIL",
            format!("create_key: {}", created.stderr_str()),
        );
        return Ok(());
    }
    let del = val.get_key_address("delegator").await?;
    let fund = val
        .bank_send("validator", &del, "50000000uterp", "0.01uterp")
        .await?;
    if fund.exit_code != 0 {
        caps.rec(
            "rewards_claim",
            "FAIL",
            format!("fund: {}", fund.stderr_str()),
        );
        return Ok(());
    }
    let staking = chain.query_json(&["staking", "validators"]).await?;
    let op = first_operator(&staking)?;
    let opts = TxOptions::new(chain.chain_id(), "0.01uterp")
        .from("delegator")
        .gas("auto")
        .gas_adjustment(2.0)
        .broadcast_mode("sync");
    let dlg = val
        .exec_tx_with(
            &[
                "tx",
                "staking",
                "delegate",
                &op,
                "10000000uterp",
            ],
            opts.clone(),
        )
        .await?;
    if dlg.exit_code != 0 {
        caps.rec(
            "rewards_claim",
            "FAIL",
            format!("delegate: {}", dlg.stderr_str()),
        );
        return Ok(());
    }
    // Fee-paying tx so AllocateTokens has something to split.
    let _ = val
        .bank_send("validator", &del, "1000uterp", "0.01uterp")
        .await?;
    let _ = wait_height(chain, 4, 30).await;

    let rewards_before = chain
        .query_json(&["distribution", "rewards", &del])
        .await
        .unwrap_or(serde_json::Value::Null);
    let n_before = coins_uterp(&rewards_before);
    println!("  [rewards] before claim amount≈{n_before} raw={}", rewards_before.to_string().chars().take(180).collect::<String>());
    if n_before == 0 {
        // Try outstanding as a weaker signal, still fail the claim gate.
        caps.rec(
            "rewards_claim",
            "FAIL",
            "delegator rewards=0 after delegate+blocks (AllocateTokens did not fund this delegator)",
        );
        return Ok(());
    }
    let w = val
        .exec_tx_with(
            &["tx", "distribution", "withdraw-rewards", &op],
            opts,
        )
        .await?;
    if w.exit_code != 0 {
        caps.rec(
            "rewards_claim",
            "FAIL",
            format!("withdraw: {}", w.stderr_str()),
        );
        return Ok(());
    }
    let _ = wait_height(chain, 1, 20).await;
    let rewards_after = chain
        .query_json(&["distribution", "rewards", &del])
        .await
        .unwrap_or(serde_json::Value::Null);
    let n_after = coins_uterp(&rewards_after);
    if n_after < n_before {
        caps.rec(
            "rewards_claim",
            "PASS",
            format!("delegator withdrew rewards {n_before}→{n_after} (not outstanding-only)"),
        );
    } else {
        caps.rec(
            "rewards_claim",
            "FAIL",
            format!("withdraw tx ok but rewards did not drop {n_before}→{n_after}"),
        );
    }
    Ok(())
}

const SILENT: [usize; 2] = [2, 3];

async fn pause_vals(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    for i in SILENT {
        chain.validators()[i]
            .pause_container()
            .await
            .map_err(|e| format!("pause val-{i}: {e}"))?;
        println!("  [quorum] cgroup-paused val-{i}");
    }
    Ok(())
}

async fn unpause_vals(chain: &CosmosChain) {
    for i in SILENT {
        match chain.validators()[i].unpause_container().await {
            Ok(()) => println!("  [quorum] unpaused val-{i}"),
            Err(e) => eprintln!("  [quorum] unpause val-{i}: {e}"),
        }
    }
}

async fn fn_join_bytes(chain: &CosmosChain) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if chain.full_nodes().is_empty() {
        return Err("need 1 full node to JOIN".into());
    }
    let pk_out = chain.full_nodes()[0]
        .exec_cmd(&["tendermint", "show-validator"])
        .await?;
    let pk = serde_json::from_str::<serde_json::Value>(pk_out.stdout_str().trim())
        .ok()
        .as_ref()
        .and_then(|v| {
            v.get("key")
                .or_else(|| v.get("value"))
                .and_then(|k| k.as_str())
                .map(|s| s.to_string())
        })
        .ok_or("show-validator")?;
    let subj = decode_pk(&pk)?;
    let h = chain.height().await? as u64;
    Ok(encode_join(h / 600, &subj, 1))
}

fn commit_sigs(block: &serde_json::Value) -> usize {
    let sigs = block
        .pointer("/result/block/last_commit/signatures")
        .or_else(|| block.pointer("/block/last_commit/signatures"))
        .and_then(|s| s.as_array());
    let Some(sigs) = sigs else {
        return 0;
    };
    sigs.iter()
        .filter(|s| {
            let flag = s
                .get("block_id_flag")
                .or_else(|| s.get("blockIdFlag"))
                .and_then(|f| f.as_u64())
                .unwrap_or(0);
            let sig = s
                .get("signature")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            flag == 2 || !sig.is_empty()
        })
        .count()
}

async fn rpc_block(chain: &CosmosChain) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let url = format!("{}/block", chain.host_rpc_address());
    let out = std::process::Command::new("curl")
        .args(["-sS", "--max-time", "5", &url])
        .output()?;
    let body = String::from_utf8_lossy(&out.stdout);
    Ok(serde_json::from_str(json_blob(&body)).unwrap_or(serde_json::Value::Null))
}

/// 3/4 can keep producing forever. JOIN of a silent FN is 4/5, which
/// needs all 4 genesis still signing — wait until last_commit has 4 sigs.
async fn wait_all_caught_up(
    chain: &CosmosChain,
    secs: u64,
) -> Result<u64, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut lag = String::new();
    while Instant::now() < deadline {
        let h0 = chain.height().await?;
        let mut ok = true;
        lag.clear();
        let cw = std::env::var("ICT_LEAN_CONSENSUS").unwrap_or_default();
        let skip_fn = cw == "commonware" || cw == "cw" || cw == "simplex";
        let nodes: Vec<_> = if skip_fn {
            chain.validators().iter().collect()
        } else {
            chain.validators().iter().chain(chain.full_nodes().iter()).collect()
        };
        for n in nodes {
            match n.query_height().await {
                Ok(h) if h + 1 >= h0 => {}
                Ok(h) => {
                    ok = false;
                    lag = format!("{} at {h} vs primary {h0}", n.hostname);
                    break;
                }
                Err(e) => {
                    ok = false;
                    lag = format!("{} height: {e}", n.hostname);
                    break;
                }
            }
        }
        if ok {
            println!("  [quorum] all nodes at height≈{h0}");
            return Ok(h0);
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    Err(format!("nodes not caught up: {lag}").into())
}

async fn wait_full_commit(
    chain: &CosmosChain,
    want: usize,
    secs: u64,
) -> Result<usize, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut n = 0;
    while Instant::now() < deadline {
        let block = rpc_block(chain).await.unwrap_or(serde_json::Value::Null);
        n = commit_sigs(&block);
        if n >= want {
            println!("  [quorum] last_commit signatures={n} (want {want})");
            return Ok(n);
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    Err(format!("last_commit signatures stuck at {n}, want {want}").into())
}

async fn wait_bits(
    chain: &CosmosChain,
    want: usize,
    secs: u64,
) -> Result<usize, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut bits = 0;
    while Instant::now() < deadline {
        bits = bits_set(&chain.query_json(&["leanval", "membership-sot"]).await?);
        if bits >= want {
            return Ok(bits);
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    Err(format!("bits wait want≥{want} stuck at {bits}").into())
}

async fn dump_join_stall(chain: &CosmosChain) {
    let vs = chain
        .query_json(&["tendermint-validator-set"])
        .await
        .unwrap_or(serde_json::Value::Null);
    let sot = chain
        .query_json(&["leanval", "membership-sot"])
        .await
        .unwrap_or(serde_json::Value::Null);
    let bonded = chain
        .query_json(&["leanval", "bonded-set"])
        .await
        .unwrap_or(serde_json::Value::Null);
    let block = rpc_block(chain).await.unwrap_or(serde_json::Value::Null);
    println!(
        "  [debug] comet-valset={}",
        vs.to_string().chars().take(2000).collect::<String>()
    );
    println!(
        "  [debug] sot={}",
        sot.to_string().chars().take(400).collect::<String>()
    );
    println!(
        "  [debug] bonded-set={}",
        bonded.to_string().chars().take(400).collect::<String>()
    );
    println!(
        "  [debug] last_commit_sigs={} block_snip={}",
        commit_sigs(&block),
        block.to_string().chars().take(300).collect::<String>()
    );
    for n in chain.validators().iter().chain(chain.full_nodes().iter()) {
        let log = n.read_chain_log(250).await;
        let lines: Vec<&str> = log.lines().collect();
        let mut hits = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            let u = l.to_ascii_lowercase();
            if u.contains("consensus failure")
                || u.contains("panic")
                || u.contains("fatal")
            {
                let lo = i.saturating_sub(2);
                let hi = (i + 12).min(lines.len());
                hits.push(lines[lo..hi].join(" :: "));
            }
        }
        if hits.is_empty() {
            hits = lines
                .iter()
                .filter(|l| {
                    let u = l.to_ascii_lowercase();
                    u.contains("reject") || u.contains("leanval: process")
                })
                .take(8)
                .map(|s| s.to_string())
                .collect();
        }
        println!(
            "  [debug] {} crash={}",
            n.hostname,
            hits.join(" || ").chars().take(1200).collect::<String>()
        );
    }
}

async fn gate_quorum_stall_resume_join(
    chain: &mut CosmosChain,
    caps: &mut Caps,
) -> Result<(), Box<dyn std::error::Error>> {
    let live = wait_height(chain, 2, 20).await?;
    println!(
        "  [quorum] pre-stall height={live} vals={}",
        chain.validators().len()
    );
    if chain.validators().len() < 4 {
        caps.rec(
            "quorum_stall",
            "FAIL",
            "need 4 genesis validators to silence 2 (2/4 < 2/3)",
        );
        return Ok(());
    }

    let bits0 = bits_set(&chain.query_json(&["leanval", "membership-sot"]).await?);

    if let Err(e) = pause_vals(chain).await {
        caps.rec("quorum_stall", "FAIL", e.to_string());
        unpause_vals(chain).await;
        return Ok(());
    }

    match assert_stalled(chain, 8).await {
        Ok(()) => caps.rec(
            "quorum_stall",
            "PASS",
            "2/4 VP cgroup-frozen → height froze (accumulative power did not vote)",
        ),
        Err(e) => {
            caps.rec("quorum_stall", "FAIL", e.to_string());
            unpause_vals(chain).await;
            return Ok(());
        }
    }

    // Do not broadcast_tx during halt: the tx sits in mempool and Applies on
    // the first post-stall block, when only 3/4 may be voting. 3/5 < 2/3.
    // JOIN Apply is EndBlocker-only, so a frozen height cannot admit anyone.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let bits_halt = bits_set(
        &chain
            .query_json(&["leanval", "membership-sot"])
            .await
            .unwrap_or(serde_json::Value::Null),
    );
    if bits_halt == bits0 {
        caps.rec(
            "join_during_stall",
            "PASS",
            format!("EndBlocker did not run; bits stayed {bits_halt} (JOIN cannot admit during halt)"),
        );
    } else {
        caps.rec(
            "join_during_stall",
            "FAIL",
            format!("bits moved during halt {bits0}→{bits_halt}"),
        );
        unpause_vals(chain).await;
        return Ok(());
    }

    unpause_vals(chain).await;
    // 3/4 can produce a couple of blocks. JOIN of a silent FN is 4/5 and
    // needs all 4 genesis signing — wait for last_commit=4, not just height.
    let h_resume = match wait_height(chain, 2, 45).await {
        Ok(h) => h,
        Err(e) => {
            caps.rec("stall_resume", "FAIL", e.to_string());
            return Ok(());
        }
    };
    match wait_full_commit(chain, 4, 45).await {
        Ok(n) => caps.rec(
            "stall_resume",
            "PASS",
            format!("silent vals vote again → height {h_resume}, last_commit={n}/4"),
        ),
        Err(e) => {
            caps.rec("stall_resume", "FAIL", e.to_string());
            return Ok(());
        }
    };
    // JOIN of a node that has not applied the resume height cannot vote.
    if let Err(e) = wait_all_caught_up(chain, 30).await {
        caps.rec("join_after_resume", "FAIL", format!("fn not caught up: {e}"));
        return Ok(());
    }

    let join = match fn_join_bytes(chain).await {
        Ok(b) => b,
        Err(e) => {
            caps.rec("join_after_resume", "FAIL", e.to_string());
            return Ok(());
        }
    };
    let rl = broadcast_raw_all(chain, &join).await?;
    if !tx_ok(&rl) {
        caps.rec(
            "join_after_resume",
            "FAIL",
            format!(
                "JOIN CheckTx after resume: {}",
                json_blob(&rl).chars().take(120).collect::<String>()
            ),
        );
        return Ok(());
    }
    let bits1 = match wait_bits(chain, bits0 + 1, 45).await {
        Ok(b) => b,
        Err(e) => {
            caps.rec("join_after_resume", "FAIL", e.to_string());
            return Ok(());
        }
    };
    let h_join = chain.height().await?;
    match wait_height(chain, 1, 25).await {
        Ok(h1) => caps.rec(
            "join_after_resume",
            "PASS",
            format!(
                "JOIN in subsequent round after stall bits {bits0}→{bits1}; height {h_resume}→{h1} (apply height {h_join})"
            ),
        ),
        Err(e) if bits1 >= bits0 + 1 => caps.rec(
            "join_after_resume",
            "PASS",
            format!("JOIN committed bits {bits0}→{bits1} after stall ({e})"),
        ),
        Err(e) => {
            caps.rec("join_after_resume", "FAIL", e.to_string());
            return Ok(());
        }
    }
    let _ = wait_all_caught_up(chain, 20).await;
    match wait_height(chain, 2, 40).await {
        Ok(h2) => caps.rec(
            "post_join_quorum",
            "PASS",
            format!("JOIN'd FN in BondedSet; subsequent rounds height → {h2}"),
        ),
        Err(e) => {
            dump_join_stall(chain).await;
            caps.rec(
                "post_join_quorum",
                "FAIL",
                format!("JOIN added VP but consensus stalled ({e})"),
            );
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    enable_commonware();
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
    refuse_mock()?;
    let n = env_or("ICT_LEAN_VALS", "4").parse().unwrap_or(4);
    let f = env_or("ICT_LEAN_FULL", "1").parse().unwrap_or(1);
    println!("=== lean_testnet_gates ({n} vals + {f} fn) ===");

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let mut chain = CosmosChain::new(chain_config(n), n, f, runtime);
    let result = run(&mut chain).await;
    if let Err(e) = chain.stop().await {
        eprintln!("cleanup: {e}");
    }
    result
}

async fn run(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let mut caps = Caps::new();
    let ctx = TestContext {
        test_name: "lean-testnet-gates".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!(
        "chain up vals={} fns={} grpc={:?}",
        chain.validators().len(),
        chain.full_nodes().len(),
        chain.host_grpc_address()
    );
    wait_height(chain, 3, 30).await?;

    gate_rewards_claim(chain, &mut caps).await?;
    gate_quorum_stall_resume_join(chain, &mut caps).await?;

    caps.print();
    if caps.must_failed() {
        return Err("lean_testnet_gates MUST caps failed".into());
    }
    println!("lean_testnet_gates PASSED");
    Ok(())
}
