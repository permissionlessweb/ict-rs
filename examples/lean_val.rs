//! Pair B: BondedSet + LNPR wrap HMVE — retuned to feat/lean-consensus @ e640367.
//!
//! Names match `x/leanval` (`QueryBondedSet`, `EncodeLNPR`, `WrapPrepareProposal`,
//! `WrapProcessProposal`, `RequireLNPR`, `PeriodFromHeight = height / 600`).
//! See `LEAN_VAL_HANDSHAKE.md`.
//!
//! ```sh
//! cargo run --example lean_val --features docker
//! ICT_LEAN_CASES=power0,bonded,reject,hmve,split cargo run --example lean_val --features docker
//! ```

use ict_rs::cli::parse_query_response;
use ict_rs::prelude::*;

/// types.PrefixHMVE / PrefixLNPR
const PREFIX_HMVE: &[u8] = b"HMVE"; // 0x48 0x4D 0x56 0x45
const PREFIX_LNPR: &[u8] = b"LNPR"; // 0x4C 0x4E 0x50 0x52
const LNPR_VERSION: u8 = 1;
/// types.BlocksPerPeriod
const BLOCKS_PER_PERIOD: i64 = 600;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn upgrade_repo() -> String {
    env_or("ICT_UPGRADE_REPO", "terpnetwork/terp-core")
}

fn image_version() -> String {
    env_or("ICT_LEAN_IMAGE", env_or("ICT_UPGRADE_TO", "local"))
}

fn enabled_cases() -> Vec<String> {
    env_or("ICT_LEAN_CASES", "power0,bonded,reject,hmve,split")
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn case_on(enabled: &[String], name: &str) -> bool {
    enabled.iter().any(|c| c == name || c == "all")
}

/// types.PeriodFromHeight
fn period_from_height(height: i64) -> u64 {
    if height < 0 {
        return 0;
    }
    (height / BLOCKS_PER_PERIOD) as u64
}

fn has_prefix(tx: &[u8], p: &[u8]) -> bool {
    tx.starts_with(p)
}

/// If txs[0] is HMVE, LNPR lives at txs[1]; else txs[0].
fn expected_lnpr_index(txs: &[Vec<u8>]) -> usize {
    if !txs.is_empty() && has_prefix(&txs[0], PREFIX_HMVE) {
        1
    } else {
        0
    }
}

struct SubjectProof {
    subject: Vec<u8>,
    weight: i64,
    proof: Vec<u8>,
}

struct LnprBlob {
    period: u64,
    subjects: Vec<SubjectProof>,
}

fn put_u64(v: u64) -> [u8; 8] {
    v.to_be_bytes()
}

fn put_i64(v: i64) -> [u8; 8] {
    v.to_be_bytes()
}

/// types.EncodeLNPR — not a user Msg; proposal inject only.
#[allow(dead_code)]
fn encode_lnpr(b: &LnprBlob) -> Vec<u8> {
    let mut out = PREFIX_LNPR.to_vec();
    out.push(LNPR_VERSION);
    out.extend_from_slice(&put_u64(b.period));
    let n = b.subjects.len() as u16;
    out.extend_from_slice(&n.to_be_bytes());
    for s in &b.subjects {
        let subj = if s.subject.len() > 255 {
            &s.subject[..255]
        } else {
            &s.subject
        };
        out.push(subj.len() as u8);
        out.extend_from_slice(subj);
        out.extend_from_slice(&put_i64(s.weight));
        let pl = s.proof.len() as u32;
        out.extend_from_slice(&pl.to_be_bytes());
        out.extend_from_slice(&s.proof);
    }
    out
}

/// types.DecodeLNPR
fn decode_lnpr(tx: &[u8]) -> Option<LnprBlob> {
    if !has_prefix(tx, PREFIX_LNPR) {
        return None;
    }
    let mut p = &tx[PREFIX_LNPR.len()..];
    if p.len() < 1 + 8 + 2 || p[0] != LNPR_VERSION {
        return None;
    }
    p = &p[1..];
    let period = u64::from_be_bytes(p[..8].try_into().ok()?);
    p = &p[8..];
    let n = u16::from_be_bytes(p[..2].try_into().ok()?) as usize;
    p = &p[2..];
    let mut subjects = Vec::with_capacity(n);
    for _ in 0..n {
        if p.is_empty() {
            return None;
        }
        let al = p[0] as usize;
        p = &p[1..];
        if p.len() < al + 8 + 4 {
            return None;
        }
        let subject = p[..al].to_vec();
        p = &p[al..];
        let weight = i64::from_be_bytes(p[..8].try_into().ok()?);
        p = &p[8..];
        let pl = u32::from_be_bytes(p[..4].try_into().ok()?) as usize;
        p = &p[4..];
        if p.len() < pl {
            return None;
        }
        let proof = p[..pl].to_vec();
        p = &p[pl..];
        subjects.push(SubjectProof {
            subject,
            weight,
            proof,
        });
    }
    Some(LnprBlob { period, subjects })
}

fn find_lnpr(txs: &[Vec<u8>]) -> Option<(LnprBlob, usize)> {
    for (i, tx) in txs.iter().enumerate() {
        if let Some(b) = decode_lnpr(tx) {
            return Some((b, i));
        }
    }
    None
}

fn modify_lean_genesis(_cfg: &ChainConfig, raw: Vec<u8>) -> IctResult<Vec<u8>> {
    let mut genesis: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| IctError::Config(format!("parse genesis: {e}")))?;

    if let Some(params) = genesis.pointer_mut("/app_state/staking/params") {
        params["bond_denom"] = serde_json::json!("uterp");
    }
    if let Some(params) = genesis.pointer_mut("/app_state/mint/params") {
        params["mint_denom"] = serde_json::json!("uterp");
    }
    if let Some(params) = genesis.pointer_mut("/app_state/gov/params") {
        params["min_deposit"] = serde_json::json!([{"denom": "uterp", "amount": "10000000"}]);
        params["voting_period"] = serde_json::json!("15s");
        params["expedited_voting_period"] = serde_json::json!("5s");
        params["max_deposit_period"] = serde_json::json!("15s");
    }

    // Keeper field is RequireLNPR (default true). Genesis key when module state lands.
    let require = env_or("ICT_LEAN_REQUIRE_LNPR", "1");
    let require_bool = require != "0" && !require.eq_ignore_ascii_case("false");
    if genesis.pointer("/app_state/leanval").is_none() {
        if let Some(app) = genesis.get_mut("app_state").and_then(|v| v.as_object_mut()) {
            app.insert(
                "leanval".to_string(),
                serde_json::json!({
                    "params": { "RequireLNPR": require_bool },
                    "bonded_set": []
                }),
            );
        }
    } else if let Some(params) = genesis.pointer_mut("/app_state/leanval/params") {
        params["RequireLNPR"] = serde_json::json!(require_bool);
    }

    serde_json::to_vec(&genesis).map_err(|e| IctError::Config(format!("encode genesis: {e}")))
}

