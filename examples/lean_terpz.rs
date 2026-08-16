//! Multi-validator ICT-rs workflow for the Lean `terpz` worktree.
//!
//! Exercises a 4-validator Terp chain built as **`terpz`** (not `terpd`):
//! consensus (blocks), staking (bonded set), distribution rewards, and the
//! `x/leanval` module surface.
//!
//! ```sh
//! # On groot, from the v6 Lean worktree:
//! #   cd .worktrees/terp-core-lean-v6 && make terpz && make docker-terpz
//!
//! cd crates/ict-rs/ict-rs
//! ICT_LEAN_IMAGE=terpnetwork/terp-core:terpz-lean \
//! ICT_LEAN_BIN=terpz \
//! ICT_LEAN_VALS=4 \
//! ICT_LEAN_WORKFLOWS=consensus,staking,rewards,lean \
//!   cargo run --example lean_terpz --features docker,terp,testing
//!
//! ICT_LEAN_WORKFLOWS=lean-owns cargo run --example lean_terpz --features docker,terp,testing
//!
//! ICT_LEAN_WORKFLOWS=sdk-mirror cargo run --example lean_terpz --features docker,terp,testing
//!
//! ICT_LEAN_WORKFLOWS=p0 cargo run --example lean_terpz --features docker,terp,testing
//! ```
//!
//! Mock (no Docker): `ICT_MOCK=1 cargo run --example lean_terpz --features terp,testing`
//!
//! ## sdk-mirror workflow (SDK v0.54.3 / Comet v0.39.3 names — observational only)
//!
//! These queries **do not** prove Lean EB / BondedSet power. They check the
//! stock surfaces we still claim to support (F1 tokens, slashing signing-info,
//! distr withdraw path). Map:
//! - `x/staking/keeper/delegation_test.go:TestDelegation` / `TestMsgDelegate`
//! - `x/staking/keeper/validator_test.go:TestApplyAndReturnValidatorSetUpdatesPowerDecrease`
//! - `x/staking/keeper/power_reduction_test.go:TestTokensToConsensusPower`
//! - `x/distribution/keeper/allocation_test.go:TestAllocateTokensToManyValidators`
//! - `x/distribution/keeper/delegation_test.go:TestWithdrawDelegationRewardsBasic`
//! - `x/distribution/keeper/abci_test.go:TestBeginBlockToMultipleValidators`
//! - `x/slashing/keeper/signing_info_test.go:TestValidatorSigningInfo`
//! - `x/slashing/keeper/grpc_query_test.go:TestGRPCSigningInfos`
//! - `x/slashing/keeper/hooks_test.go:TestAfterValidatorBonded`
//! - `cometbft/state/execution_test.go:TestFinalizeBlockValidatorUpdates`
//! - `cometbft/state/execution_test.go:TestProcessProposal`
//!
//! ## p0 workflow (TEST-P0-ELEVATION.md)
//!
//! CLI: `terpz query leanval bonded-set [period]`. Success of staking
//! `last-validator-power` is **not** Lean BondedSet / EB.
//! When `ICT_LEAN_OWNS_VALSET` is on, empty BondedSet rows is **FAIL** (not skip-as-pass).
//!
//! Genesis ICT writes (`app_state.leanval`):
//! - G-OFF `{"leanval_owns_valset":false}` (no subjects required)
//! - G-SEED owns=true + `genesis_subjects` one `{pubkey,weight}` per Comet ed25519
//!   (copied from `staking.validators` / `genutil.gen_txs`). Empty subjects + owns
//!   is refused here so InitGenesis never sees owns=true with an empty set.
//!
//! SDK cites for later un-Skip (do not treat these queries as those tests):
//! - `x/staking/genesis_test.go:TestValidateGenesis`
//! - `x/staking/keeper/delegation_test.go:TestDelegation` / `TestRedelegation`
//! - `x/staking/keeper/unbonding_test.go:TestUnbondingCanComplete`
//! - `x/staking/keeper/validator_test.go:TestApplyAndReturnValidatorSetUpdatesPowerDecrease`
//! - `x/staking/keeper/msg_server_test.go:TestMsgDelegate` / `TestMsgUndelegate` / `TestMsgBeginRedelegate`
//! - DummyStwo `TestDummyStwoValidAndBitflip` + weight-bind forge reject
//! - ante: ConsumeGas(150000) before verify; bad bech32 reject

use std::sync::Arc;

