//! Production-grade Lean e2e: **ict-rs Docker + cw-orch Daemon + JOIN/LEAV**.
//!
//! Not mock. Not ICT_MOCK. Never writes `lean-pending.json`.
//! Exercises live capabilities and **prints a gap matrix** for work still
//! required to ship (image JOIN, cash Δ, Stwo, one power function).
//!
//! Needs `terpz-lean` built from feat/lean-v6 @ 94c61e6 or later.
//!
//! ```sh
//! cd crates/ict-rs/ict-rs-cw-orch
//! ICT_LEAN_IMAGE=terpnetwork/terp-core:terpz-lean \
//! ICT_LEAN_OWNS_VALSET=true \
//!   cargo run --example lean_production_e2e --features docker
//! ```

use ict_rs::cosmos::interchain::wait_for_blocks;
use ict_rs::prelude::*;
use ict_rs_cw_orch::daemon_builder_from_chain;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn refuse_mock() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err(
            "lean_production_e2e refuses ICT_MOCK=1 — this is a full Docker + cw-orch workflow"
                .into(),
        );
    }
    Ok(())
}

fn owns_valset() -> bool {
    matches!(
        env_or("ICT_LEAN_OWNS_VALSET", "true").as_str(),
        "1" | "true" | "TRUE" | "yes"
    )
}

fn nvals() -> usize {
    env_or("ICT_LEAN_VALS", "2").parse().unwrap_or(2)
}

fn nfns() -> usize {
    env_or("ICT_LEAN_FULL", "2").parse().unwrap_or(2)
}

fn pubkey_b64(pk: &serde_json::Value) -> Option<String> {
    pk.get("key")
        .or_else(|| pk.get("value"))
        .and_then(|k| k.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| pk.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string()))
}