fn terp_lean_config(version: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: "120u-leanval-1".to_string(),
        images: vec![DockerImage {
            repository: upgrade_repo(),
            version: version.to_string(),
            uid_gid: None,
        }],
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
        modify_genesis: Some(Box::new(modify_lean_genesis)),
        pre_genesis: None,
        config_file_overrides: std::collections::HashMap::new(),
        additional_start_args: vec!["--wasm.skip_wasmvm_version_check".to_string()],
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

async fn query_json(
    chain: &CosmosChain,
    args: &[&str],
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut cmd = vec!["query"];
    cmd.extend_from_slice(args);
    cmd.extend_from_slice(&["--output", "json"]);
    let out = chain.exec(&cmd, &[]).await?;
    if out.exit_code != 0 {
        return Err(format!("query {:?} failed: {}", args, out.stderr_str()).into());
    }
    Ok(parse_query_response(&out)?)
}

async fn query_first(
    chain: &CosmosChain,
    attempts: &[&[&str]],
) -> Result<serde_json::Value, String> {
    let mut last = String::new();
    for args in attempts {
        match query_json(chain, args).await {
            Ok(v) if !v.is_null() => return Ok(v),
            Ok(_) => last = format!("{:?} empty", args),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

fn json_int(v: &serde_json::Value, keys: &[&str]) -> Option<i128> {
    let mut cur = v;
    for k in keys {
        cur = cur.get(*k)?;
    }
    if let Some(n) = cur.as_i64() {
        return Some(n as i128);
    }
    if let Some(n) = cur.as_u64() {
        return Some(n as i128);
    }
    cur.as_str()?.parse().ok()
}

/// QueryBondedSet JSON: subjects[] with subject, weight, has_proof.
fn subject_powers(v: &serde_json::Value) -> Vec<serde_json::Value> {
    for key in ["subjects", "bonded_set", "SubjectPower", "set"] {
        if let Some(a) = v.get(key).and_then(|x| x.as_array()) {
            return a.clone();
        }
    }
    v.as_array().cloned().unwrap_or_default()
}

fn row_weight(row: &serde_json::Value) -> i128 {
    json_int(row, &["weight"])
        .or_else(|| json_int(row, &["Weight"]))
        .unwrap_or(0)
}

fn row_has_proof(row: &serde_json::Value) -> bool {
    row.get("has_proof")
        .or_else(|| row.get("HasProof"))
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

fn row_subject(row: &serde_json::Value) -> String {
    for k in ["subject", "Subject"] {
        if let Some(s) = row.get(k).and_then(|x| x.as_str()) {
            return s.to_string();
        }
    }
    String::new()
}

fn decode_tx_bytes(raw: &str) -> Option<Vec<u8>> {
    let s = raw.trim();
    if s.len() >= 8 && s.len() % 2 == 0 && s.as_bytes().iter().all(|c| c.is_ascii_hexdigit()) {
        let mut out = Vec::with_capacity(s.len() / 2);
        for i in 0..(s.len() / 2) {
            out.push(u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?);
        }
        return Some(out);
    }
    decode_std_b64(s)
}

/// Minimal std base64 (no extra crate).
fn decode_std_b64(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            break;
        }
        let a = val(bytes[i])?;
        let b = *bytes.get(i + 1)?;
        if b == b'=' {
            break;
        }
        let b = val(b)?;
        out.push((a << 2) | (b >> 4));
        let c = *bytes.get(i + 2)?;
        if c == b'=' {
            break;
        }
        let c = val(c)?;
        out.push(((b & 0x0f) << 4) | (c >> 2));
        let d = *bytes.get(i + 3)?;
        if d == b'=' {
            break;
        }
        let d = val(d)?;
        out.push(((c & 0x03) << 6) | d);
        i += 4;
    }
    Some(out)
}

fn extract_raw_txs(block: &serde_json::Value) -> Vec<String> {
    let arr = block
        .pointer("/block/data/txs")
        .or_else(|| block.pointer("/data/txs"))
        .or_else(|| block.get("txs"))
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    arr.iter()
        .filter_map(|t| t.as_str().map(|s| s.to_string()))
        .collect()
}

fn txs_from_block(block: &serde_json::Value) -> Vec<Vec<u8>> {
    extract_raw_txs(block)
        .iter()
        .filter_map(|s| decode_tx_bytes(s))
        .collect()
}

async fn load_block_txs(chain: &CosmosChain, h: u64) -> Result<Vec<Vec<u8>>, String> {
    let hs = h.to_string();
    let block = query_first(
        chain,
        &[&["block", "--type", "height", &hs], &["block", &hs]],
    )
    .await?;
    Ok(txs_from_block(&block))
}

async fn query_bonded_set(
    chain: &CosmosChain,
    period: u64,
) -> Result<serde_json::Value, String> {
    let p = period.to_string();
    // Planned CLI for QueryBondedSet (no gRPC in e640367).
    query_first(
        chain,
        &[
            &["leanval", "bonded-set", &p],
            &["leanval", "QueryBondedSet", &p],
            &["leanval", "bonded-set"],
        ],
    )
    .await
}

async fn validator_opers(chain: &CosmosChain) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let vals = query_json(chain, &["staking", "validators"])
        .await
        .unwrap_or(serde_json::json!({}));
    let list = vals
        .get("validators")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|v| {
            v.get("operator_address")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        })
        .collect())
}