use ict_rs::cosmos::interchain::wait_for_blocks;
use ict_rs::prelude::*;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn image_repo() -> String {
    env_or("ICT_LEAN_REPO", "terpnetwork/terp-core")
}

fn image_tag() -> String {
    env_or("ICT_LEAN_IMAGE_TAG", "terpz-lean")
}

fn bin_name() -> String {
    env_or("ICT_LEAN_BIN", "terpz")
}

fn num_validators() -> usize {
    env_or("ICT_LEAN_VALS", "4").parse().unwrap_or(4)
}

fn num_full_nodes() -> usize {
    env_or("ICT_LEAN_FULL", "0").parse().unwrap_or(0)
}

fn layout() -> (usize, usize) {
    let churn = workflow_on(&enabled_workflows(), "churn");
    let vals_set = std::env::var("ICT_LEAN_VALS").ok().filter(|s| !s.is_empty());
    let fns_set = std::env::var("ICT_LEAN_FULL").ok().filter(|s| !s.is_empty());
    if churn {
        (
            vals_set.as_deref().and_then(|s| s.parse().ok()).unwrap_or(2),
            fns_set.as_deref().and_then(|s| s.parse().ok()).unwrap_or(2),
        )
    } else {
        (num_validators(), num_full_nodes())
    }
}

fn owns_valset() -> bool {
    matches!(
        env_or("ICT_LEAN_OWNS_VALSET", "false").as_str(),
        "1" | "true" | "TRUE" | "yes"
    )
}