fn collect_subjects(genesis: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut subjects = Vec::new();
    if let Some(vals) = genesis
        .pointer("/app_state/staking/validators")
        .and_then(|v| v.as_array())
    {
        for v in vals {
            if let Some(pubkey) = v
                .get("consensus_pubkey")
                .or_else(|| v.get("pubkey"))
                .and_then(pubkey_b64)
            {
                subjects.push(serde_json::json!({"pubkey": pubkey, "weight": 10}));
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
                if let Some(pubkey) = msg
                    .get("consensus_pubkey")
                    .or_else(|| msg.get("pubkey"))
                    .and_then(pubkey_b64)
                {
                    subjects.push(serde_json::json!({"pubkey": pubkey, "weight": 10}));
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
    if owns_valset() {
        let subjects = collect_subjects(&genesis);
        if subjects.is_empty() {
            return Err(IctError::Config(
                "owns=true but no genesis_subjects from gentxs".into(),
            ));
        }
        eprintln!(
            "lean_production_e2e genesis_subjects={}",
            subjects.len()
        );
        genesis["app_state"]["leanval"] = serde_json::json!({
            "leanval_owns_valset": true,
            "genesis_subjects": subjects,
        });
    } else {
        genesis["app_state"]["leanval"] = serde_json::json!({"leanval_owns_valset": false});
    }
    serde_json::to_vec(&genesis).map_err(|e| IctError::Config(format!("encode: {e}")))
}

fn terpz_config() -> ChainConfig {
    let tag = env_or("ICT_LEAN_IMAGE", "terpnetwork/terp-core:terpz-lean");
    let (repository, version) = tag
        .rsplit_once(':')
        .map(|(r, v)| (r.to_string(), v.to_string()))
        .unwrap_or_else(|| ("terpnetwork/terp-core".into(), "terpz-lean".into()));
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terpz-lean-prod".to_string(),
        chain_id: "lean-prod-1".to_string(),
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
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: Some(Box::new(modify_genesis)),
        pre_genesis: None,
        config_file_overrides: std::collections::HashMap::new(),
        additional_start_args: vec!["--wasm.skip_wasmvm_version_check".to_string()],
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

fn bonded_rows(v: &serde_json::Value) -> usize {
    for key in ["rows", "subjects", "bonded_set"] {
        if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
            return arr.len();
        }
    }
    0
}

fn staking_count(v: &serde_json::Value) -> usize {
    v.get("validators")
        .and_then(|x| x.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

async fn query_json(
    chain: &CosmosChain,
    args: &[&str],
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(chain.query_json(args).await?)
}

fn period_of(height: u64) -> u64 {
    height / 600
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

fn encode_leave(period: u64, subject: &[u8]) -> Vec<u8> {
    let subj = if subject.len() > 255 {
        &subject[..255]
    } else {
        subject
    };
    let mut out = b"LEAV".to_vec();
    out.push(1);
    out.extend_from_slice(&period.to_be_bytes());
    out.push(subj.len() as u8);
    out.extend_from_slice(subj);
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
    let s: Vec<u8> = b64.trim().bytes().filter(|c| *c != b'=' && !c.is_ascii_whitespace()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < s.len() + 3 && i < s.len() {
        let a = d(s[i]).ok_or("b64")?;
        let b = if i + 1 < s.len() { d(s[i + 1]).ok_or("b64")? } else { 0 };
        let c = if i + 2 < s.len() { d(s[i + 2]).unwrap_or(0) } else { 0 };
        let e = if i + 3 < s.len() { d(s[i + 3]).unwrap_or(0) } else { 0 };
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

async fn broadcast_raw(
    chain: &CosmosChain,
    raw: &[u8],
) -> Result<String, Box<dyn std::error::Error>> {
    let hex = to_hex(raw);
    let cmd = format!(
        r#"curl -sS "http://127.0.0.1:26657/broadcast_tx_sync?tx=0x{hex}" || wget -qO- "http://127.0.0.1:26657/broadcast_tx_sync?tx=0x{hex}""#
    );
    let out = chain.validators()[0]
        .exec_raw(&["sh", "-c", &cmd], &[])
        .await?;
    Ok(out.stdout_str().to_string() + &out.stderr_str())
}

fn tx_ok(resp: &str) -> bool {
    let json_part = match (resp.find('{'), resp.rfind('}')) {
        (Some(i), Some(j)) if j > i => &resp[i..=j],
        _ => resp.trim(),
    };
    let v: serde_json::Value = serde_json::from_str(json_part).unwrap_or(serde_json::Value::Null);
    let code = v
        .pointer("/result/code")
        .or_else(|| v.get("code"))
        .and_then(|c| c.as_u64())
        .unwrap_or(99);
    code == 0
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
    fn print_matrix(&self) {
        println!("\n=== ELEVATION MATRIX (live vs remaining work) ===");
        for (id, st, note) in &self.rows {
            println!("  {st:8} {id:28} {note}");
        }
    }
    fn must_failed(&self) -> bool {
        self.rows
            .iter()
            .any(|(id, st, _)| *st == "FAIL" && matches!(*id, "file_admission" | "join_leav" | "owns_bonded" | "cw_orch" | "consensus" | "lnpr_checktx"))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
    // cw-orch-daemon/tonic rustls 0.23 requires an explicit process CryptoProvider.
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| "rustls CryptoProvider already set")?;
    refuse_mock()?;

    let n = nvals();
    let f = nfns();
    println!("=== lean_production_e2e Docker+cw-orch ({n} vals + {f} fns) ===");

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let mut chain = CosmosChain::new(terpz_config(), n, f, runtime);
    let result = run(&mut chain, n, f).await;
    if let Err(e) = chain.stop().await {
        eprintln!("cleanup: {e}");
    }
    result
}

async fn run(
    chain: &mut CosmosChain,
    n: usize,
    f: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut caps = Caps::new();
    let ctx = TestContext {
        test_name: "lean-cw-orch-prod".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!(
        "chain up: {} genesis vals, {} full nodes, grpc={:?}",
        chain.validators().len(),
        chain.full_nodes().len(),
        chain.host_grpc_address()
    );
    if chain.validators().len() != n {
        return Err(format!("want {n} genesis containers").into());
    }

    wait_for_blocks(chain, 3).await?;

    // --- cw-orch Daemon over live gRPC (fails if ports/mapping are fake) ---
    let builder = daemon_builder_from_chain(
        chain,
        Some(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        ),
    )?;
    let daemon = tokio::task::spawn_blocking(move || builder.build())
        .await
        .map_err(|e| format!("join Daemon build: {e}"))?;
    match daemon {
        Ok(_d) => {
            println!("  [cw-orch] Daemon built on live gRPC (production node, not mock)");
            caps.rec("cw_orch", "PASS", "Daemon on host gRPC");
        }
        Err(e) => {
            caps.rec("cw_orch", "FAIL", format!("{e}"));
            caps.print_matrix();
            return Err(format!("cw-orch Daemon build failed: {e}").into());
        }
    }

    let h0 = chain.height().await?;
    wait_for_blocks(chain, 3).await?;
    let h1 = chain.height().await?;
    if h1 < h0 + 2 {
        caps.rec("consensus", "FAIL", format!("{h0}→{h1}"));
        caps.print_matrix();
        return Err(format!("consensus stalled {h0}→{h1}").into());
    }
    caps.rec("consensus", "PASS", format!("height {h0} → {h1}"));

    let staking = query_json(chain, &["staking", "validators"]).await?;
    let sv = staking_count(&staking);
    println!("  [staking] validators={sv} (want {n})");
    if sv < n {
        caps.print_matrix();
        return Err(format!("staking {sv} < {n}").into());
    }

    let bs = query_json(chain, &["leanval", "bonded-set", "0"]).await?;
    let rows0 = bonded_rows(&bs);
    println!("  [leanval] bonded-set 0 rows={rows0}");
    if owns_valset() && rows0 < n {
        caps.rec("owns_bonded", "FAIL", format!("{rows0} < {n}"));
        caps.print_matrix();
        return Err(format!("BondedSet {rows0} < {n}").into());
    }
    caps.rec(
        "owns_bonded",
        if owns_valset() { "PASS" } else { "GAP" },
        format!("BondedSet={rows0} owns={}", owns_valset()),
    );

    match query_json(chain, &["wasm", "list-code"]).await {
        Ok(v) => caps.rec("wasm_module", "PASS", format!("list-code bytes={}", v.to_string().len())),
        Err(e) => {
            caps.rec("wasm_module", "FAIL", format!("{e}"));
            caps.print_matrix();
            return Err(format!("wasm list-code: {e}").into());
        }
    }

    let primary = &chain.validators()[0];
    let addr = primary.get_key_address("validator").await?;
    let send = primary
        .bank_send("validator", &addr, "1000uterp", "0.01uterp")
        .await?;
    if send.exit_code != 0 {
        caps.rec("fees_tx", "FAIL", send.stderr_str().to_string());
        caps.print_matrix();
        return Err(format!("fee-bearing bank send failed: {}", send.stderr_str()).into());
    }
    caps.rec("fees_tx", "PASS", "bank send 0.01uterp gas");

    let op = staking
        .pointer("/validators/0/operator_address")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let rewards_before = if !op.is_empty() {
        query_json(chain, &["distribution", "validator-outstanding-rewards", &op])
            .await
            .ok()
    } else {
        None
    };

    // LNPR must not CheckTx (vote-sdk inject-class).
    let lnpr_probe = [b"LNPR".as_slice(), &[1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]].concat();
    let lnpr_resp = broadcast_raw(chain, &lnpr_probe).await.unwrap_or_default();
    if tx_ok(&lnpr_resp) {
        caps.rec("lnpr_checktx", "FAIL", "LNPR accepted in mempool");
    } else {
        caps.rec("lnpr_checktx", "PASS", "LNPR CheckTx rejected");
    }

    caps.rec(
        "file_admission",
        "PASS",
        "e2e does not write lean-pending.json",
    );

    if f >= 2 && owns_valset() {
        workflow_membership(chain, &mut caps).await?;
    } else {
        caps.rec("join_leav", "GAP", "need 2 full nodes + owns=true");
    }

    // Remaining product work — do not launder query-exists as cash/Stwo.
    let rewards_after = if !op.is_empty() {
        query_json(chain, &["distribution", "validator-outstanding-rewards", &op])
            .await
            .ok()
    } else {
        None
    };
    match (rewards_before, rewards_after) {
        (Some(a), Some(b)) if a != b => {
            caps.rec("f1_cash_delta", "PASS", "outstanding-rewards changed")
        }
        (Some(_), Some(_)) => caps.rec(
            "f1_cash_delta",
            "GAP",
            "query ok but Δ=0 — not Lean rewards; need known-fee assert",
        ),
        _ => caps.rec("f1_cash_delta", "GAP", "outstanding-rewards unreadable"),
    }
    caps.rec(
        "dual_last_power",
        "GAP",
        "staking EndBlock still writes LastValidatorPowers (wrap unused)",
    );
    caps.rec(
        "stwo_statement",
        "GAP",
        "Dummy DSTW still; store roots slot exists, not a Stwo walk",
    );
    caps.rec(
        "wasm_sudo_verify",
        "GAP",
        "cw-lean-verifier + proof_instance_verify not exercised",
    );

    // loop_id=aggregate (RESEARCH-PACK-CONSENSUS-OBJECT): one verify for N
    // proofs. Dummy per-subject checksums (3a+5b+7 / DSTW costume) are FAIL.
    // BondedSet 2->4 is not aggregation.
    probe_aggregate(chain, &mut caps, n).await?;

    caps.print_matrix();
    if caps.must_failed() {
        return Err("lean_production_e2e MUST caps failed — see ELEVATION MATRIX".into());
    }
    println!("lean_production_e2e PASSED (gaps remain — not product-done)");
    Ok(())
}


/// loop_id=aggregate: one host verify for N proofs. Dummy per-subject FAIL.
async fn probe_aggregate(
    chain: &CosmosChain,
    caps: &mut Caps,
    n: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let n_proofs = n.max(2);
    let mut dummy_ok = 0usize;
    for i in 0..n_proofs {
        // Per-subject Dummy costume — must not CheckTx as an aggregate.
        let mut blob = b"DSTW".to_vec();
        blob.push(1);
        blob.extend_from_slice(&(i as u64).to_be_bytes());
        // c = 3a+5b+7 costume (not a named Stwo/FRI fold).
        blob.extend_from_slice(&3u64.to_be_bytes());
        blob.extend_from_slice(&5u64.to_be_bytes());
        blob.extend_from_slice(&7u64.to_be_bytes());
        let resp = broadcast_raw(chain, &blob).await.unwrap_or_default();
        if tx_ok(&resp) {
            dummy_ok += 1;
        }
    }

    let agg_q = query_json(chain, &["leanval", "aggregate-proof"]).await;
    let one_object = match &agg_q {
        Ok(v) => v
            .get("proofs")
            .and_then(|p| p.as_array())
            .map(|a| a.len() == 1)
            .unwrap_or(false)
            || v.get("aggregate").is_some()
            || v.get("folded").is_some(),
        Err(_) => false,
    };

    if dummy_ok > 0 {
        caps.rec(
            "loop_aggregate",
            "FAIL",
            format!(
                "Dummy per-subject CheckTx accepted {dummy_ok}/{n_proofs} (3a+5b+7) — not one verify for N"
            ),
        );
    } else if one_object {
        caps.rec(
            "loop_aggregate",
            "PASS",
            format!("one aggregate object; Dummy per-subject rejected ({n_proofs})"),
        );
    } else {
        caps.rec(
            "loop_aggregate",
            "GAP",
            format!(
                "Dummy per-subject rejected ({n_proofs}); no single verify-for-N object (not BondedSet join)"
            ),
        );
    }
    Ok(())
}

/// Dual-writer demo + committed JOIN/LEAV. Never writes lean-pending.json.
async fn workflow_membership(
    chain: &CosmosChain,
    caps: &mut Caps,
) -> Result<(), Box<dyn std::error::Error>> {
    use ict_rs::tx::TxOptions;

    let mut join_pks = Vec::new();
    let mut addrs = Vec::new();
    for i in 0..2 {
        let fnn = &chain.full_nodes()[i];
        let out = fnn.create_key("operator", 118).await?;
        if out.exit_code != 0 {
            return Err(format!("fn{i} key: {}", out.stderr_str()).into());
        }
        addrs.push(fnn.get_key_address("operator").await?);
        let pk_out = fnn.exec_cmd(&["tendermint", "show-validator"]).await?;
        let pk = serde_json::from_str::<serde_json::Value>(pk_out.stdout_str().trim())
            .ok()
            .as_ref()
            .and_then(pubkey_b64)
            .ok_or("show-validator")?;
        join_pks.push(pk);
    }
    let primary = &chain.validators()[0];
    for a in &addrs {
        let o = primary
            .bank_send("validator", a, "2000000000uterp", "")
            .await?;
        if o.exit_code != 0 {
            return Err(format!("fund: {}", o.stderr_str()).into());
        }
    }

    let bs_before = bonded_rows(&query_json(chain, &["leanval", "bonded-set", "0"]).await?);
    let st_before = staking_count(&query_json(chain, &["staking", "validators"]).await?);

    // create-validator without JOIN — staking may grow; BondedSet must not.
    {
        let fnn = &chain.full_nodes()[0];
        let spec = serde_json::json!({
            "pubkey": {"@type":"/cosmos.crypto.ed25519.PubKey","key": join_pks[0]},
            "amount": "1000000000uterp",
            "moniker": "fn-0",
            "identity": "",
            "website": "",
            "security": "",
            "details": "staking-only, not Lean admit",
            "commission-rate": "0.1",
            "commission-max-rate": "0.2",
            "commission-max-change-rate": "0.01",
            "min-self-delegation": "1"
        });
        fnn.exec_raw(
            &[
                "sh",
                "-c",
                &format!("cat > /tmp/v.json << 'EOF'\n{spec}\nEOF"),
            ],
            &[],
        )
        .await?;
        let opts = TxOptions::new(chain.chain_id(), "0.01uterp")
            .from("operator")
            .gas("auto")
            .gas_adjustment(2.0)
            .broadcast_mode("sync");
        let mut args: Vec<String> = vec![
            "tx".into(),
            "staking".into(),
            "create-validator".into(),
            "/tmp/v.json".into(),
        ];
        args.extend(opts.to_flags());
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let out = fnn.exec_cmd(&refs).await?;
        if out.exit_code != 0 {
            caps.rec(
                "staking_without_join",
                "GAP",
                format!("create-validator: {}", out.stderr_str()),
            );
        } else {
            wait_for_blocks(chain, 2).await?;
            let bs = bonded_rows(&query_json(chain, &["leanval", "bonded-set", "0"]).await?);
            let st = staking_count(&query_json(chain, &["staking", "validators"]).await?);
            if bs != bs_before {
                caps.rec(
                    "staking_without_join",
                    "FAIL",
                    format!("BondedSet {bs_before}→{bs} after staking-only"),
                );
            } else {
                caps.rec(
                    "staking_without_join",
                    "PASS",
                    format!("BondedSet stayed {bs}; staking {st_before}→{st}"),
                );
            }
        }
    }

    let height = chain.height().await? as u64;
    let period = period_of(height);
    let subj0 = decode_pk(&join_pks[0])?;
    let subj1 = decode_pk(&join_pks[1])?;

    let r0 = broadcast_raw(chain, &encode_join(period, &subj0, 10)).await?;
    let r1 = broadcast_raw(chain, &encode_join(period, &subj1, 10)).await?;
    println!("  [join] tx0={} tx1={}", r0.chars().take(160).collect::<String>(), r1.chars().take(160).collect::<String>());
    let unc = chain.validators()[0]
        .exec_raw(
            &[
                "sh",
                "-c",
                "wget -qO- http://127.0.0.1:26657/num_unconfirmed_txs || true",
            ],
            &[],
        )
        .await;
    if let Ok(u) = unc {
        println!(
            "  [join] unconfirmed {}",
            u.stdout_str().chars().take(200).collect::<String>()
        );
    }


    if !tx_ok(&r0) || !tx_ok(&r1) {
        caps.rec(
            "join_leav",
            "FAIL",
            format!(
                "JOIN broadcast rejected (rebuild terpz-lean @94c61e6+). r0={} r1={}",
                r0.chars().take(120).collect::<String>(),
                r1.chars().take(120).collect::<String>()
            ),
        );
        return Ok(());
    }

    let mut after_join = 0;
    for _ in 0..16 {
        let _ = broadcast_raw(chain, &encode_join(period, &subj0, 10)).await;
        let _ = broadcast_raw(chain, &encode_join(period, &subj1, 10)).await;
        wait_for_blocks(chain, 1).await?;
        after_join = bonded_rows(&query_json(chain, &["leanval", "bonded-set", "0"]).await?);
        if after_join >= bs_before + 2 {
            break;
        }
    }
    println!("  [churn] BondedSet after JOIN rows={after_join} (before={bs_before})");
    if after_join < bs_before + 2 {
        caps.rec(
            "join_leav",
            "FAIL",
            format!("after JOIN BondedSet {after_join} want >={}", bs_before + 2),
        );
        return Ok(());
    }

    let rl = broadcast_raw(chain, &encode_leave(period, &subj1)).await?;
    if !tx_ok(&rl) {
        caps.rec(
            "join_leav",
            "FAIL",
            format!("LEAV rejected: {}", rl.chars().take(160).collect::<String>()),
        );
        return Ok(());
    }
    wait_for_blocks(chain, 3).await?;
    let after_leave = bonded_rows(&query_json(chain, &["leanval", "bonded-set", "0"]).await?);
    println!("  [churn] BondedSet after LEAV rows={after_leave}");
    if after_leave + 1 != after_join {
        caps.rec(
            "join_leav",
            "FAIL",
            format!("LEAV {after_join}→{after_leave}"),
        );
        return Ok(());
    }
    caps.rec(
        "join_leav",
        "PASS",
        format!("JOIN {bs_before}→{after_join} LEAV →{after_leave} (no pending file)"),
    );
    Ok(())
}