/// Case 1: no AcceptProof ⇒ QueryBondedSet Weight==0 (not LastValidatorPowers).
async fn case_power0(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- case power0: no period proof ⇒ Weight 0 (QueryBondedSet) ---");
    let h = chain.height().await? as i64;
    let p = period_from_height(h);
    println!("  height={h} PeriodFromHeight={p} (h/{BLOCKS_PER_PERIOD})");

    let opers = validator_opers(chain).await?;
    if let Some(oper) = opers.first() {
        match query_first(
            chain,
            &[
                &["staking", "last-validator-power", oper],
                &["staking", "validator", oper],
            ],
        )
        .await
        {
            Ok(_) => println!(
                "  staking LastValidatorPowers/validator exists for {oper} — must not confer Comet power"
            ),
            Err(e) => println!("  QUERY_MISSING staking last-validator-power: {e}"),
        }
    }

    match query_bonded_set(chain, p).await {
        Ok(v) => {
            for row in subject_powers(&v) {
                let w = row_weight(&row);
                let hp = row_has_proof(&row);
                println!(
                    "  subject={} weight={w} has_proof={hp}",
                    row_subject(&row)
                );
                if !hp && w != 0 {
                    return Err(
                        "INVARIANT: QueryBondedSet missing proof must be Weight==0 (see TestBondedSetMissingProofWeightZero)"
                            .into(),
                    );
                }
            }
        }
        Err(e) => {
            println!("  QUERY_MISSING QueryBondedSet: {e}");
            println!("  handshake: no gRPC yet — keeper QueryBondedSet(period); CLI planned as `query leanval bonded-set [period]`");
        }
    }
    Ok(())
}