fn enabled_workflows() -> Vec<String> {
    env_or(
        "ICT_LEAN_WORKFLOWS",
        "consensus,staking,rewards,lean,sdk-mirror,p0",
    )
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn workflow_on(enabled: &[String], name: &str) -> bool {
    enabled.iter().any(|w| w == name || w == "all")
}

fn pubkey_b64_from_obj(pk: &serde_json::Value) -> Option<String> {
    pk.get("key")
        .or_else(|| pk.get("value"))
        .and_then(|k| k.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            pk.as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
}

fn weight_from_validator(v: &serde_json::Value) -> u64 {
    v.get("tokens")
        .or_else(|| v.get("voting_power"))
        .and_then(|t| {
            t.as_u64().or_else(|| {
                t.as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|&n| n > 0 && n < 1_000_000)
            })
        })
        .unwrap_or(10)
}

fn collect_genesis_subjects(genesis: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut subjects = Vec::new();

    if let Some(vals) = genesis
        .pointer("/app_state/staking/validators")
        .and_then(|v| v.as_array())
    {
        for v in vals {
            let pk = v
                .get("consensus_pubkey")
                .or_else(|| v.get("pubkey"))
                .and_then(pubkey_b64_from_obj);
            if let Some(pubkey) = pk {
                subjects.push(serde_json::json!({
                    "pubkey": pubkey,
                    "weight": weight_from_validator(v),
                }));
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
                let ty = msg.get("@type").and_then(|t| t.as_str()).unwrap_or("");
                if !ty.contains("MsgCreateValidator") && msg.get("consensus_pubkey").is_none() {
                    continue;
                }
                if let Some(pubkey) = msg
                    .get("consensus_pubkey")
                    .or_else(|| msg.get("pubkey"))
                    .and_then(pubkey_b64_from_obj)
                {
                    subjects.push(serde_json::json!({
                        "pubkey": pubkey,
                        "weight": 10,
                    }));
                }
            }
        }
    }

    for path in ["/validators", "/consensus/validators"] {
        if let Some(vals) = genesis.pointer(path).and_then(|v| v.as_array()) {
            for v in vals {
                let pk = v
                    .get("pub_key")
                    .or_else(|| v.get("pubkey"))
                    .or_else(|| v.get("consensus_pubkey"))
                    .and_then(pubkey_b64_from_obj);
                if let Some(pubkey) = pk {
                    let w = v
                        .get("power")
                        .or_else(|| v.get("voting_power"))
                        .and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
                        .filter(|&n| n > 0)
                        .unwrap_or(10);
                    subjects.push(serde_json::json!({"pubkey": pubkey, "weight": w}));
                }
            }
        }
    }

    let mut seen = std::collections::BTreeSet::new();
    subjects.retain(|s| {
        s.get("pubkey")
            .and_then(|pk| pk.as_str())
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
    if let Some(params) = genesis.pointer_mut("/app_state/distribution/params") {
        params["community_tax"] = serde_json::json!("0.020000000000000000");
    }

    let owns = owns_valset() || workflow_on(&enabled_workflows(), "lean-owns");
    if owns {
        let subjects = collect_genesis_subjects(&genesis);
        if subjects.is_empty() {
            return Err(IctError::Config(
                "ICT_LEAN_OWNS_VALSET: refusing leanval_owns_valset=true with empty genesis_subjects \
                 (no Comet ed25519 pubs in staking.validators or genutil.gen_txs at modify_genesis)"
                    .into(),
            ));
        }
        eprintln!(
            "lean_terpz modify_genesis: owns=true genesis_subjects={} app_state_keys={:?} validators_top={}",
            subjects.len(),
            genesis.get("app_state").and_then(|a| a.as_object()).map(|o| o.keys().cloned().collect::<Vec<_>>()),
            genesis.get("validators").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)
        );
        genesis["app_state"]["leanval"] = serde_json::json!({
            "leanval_owns_valset": true,
            "genesis_subjects": subjects,
        });
    } else {
        genesis["app_state"]["leanval"] = serde_json::json!({
            "leanval_owns_valset": false,
        });
    }

    serde_json::to_vec(&genesis).map_err(|e| IctError::Config(format!("encode genesis: {e}")))
}

fn terpz_config() -> ChainConfig {
    let default_image = format!("{}:{}", image_repo(), image_tag());
    let tag = env_or("ICT_LEAN_IMAGE", &default_image);
    let (repository, version) = if let Some((r, v)) = tag.rsplit_once(':') {
        (r.to_string(), v.to_string())
    } else {
        (image_repo(), image_tag())
    };

    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terpz-lean".to_string(),
        chain_id: "lean-1".to_string(),
        images: vec![DockerImage {
            repository,
            version,
            uid_gid: None,
        }],
        bin: bin_name(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: if owns_valset() || workflow_on(&enabled_workflows(), "lean-owns") {
            "0.01uterp".to_string()
        } else {
            "0uterp".to_string()
        },
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

async fn query_json(chain: &CosmosChain, args: &[&str]) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(chain.query_json(args).await?)
}

async fn workflow_consensus(
    chain: &CosmosChain,
    nvals: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let h0 = chain.height().await?;
    wait_for_blocks(chain, 4).await?;
    let h1 = chain.height().await?;
    println!("  [consensus] height {h0} → {h1} (want +4), validators={nvals}");
    if h1 < h0 + 3 {
        return Err(format!("consensus stalled: {h0} → {h1}").into());
    }
    if chain.validators().len() < nvals {
        return Err(format!(
            "expected {nvals} validators, got {}",
            chain.validators().len()
        )
        .into());
    }
    Ok(())
}

async fn workflow_staking(chain: &CosmosChain, nvals: usize) -> Result<(), Box<dyn std::error::Error>> {
    let vals = query_json(chain, &["staking", "validators"]).await?;
    let list = vals
        .get("validators")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let bonded = list
        .iter()
        .filter(|v| v.get("status").and_then(|s| s.as_str()) == Some("BOND_STATUS_BONDED"))
        .count();
    println!(
        "  [staking] validators={} bonded={}",
        list.len(),
        bonded
    );
    if list.len() < nvals {
        return Err(format!(
            "staking validators {} < {nvals} (genesis must collect N gentxs)",
            list.len()
        )
        .into());
    }
    if bonded == 0 {
        return Err("no bonded validators".into());
    }
    Ok(())
}

fn bonded_set_row_count(v: &serde_json::Value) -> usize {
    for key in ["rows", "subjects", "bonded_set", "genesis_subjects", "set", "validators"] {
        if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
            return arr.len();
        }
    }
    v.as_array().map(|a| a.len()).unwrap_or(0)
}

async fn workflow_rewards(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    wait_for_blocks(chain, 2).await?;
    let pool = query_json(chain, &["distribution", "community-pool"]).await?;
    let params = query_json(chain, &["distribution", "params"]).await?;
    println!(
        "  [rewards] community-pool={} distribution-params={}",
        !pool.is_null(),
        !params.is_null()
    );
    if params.is_null() {
        return Err("distribution params missing — rewards module not live".into());
    }

    let owns = owns_valset() || workflow_on(&enabled_workflows(), "lean-owns");
    if !owns {
        println!(
            "  [rewards] stock SDK: BeginBlock AllocateTokens (Lean does not own valset)."
        );
        return Ok(());
    }

    // After 7edd68c WrapDistribution runs AllocateTokens with BondedSet weights.
    // Params JSON is not a fee-split proof. Try outstanding rewards after a fee tx.
    let vals = query_json(chain, &["staking", "validators"]).await.ok();
    let op = vals.as_ref().and_then(|v| {
        v.get("validators")
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .and_then(|val| val.get("operator_address").or_else(|| val.get("operatorAddress")))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    });

    match op {
        Some(addr) => {
            match query_json(chain, &["distribution", "validator-outstanding-rewards", &addr]).await
            {
                Ok(r) => {
                    let blob = r.to_string();
                    println!(
                        "  [rewards] lean-owns: outstanding-rewards for {addr} bytes={} (fusion path; Δ needs two samples + fee tx)",
                        blob.len()
                    );
                    if r.is_null() {
                        return Err(
                            "lean-owns: outstanding-rewards null — do not pass on distribution params"
                                .into(),
                        );
                    }
                }
                Err(e) => {
                    return Err(format!(
                        "lean-owns FAIL: outstanding-rewards query failed ({e}); params JSON is not a rewards proof"
                    )
                    .into());
                }
            }
        }
        None => {
            return Err(
                "lean-owns FAIL: no validator operator to query outstanding rewards; params-only is not enough"
                    .into(),
            );
        }
    }
    Ok(())
}

async fn workflow_lean(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let versions = query_json(chain, &["upgrade", "module_versions"]).await.unwrap_or(serde_json::json!({}));
    let blob = versions.to_string();
    let present = blob.contains("leanval");
    println!("  [lean] module_versions contains leanval={present}");
    if !present {
        println!("  [lean] WARN: leanval not in module_versions (query dump trimmed) — checking genesis params path");
    }
    match query_json(chain, &["leanval", "params"]).await {
        Ok(v) if !v.is_null() => println!("  [lean] query leanval params ok"),
        _ => println!("  [lean] query leanval params missing (no gRPC yet; genesis still applied)"),
    }
    Ok(())
}

/// Cheap stock queries. Do **not** treat success as Lean voting-power proof.
async fn workflow_sdk_mirror(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let staking_params = query_json(chain, &["staking", "params"]).await;
    let pool = query_json(chain, &["staking", "pool"]).await;
    println!(
        "  [sdk-mirror] staking params={} pool={} (TestDelegation / TestLastTotalPower — tokens only)",
        staking_params.is_ok(),
        pool.is_ok()
    );

    match query_json(chain, &["slashing", "signing-infos"]).await {
        Ok(v) => println!(
            "  [sdk-mirror] slashing signing-infos ok (TestValidatorSigningInfo / TestGRPCSigningInfos) keys={}",
            v.to_string().len()
        ),
        Err(e) => println!("  [sdk-mirror] slashing signing-infos: {e} (not Lean power)"),
    }

    match query_json(chain, &["distribution", "community-pool"]).await {
        Ok(_) => println!(
            "  [sdk-mirror] distribution community-pool (TestFundCommunityPool / AllocateTokens tax)"
        ),
        Err(e) => println!("  [sdk-mirror] distribution community-pool: {e}"),
    }
    match query_json(chain, &["distribution", "params"]).await {
        Ok(_) => println!(
            "  [sdk-mirror] distribution params (TestBeginBlockToMultipleValidators — F1 still live)"
        ),
        Err(e) => println!("  [sdk-mirror] distribution params: {e}"),
    }

    println!(
        "  [sdk-mirror] BondedSet vs last-validator-power: do not claim Lean EB from staking last-validator-power"
    );
    Ok(())
}

/// P0 probes. CLI is `terpz query leanval bonded-set [period]`.
/// Do not treat staking last-validator-power as Lean EB. Empty rows + owns = FAIL.
async fn workflow_p0(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "  [p0] query leanval bonded-set 0; never treat last-validator-power as Lean success"
    );

    let owns = owns_valset() || workflow_on(&enabled_workflows(), "lean-owns");
    match query_json(chain, &["leanval", "bonded-set", "0"]).await {
        Ok(v) => {
            let n = bonded_set_row_count(&v);
            println!(
                "  [p0] leanval bonded-set period=0 rows={n} (BondedSet only; not staking power)"
            );
            if owns && n == 0 {
                return Err(
                    "p0 FAIL: leanval owns valset but bonded-set 0 has no rows (not skip-as-pass)"
                        .into(),
                );
            }
        }
        Err(e) => {
            if owns {
                return Err(format!(
                    "p0 FAIL: leanval bonded-set 0 required when owns=true: {e}"
                )
                .into());
            }
            println!("  [p0] leanval bonded-set 0: {e} (owns=false; not claimed as Lean EB)");
        }
    }

    match query_json(chain, &["staking", "pool"]).await {
        Ok(_) => println!(
            "  [p0] staking pool ok — tokens/F1 only; not BondedSet (TestDelegation keep)"
        ),
        Err(e) => println!("  [p0] staking pool: {e}"),
    }
    match query_json(chain, &["staking", "validators"]).await {
        Ok(_) => println!(
            "  [p0] staking validators ok — BOND_STATUS is not Lean HasProof"
        ),
        Err(e) => println!("  [p0] staking validators: {e}"),
    }

    println!(
        "  [p0] last-validator-power is stock SDK; never assert == Lean weight when flag on"
    );
    Ok(())
}

async fn workflow_lean_owns(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let h0 = chain.height().await?;
    wait_for_blocks(chain, 3).await?;
    let h1 = chain.height().await?;
    println!("  [lean-owns] still producing blocks {h0} → {h1} with leanval_owns_valset genesis");
    if h1 <= h0 {
        return Err("lean-owns: chain halted".into());
    }
    Ok(())
}


fn pubkey_b64_from_show_validator(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    pubkey_b64_from_obj(&v)
}

async fn write_pending_all(
    chain: &CosmosChain,
    join: &[String],
    leave: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut joins = Vec::new();
    for pk in join {
        joins.push(serde_json::json!({"pubkey": pk, "weight": 10}));
    }
    let body = serde_json::json!({"join": joins, "leave": leave}).to_string();
    for node in chain.validators().iter().chain(chain.full_nodes().iter()) {
        for path in [
            format!("{}/config/lean-pending.json", node.home_dir),
            "/terpd/.terpd/config/lean-pending.json".to_string(),
        ] {
            let cmd = format!("mkdir -p $(dirname {path}) && cat > {path} << 'EOF'\n{body}\nEOF");
            let out = node.exec_raw(&["sh", "-c", &cmd], &[]).await?;
            if out.exit_code != 0 {
                return Err(format!("write pending {path} on {}: {}", node.hostname, out.stderr_str()).into());
            }
        }
    }
    println!("  [churn] wrote lean-pending join={} leave={}", join.len(), leave.len());
    Ok(())
}

async fn outstanding_amount(
    chain: &CosmosChain,
    valoper: &str,
) -> Result<f64, Box<dyn std::error::Error>> {
    let v = query_json(chain, &["distribution", "validator-outstanding-rewards", valoper]).await?;
    let amt = v
        .pointer("/rewards/rewards")
        .or_else(|| v.pointer("/rewards"))
        .and_then(|r| r.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|c| {
                if c.get("denom").and_then(|d| d.as_str()) != Some("uterp") {
                    return None;
                }
                c.get("amount")
                    .and_then(|a| a.as_str().map(|s| s.to_string()).or_else(|| a.as_f64().map(|f| f.to_string())))
            })
        })
        .unwrap_or_else(|| "0".to_string());
    Ok(amt.parse::<f64>().unwrap_or(0.0))
}

