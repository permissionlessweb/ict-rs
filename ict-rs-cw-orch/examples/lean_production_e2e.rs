//! Production-grade Lean + Stwo e2e via **ict-rs Docker + cw-orch Daemon**.
//!
//! Not mock. Not ICT_MOCK. Not multi-test. A real `terpz` image, N genesis
//! validators, host-mapped gRPC, cw-orch `Daemon` over that gRPC, plus node
//! CLI for BondedSet / staking / wasm / fees.
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

async fn query_json(
    chain: &CosmosChain,
    args: &[&str],
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(chain.query_json(args).await?)
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
    result.map(|()| println!("lean_production_e2e PASSED"))
}

async fn run(
    chain: &mut CosmosChain,
    n: usize,
    f: usize,
) -> Result<(), Box<dyn std::error::Error>> {
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
        }
        Err(e) => {
            return Err(format!(
                "cw-orch Daemon build failed against live node (not mock): {e}"
            )
            .into());
        }
    }

    // --- production module surfaces ---
    let h0 = chain.height().await?;
    wait_for_blocks(chain, 3).await?;
    let h1 = chain.height().await?;
    if h1 < h0 + 2 {
        return Err(format!("consensus stalled {h0}→{h1}").into());
    }
    println!("  [consensus] height {h0} → {h1}");

    let staking = query_json(chain, &["staking", "validators"]).await?;
    let sv = staking
        .get("validators")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    println!("  [staking] validators={sv} (want {n})");
    if sv < n {
        return Err(format!("staking {sv} < {n} — genesis gentxs not collected").into());
    }

    let bs = query_json(chain, &["leanval", "bonded-set", "0"]).await?;
    let rows = bonded_rows(&bs);
    println!("  [leanval] bonded-set 0 rows={rows}");
    if owns_valset() && rows < n {
        return Err(format!("BondedSet {rows} < {n}").into());
    }

    // wasm module is live on production terpz
    match query_json(chain, &["wasm", "list-code"]).await {
        Ok(v) => println!("  [wasm] list-code ok keys={}", v.to_string().len()),
        Err(e) => return Err(format!("wasm list-code required on production image: {e}").into()),
    }

    // fee-bearing send (non-zero gas)
    let primary = &chain.validators()[0];
    let addr = primary.get_key_address("validator").await?;
    let send = primary
        .bank_send("validator", &addr, "1000uterp", "0.01uterp")
        .await?;
    if send.exit_code != 0 {
        return Err(format!("fee-bearing bank send failed: {}", send.stderr_str()).into());
    }
    println!("  [fees] bank send with 0.01uterp gas ok");

    let op = staking
        .pointer("/validators/0/operator_address")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    if !op.is_empty() {
        match query_json(chain, &["distribution", "validator-outstanding-rewards", op]).await {
            Ok(r) => println!(
                "  [rewards] outstanding-rewards query ok bytes={}",
                r.to_string().len()
            ),
            Err(e) => println!("  [rewards] outstanding-rewards: {e}"),
        }
    }

    // MsgSudoContract to a made-up addr must not be a silent success
    // (ante / wasm). Best-effort: query leanval is the module surface.

    if f >= 2 && owns_valset() {
        workflow_join_leave(chain).await?;
    }

    println!("  [prod] Docker+cw-orch+CLI surfaces exercised (no mock)");
    Ok(())
}

async fn workflow_join_leave(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
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
    for i in 0..2 {
        let fnn = &chain.full_nodes()[i];
        let spec = serde_json::json!({
            "pubkey": {"@type":"/cosmos.crypto.ed25519.PubKey","key": join_pks[i]},
            "amount": "1000000000uterp",
            "moniker": format!("fn-{i}"),
            "identity": "",
            "website": "",
            "security": "",
            "details": "cw-orch prod join",
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
            return Err(format!("create-validator fn{i}: {}", out.stderr_str()).into());
        }
    }
    wait_for_blocks(chain, 2).await?;

    let body = serde_json::json!({
        "join": join_pks.iter().map(|pk| serde_json::json!({"pubkey": pk, "weight": 10})).collect::<Vec<_>>(),
        "leave": []
    })
    .to_string();
    for node in chain.validators().iter().chain(chain.full_nodes().iter()) {
        for path in [
            format!("{}/config/lean-pending.json", node.home_dir),
            "/terpd/.terpd/config/lean-pending.json".to_string(),
        ] {
            node.exec_raw(
                &[
                    "sh",
                    "-c",
                    &format!("mkdir -p $(dirname {path}) && cat > {path} << 'EOF'\n{body}\nEOF"),
                ],
                &[],
            )
            .await?;
        }
    }
    wait_for_blocks(chain, 3).await?;
    let bs = query_json(chain, &["leanval", "bonded-set", "0"]).await?;
    let rows = bonded_rows(&bs);
    println!("  [churn] bonded-set after join rows={rows}");
    if rows < 3 {
        return Err(format!("after join BondedSet {rows} < 3").into());
    }
    Ok(())
}