/// Case 2: ProcessInjectedLNPR / ApplyLNPR → QueryBondedSet includes Weight>0.
async fn case_bonded(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- case bonded: EncodeLNPR apply → QueryBondedSet Weight ---");
    let h = chain.height().await? as i64;
    let p = period_from_height(h);

    // Not a user Msg. Inspect injected LNPR on the last block + QueryBondedSet.
    match load_block_txs(chain, h as u64).await {
        Ok(txs) => {
            if let Some((blob, idx)) = find_lnpr(&txs) {
                println!(
                    "  DecodeLNPR at txs[{idx}] period={} nsubj={} (want period {p})",
                    blob.period,
                    blob.subjects.len()
                );
                if blob.period != p {
                    return Err(format!(
                        "INVARIANT: LNPR period {} != PeriodFromHeight({h})={p}",
                        blob.period
                    )
                    .into());
                }
            } else {
                println!("  no DecodeLNPR on height {h} (image may lack WrapPrepareProposal)");
            }
        }
        Err(e) => println!("  QUERY_MISSING block: {e}"),
    }

    match query_bonded_set(chain, p).await {
        Ok(v) => {
            let rows = subject_powers(&v);
            let with_w: Vec<_> = rows.iter().filter(|s| row_weight(s) > 0).collect();
            println!(
                "  QueryBondedSet({p}) subjects={} with_weight={}",
                rows.len(),
                with_w.len()
            );
            if with_w.is_empty() {
                return Err(
                    "INVARIANT: after accepted LNPR/ApplyLNPR, QueryBondedSet must include Weight>0"
                        .into(),
                );
            }
        }
        Err(e) => {
            println!("  QUERY_MISSING QueryBondedSet: {e}");
        }
    }
    Ok(())
}

/// Case 3: RequireLNPR (default true) + omitted LNPR ⇒ ProcessProposal REJECT.
async fn case_reject(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- case reject: RequireLNPR + omitted LNPR → REJECT ---");
    let h = chain.height().await? as i64;
    let p = period_from_height(h);
    println!("  height={h} period={p} RequireLNPR default=true");

    match load_block_txs(chain, h as u64).await {
        Ok(txs) => match find_lnpr(&txs) {
            Some((_, idx)) => {
                let want = expected_lnpr_index(&txs);
                println!("  finalized block has LNPR at txs[{idx}] (expected index {want})");
                if idx != want {
                    return Err(format!(
                        "INVARIANT: LNPR at wrong index {idx} want {want} (HMVE-first compose)"
                    )
                    .into());
                }
            }
            None => {
                if !txs.is_empty() {
                    return Err(
                        "INVARIANT: RequireLNPR — finalized non-empty block without LNPR (WrapProcessProposal must REJECT)"
                            .into(),
                    );
                }
                println!("  empty txs — cannot observe reject vs omit");
            }
        },
        Err(e) => println!("  QUERY_MISSING block: {e}"),
    }
    Ok(())
}