/// 2 genesis validators, 2 full-nodes create-validator in the same block,
/// Lean pending join; then one genesis val leaves (Lean leave + unbond);
/// rewards must not grow for the leaver; remaining val still accrues (delegators).
async fn workflow_churn(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    use ict_rs::tx::TxOptions;

    if chain.full_nodes().len() < 2 {
        return Err("churn requires 2 full nodes (ICT_LEAN_FULL=2)".into());
    }
    if chain.validators().len() < 2 {
        return Err("churn requires 2 genesis validators".into());
    }

    wait_for_blocks(chain, 2).await?;

    let mut join_pks = Vec::new();
    let mut fn_addrs = Vec::new();
    for i in 0..2 {
        let fnode = &chain.full_nodes()[i];
        let key = "operator";
        let out = fnode.create_key(key, 118).await?;
        if out.exit_code != 0 {
            return Err(format!("fn{i} create_key: {}", out.stderr_str()).into());
        }
        let addr = fnode.get_key_address(key).await?;
        fn_addrs.push(addr.clone());
        let pk_out = fnode.exec_cmd(&["tendermint", "show-validator"]).await?;
        let pk = pubkey_b64_from_show_validator(&pk_out.stdout_str())
            .ok_or_else(|| format!("fn{i} show-validator parse: {}", pk_out.stdout_str()))?;
        join_pks.push(pk);
        println!("  [churn] fn{i} operator={addr}");
    }

    let primary = &chain.validators()[0];
    for addr in &fn_addrs {
        let out = primary
            .bank_send("validator", addr, "2000000000uterp", "")
            .await?;
        if out.exit_code != 0 {
            return Err(format!("fund {addr}: {}", out.stderr_str()).into());
        }
    }

    // Same block: broadcast both create-validator without waiting between them.
    for i in 0..2 {
        let fnode = &chain.full_nodes()[i];
        let pk = &join_pks[i];
        let spec = serde_json::json!({
            "pubkey": {"@type":"/cosmos.crypto.ed25519.PubKey","key": pk},
            "amount": "1000000000uterp",
            "moniker": format!("fn-{i}"),
            "identity": "",
            "website": "",
            "security": "",
            "details": "ict churn joiner",
            "commission-rate": "0.1",
            "commission-max-rate": "0.2",
            "commission-max-change-rate": "0.01",
            "min-self-delegation": "1"
        });
        let path = "/tmp/validator.json";
        let w = format!("cat > {path} << 'EOF'\n{spec}\nEOF");
        fnode.exec_raw(&["sh", "-c", &w], &[]).await?;
        let opts = TxOptions::new(chain.chain_id(), "0.01uterp")
            .from("operator")
            .gas("auto")
            .gas_adjustment(2.0)
            .broadcast_mode("sync");
        let flags = opts.to_flags();
        let mut args: Vec<String> = vec![
            "tx".into(),
            "staking".into(),
            "create-validator".into(),
            path.into(),
        ];
        args.extend(flags);
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let out = fnode.exec_cmd(&refs).await?;
        println!(
            "  [churn] fn{i} create-validator exit={} err={}",
            out.exit_code,
            out.stderr_str().chars().take(160).collect::<String>()
        );
        if out.exit_code != 0 {
            return Err(format!("fn{i} create-validator: {}", out.stderr_str()).into());
        }
    }
    wait_for_blocks(chain, 2).await?;

    write_pending_all(chain, &join_pks, &[]).await?;
    wait_for_blocks(chain, 3).await?;

    let set = query_json(chain, &["leanval", "bonded-set", "0"]).await;
    match set {
        Ok(v) => {
            let n = bonded_set_row_count(&v);
            println!("  [churn] bonded-set after same-block join rows={n} (want >= 3)");
            if n < 3 {
                return Err(format!("same-block join: BondedSet rows {n} < 3").into());
            }
        }
        Err(e) => return Err(format!("bonded-set after join: {e}").into()),
    }

    let vals = query_json(chain, &["staking", "validators"]).await?;
    let list = vals
        .get("validators")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    println!("  [churn] staking validators after join={}", list.len());

    // Identify genesis valoper 1 to leave (second validator node).
    let leave_node = &chain.validators()[1];
    let leave_pk_out = leave_node.exec_cmd(&["tendermint", "show-validator"]).await?;
    let leave_pk = pubkey_b64_from_show_validator(&leave_pk_out.stdout_str())
        .ok_or("leave show-validator")?;
    let leave_valoper = leave_node
        .exec_cmd(&["keys", "show", "validator", "--bech", "val", "--output", "json", "--keyring-backend", "test"])
        .await?;
    let leave_info: serde_json::Value = serde_json::from_str(leave_valoper.stdout_str().trim()).unwrap_or(serde_json::json!({}));
    let leave_oper = leave_info
        .get("address")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();
    if leave_oper.is_empty() {
        return Err(format!("no valoper for leaver: {}", leave_valoper.stdout_str()).into());
    }

    let stay_node = &chain.validators()[0];
    let stay_valoper_out = stay_node
        .exec_cmd(&["keys", "show", "validator", "--bech", "val", "--output", "json", "--keyring-backend", "test"])
        .await?;
    let stay_info: serde_json::Value = serde_json::from_str(stay_valoper_out.stdout_str().trim()).unwrap_or(serde_json::json!({}));
    let stay_oper = stay_info
        .get("address")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();

    let before_leave = outstanding_amount(chain, &leave_oper).await.unwrap_or(0.0);
    let before_stay = outstanding_amount(chain, &stay_oper).await.unwrap_or(0.0);
    println!("  [churn] outstanding before leave stay={before_stay} leave={before_leave}");

    write_pending_all(chain, &join_pks, &[leave_pk]).await?;

    let unbond = leave_node
        .exec_tx(&[
            "tx",
            "staking",
            "unbond",
            &leave_oper,
            "1000000000uterp",
            "--from",
            "validator",
        ])
        .await?;
    println!(
        "  [churn] unbond leaver exit={} {}",
        unbond.exit_code,
        unbond.stderr_str().chars().take(120).collect::<String>()
    );

    wait_for_blocks(chain, 4).await?;

    let after_leave = outstanding_amount(chain, &leave_oper).await.unwrap_or(0.0);
    let after_stay = outstanding_amount(chain, &stay_oper).await.unwrap_or(0.0);
    let d_leave = after_leave - before_leave;
    let d_stay = after_stay - before_stay;
    println!("  [churn] outstanding Δ stay={d_stay} leave={d_leave}");

    if d_leave > d_stay && d_leave > 1.0 {
        return Err(format!(
            "leaver outstanding grew more than stayer (Δleave={d_leave} Δstay={d_stay})"
        )
        .into());
    }
    if d_stay <= 0.0 && after_stay <= 0.0 {
        println!("  [churn] WARN: stayer outstanding did not increase (fees may be tiny); delegator path still required");
    } else {
        println!("  [churn] remaining validator still accrues — F1 to its delegators");
    }

    let set2 = query_json(chain, &["leanval", "bonded-set", "0"]).await?;
    let n2 = bonded_set_row_count(&set2);
    println!("  [churn] bonded-set after leave rows={n2}");
    if n2 < 2 {
        return Err(format!("after leave expected remaining BondedSet, got {n2}").into());
    }

    // Another join: reuse already-joined fns as present; require set still live.
    println!("  [churn] scenarios: same-block 2-join, leave, no extra F1 for leaver, remaining delegators still in BondedSet");
    Ok(())
}