/// Case 4: txs[0]==HMVE ⇒ LNPR is txs[1]; WrapPrepareProposal must not steal slot 0.
async fn case_hmve(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- case hmve: PrefixHMVE first; LNPR at txs[1] ---");
    let h = chain.height().await? as i64;
    match load_block_txs(chain, h as u64).await {
        Ok(txs) => {
            if txs.is_empty() {
                println!("  height={h} no txs — skip");
                return Ok(());
            }
            let t0_hmve = has_prefix(&txs[0], PREFIX_HMVE);
            let t0_lnpr = has_prefix(&txs[0], PREFIX_LNPR);
            println!(
                "  height={h} ntxs={} txs[0]_HMVE={t0_hmve} txs[0]_LNPR={t0_lnpr}",
                txs.len()
            );
            if t0_hmve {
                if txs.len() < 2 || !has_prefix(&txs[1], PREFIX_LNPR) {
                    return Err(
                        "INVARIANT: txs[0] is HMVE so LNPR must be txs[1] (WrapPrepareProposal)"
                            .into(),
                    );
                }
            } else if t0_lnpr {
                println!("  no HMVE this height — LNPR correctly occupies txs[0]");
            } else {
                println!("  txs[0] neither HMVE nor LNPR (image may not wrap yet)");
            }
        }
        Err(e) => println!("  QUERY_MISSING block: {e}"),
    }
    Ok(())
}

/// Case 5: two vals; only the prover (HasProof) has Weight>0.
async fn case_split(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- case split: two vals, one AcceptProof → only prover Weight ---");
    let h = chain.height().await? as i64;
    let p = period_from_height(h);
    let opers = validator_opers(chain).await?;
    if opers.len() < 2 {
        return Err("need ≥2 validators for split-power case".into());
    }

    match query_bonded_set(chain, p).await {
        Ok(v) => {
            let rows = subject_powers(&v);
            let powered: Vec<_> = rows.iter().filter(|s| row_weight(s) > 0).collect();
            let proved: Vec<_> = rows.iter().filter(|s| row_has_proof(s)).collect();
            println!(
                "  staking_vals={} QueryBondedSet({p}) rows={} Weight>0={} HasProof={}",
                opers.len(),
                rows.len(),
                powered.len(),
                proved.len()
            );
            for row in &rows {
                if row_weight(row) > 0 && !row_has_proof(row) {
                    return Err("INVARIANT: Weight>0 requires HasProof (AcceptProof)".into());
                }
            }
            if let Some((blob, _)) = load_block_txs(chain, h as u64)
                .await
                .ok()
                .and_then(|txs| find_lnpr(&txs))
            {
                let n_pos = blob.subjects.iter().filter(|s| s.weight > 0).count();
                println!("  EncodeLNPR subjects={} with_weight={n_pos}", blob.subjects.len());
            }
        }
        Err(e) => println!("  QUERY_MISSING QueryBondedSet: {e}"),
    }
    Ok(())
}

async fn run_test(
    chain: &mut CosmosChain,
    num_validators: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let cases = enabled_cases();
    println!("\n--- Initializing leanval chain ---");
    let ctx = TestContext {
        test_name: "lean-val-lnpr".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!(
        "Chain started. validators={num_validators} cases={} period=height/{BLOCKS_PER_PERIOD}",
        cases.join(",")
    );

    wait_for_blocks(chain, 2).await.ok();

    if case_on(&cases, "power0") {
        case_power0(chain).await?;
    }
    if case_on(&cases, "bonded") {
        case_bonded(chain).await?;
    }
    if case_on(&cases, "reject") {
        case_reject(chain).await?;
    }
    if case_on(&cases, "hmve") {
        case_hmve(chain).await?;
    }
    if case_on(&cases, "split") {
        case_split(chain).await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let ver = image_version();
    println!(
        "=== leanval QueryBondedSet / EncodeLNPR / Wrap*Proposal ({}) ===\n",
        upgrade_repo()
    );
    println!("retune: feat/lean-consensus @ e640367  handshake: LEAN_VAL_HANDSHAKE.md\n");

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    println!("Docker runtime connected.");

    let config = terp_lean_config(&ver);
    let num_validators = 2;
    let mut chain = CosmosChain::new(config, num_validators, 0, runtime);

    println!(
        "Chain: {} image {}:{} validators={num_validators}",
        chain.chain_id(),
        upgrade_repo(),
        ver
    );

    let result = run_test(&mut chain, num_validators).await;

    println!("\n--- Shutdown ---");
    if let Err(e) = chain.stop().await {
        eprintln!("Warning: cleanup error: {e}");
    }

    match result {
        Ok(()) => {
            println!("leanval cases completed (QUERY_MISSING = no gRPC yet; use QueryBondedSet)");
            Ok(())
        }
        Err(e) => {
            eprintln!("Test FAILED: {e}");
            Err(e)
        }
    }
}