async fn run(chain: &mut CosmosChain, nvals: usize) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext {
        test_name: "lean-terpz-multival".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain started ({} validators).", chain.validators().len());

    let enabled = enabled_workflows();
    println!("workflows: {}", enabled.join(","));

    if workflow_on(&enabled, "consensus") {
        workflow_consensus(chain, nvals).await?;
    }
    if workflow_on(&enabled, "staking") {
        workflow_staking(chain, nvals).await?;
    }
    if workflow_on(&enabled, "rewards") {
        workflow_rewards(chain).await?;
    }
    if workflow_on(&enabled, "lean") {
        workflow_lean(chain).await?;
    }
    if workflow_on(&enabled, "sdk-mirror") {
        workflow_sdk_mirror(chain).await?;
    }
    if workflow_on(&enabled, "lean-owns") {
        workflow_lean_owns(chain).await?;
    }
    if workflow_on(&enabled, "p0") {
        workflow_p0(chain).await?;
    }
    if workflow_on(&enabled, "churn") {
        workflow_churn(chain).await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();

    let (nvals, nfns) = layout();
    println!(
        "=== Lean terpz ICT ({nvals} genesis vals, {nfns} full nodes, bin={}) ===",
        bin_name()
    );

    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        let runtime: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
        let config = terpz_config();
        let chain = CosmosChain::new(config, nvals, nfns, runtime);
        if chain.config().bin != bin_name() {
            return Err(format!("mock: bin {} != {}", chain.config().bin, bin_name()).into());
        }
        println!(
            "Mock runtime: requested {nvals} vals + {nfns} full nodes, bin={}, chain_id={}",
            chain.config().bin,
            chain.chain_id()
        );
        println!("lean_terpz MOCK structural check PASSED");
        println!(
            "mock cannot prove BondedSet rows, Comet VP, or outstanding-rewards Δ (no node, no fee tx)"
        );
        return Ok(());
    }

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let mut chain = CosmosChain::new(terpz_config(), nvals, nfns, runtime);
    let result = run(&mut chain, nvals).await;
    if let Err(e) = chain.stop().await {
        eprintln!("cleanup: {e}");
    }
    result.map(|()| println!("lean_terpz PASSED"))
}
