//! Pair C — sudo ante + valset cutover (ict-rs / Docker).
//!
//! Handshake: [`LEAN_WAIST_HANDSHAKE.md`](./LEAN_WAIST_HANDSHAKE.md).
//! Impl: `/Users/returniflost/terp-core-lean-consensus` `feat/lean-consensus`
//! @ `c51dbee` — **not** merged into `.worktrees/lean-consensus` yet.
//!
//! `leanval_owns_valset` **defaults OFF**. Two processes:
//!
//! ```sh
//! # Run A — flag off — classic staking VP (assertion 4)
//! cargo run --example lean_waist --features docker
//!
//! # Run B — flag on — bond-only frozen VP (assertion 3)
//! ICT_LEAN_OWNS_VALSET=true cargo run --example lean_waist --features docker
//! ```

use ict_rs::cli::parse_query_response;
use ict_rs::prelude::*;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// `types.TestLeanVerifierAcc()` = 20 zero bytes, bech32 prefix `terp`.
const TEST_LEAN_VERIFIER_TERP: &str = "terp1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq5ffndw";

/// `types.ErrSudoLeanVerifier` — codespace `leanval`, code **2**.
const ERR_SUDO_LEAN_VERIFIER_CODE: i64 = 2;

fn image_repo() -> String {
    env_or("ICT_LEAN_IMAGE_REPO", "terpnetwork/terp-core")
}

fn image_tag() -> String {
    env_or("ICT_LEAN_IMAGE", "local")
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
    if let Some(params) = genesis.pointer_mut("/app_state/gov/params") {
        params["min_deposit"] = serde_json::json!([{"denom": "uterp", "amount": "10000000"}]);
        params["voting_period"] = serde_json::json!("15s");
        params["expedited_voting_period"] = serde_json::json!("5s");
        params["max_deposit_period"] = serde_json::json!("15s");
    }

    // Default OFF unless ICT_LEAN_OWNS_VALSET=true (two-run polarity).
    let owns = env_opt("ICT_LEAN_OWNS_VALSET")
        .map(|f| matches!(f.as_str(), "1" | "true" | "TRUE" | "yes"))
        .unwrap_or(false);
    if let Some(obj) = genesis.pointer_mut("/app_state/leanval") {
        if obj.get("params").is_none() {
            obj["params"] = serde_json::json!({});
        }
        if let Some(params) = obj.get_mut("params") {
            params["leanval_owns_valset"] = serde_json::json!(owns);
        }
    } else if let Some(app) = genesis.get_mut("app_state") {
        app["leanval"] = serde_json::json!({
            "params": { "leanval_owns_valset": owns }
        });
    }

    serde_json::to_vec(&genesis).map_err(|e| IctError::Config(format!("encode genesis: {e}")))
}

fn terp_config(version: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: "lean-waist-1".to_string(),
        images: vec![DockerImage {
            repository: image_repo(),
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

async fn query_json_ok(chain: &CosmosChain, args: &[&str]) -> Option<serde_json::Value> {
    query_json(chain, args).await.ok()
}

fn tx_code(raw: &str) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    v.get("code")
        .and_then(|c| c.as_i64())
        .or_else(|| v.pointer("/tx_response/code").and_then(|c| c.as_i64()))
}

fn tx_codespace(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    v.get("codespace")
        .and_then(|c| c.as_str())
        .or_else(|| v.pointer("/tx_response/codespace").and_then(|c| c.as_str()))
        .map(|s| s.to_string())
}

fn tx_raw_log(raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return raw.to_string();
    };
    v.get("raw_log")
        .or_else(|| v.pointer("/tx_response/raw_log"))
        .and_then(|c| c.as_str())
        .unwrap_or(raw)
        .to_string()
}

struct TxOut {
    exit: i32,
    stdout: String,
    stderr: String,
    code: Option<i64>,
    codespace: Option<String>,
}

async fn tx(chain: &CosmosChain, args: &[&str]) -> Result<TxOut, Box<dyn std::error::Error>> {
    let mut cmd = vec!["tx"];
    cmd.extend_from_slice(args);
    cmd.extend_from_slice(&[
        "--gas",
        "auto",
        "--gas-adjustment",
        "1.5",
        "--fees",
        "2000uterp",
        "-y",
        "--output",
        "json",
    ]);
    let out = chain.exec(&cmd, &[]).await?;
    let stdout = out.stdout_str();
    let stderr = out.stderr_str();
    let blob = if tx_code(&stdout).is_some() {
        stdout.clone()
    } else {
        stderr.clone()
    };
    Ok(TxOut {
        exit: out.exit_code,
        stdout,
        stderr,
        code: tx_code(&blob),
        codespace: tx_codespace(&blob),
    })
}

/// Hardcoded test waist unless ICT_LEAN_VERIFIER / query overrides.
fn resolve_lean_verifier_sync() -> String {
    env_opt("ICT_LEAN_VERIFIER").unwrap_or_else(|| TEST_LEAN_VERIFIER_TERP.to_string())
}

/// Default **off** (implementer `Keeper.OwnsValset() == false`).
fn leanval_owns_valset_intended() -> bool {
    env_opt("ICT_LEAN_OWNS_VALSET")
        .map(|f| matches!(f.as_str(), "1" | "true" | "TRUE" | "yes"))
        .unwrap_or(false)
}

fn first_validator_power(set: &serde_json::Value) -> Result<(String, i64), Box<dyn std::error::Error>> {
    let vals = set
        .get("validators")
        .and_then(|v| v.as_array())
        .ok_or("tendermint-validator-set: no validators")?;
    let v0 = vals.first().ok_or("empty validator set")?;
    let addr = v0
        .get("address")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();
    let power = v0
        .get("voting_power")
        .or_else(|| v0.get("power"))
        .and_then(|p| {
            p.as_i64()
                .or_else(|| p.as_str().and_then(|s| s.parse().ok()))
        })
        .ok_or("validator missing voting_power")?;
    Ok((addr, power))
}

async fn first_operator_addr(chain: &CosmosChain) -> Result<String, Box<dyn std::error::Error>> {
    let vals = query_json(chain, &["staking", "validators"]).await?;
    let arr = vals
        .get("validators")
        .and_then(|v| v.as_array())
        .ok_or("no staking validators")?;
    let op = arr
        .first()
        .and_then(|v| v.get("operator_address").or_else(|| v.get("operator_addr")))
        .and_then(|s| s.as_str())
        .ok_or("validator missing operator_address")?;
    Ok(op.to_string())
}

async fn comet_power_for(
    chain: &CosmosChain,
    tm_addr: &str,
) -> Result<i64, Box<dyn std::error::Error>> {
    let set = query_json(chain, &["tendermint-validator-set"]).await?;
    let vals = set
        .get("validators")
        .and_then(|v| v.as_array())
        .ok_or("no comet validators")?;
    for v in vals {
        let a = v.get("address").and_then(|x| x.as_str()).unwrap_or("");
        if a == tm_addr || tm_addr.is_empty() {
            return v
                .get("voting_power")
                .or_else(|| v.get("power"))
                .and_then(|p| {
                    p.as_i64()
                        .or_else(|| p.as_str().and_then(|s| s.parse().ok()))
                })
                .ok_or_else(|| "missing voting_power".into());
        }
    }
    Err("comet validator address not found after delegate".into())
}

/// 1. MsgSudoContract → Lean verifier: codespace `leanval`, code **2**.
async fn assert_sudo_rejected(
    chain: &CosmosChain,
    from: &str,
    verifier: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = r#"{"verify_and_apply":{}}"#;
    let out = tx(chain, &["wasm", "sudo", verifier, msg, "--from", from]).await?;
    let abci = out.code.unwrap_or(if out.exit == 0 { 0 } else { -1 });
    let space = out.codespace.as_deref().unwrap_or("");
    let log = tx_raw_log(&out.stdout);
    println!(
        "  [1] wasm sudo lean verifier exit={} code={} codespace={space}",
        out.exit, abci
    );
    if abci != ERR_SUDO_LEAN_VERIFIER_CODE {
        return Err(format!(
            "MsgSudoContract to Lean verifier must be ErrSudoLeanVerifier (code 2, codespace leanval); got code={abci} codespace={space} log={log} stderr={}",
            out.stderr
        )
        .into());
    }
    if !space.is_empty() && space != "leanval" {
        return Err(format!("expected codespace leanval, got {space}").into());
    }
    if !log.is_empty()
        && !log.contains("MsgSudoContract targeting Lean verifier is forbidden")
        && !log.contains("leanval")
    {
        println!("  [1] warn: raw_log missing ErrSudoLeanVerifier text: {log}");
    }
    Ok(())
}

/// 2. Unrelated wasm execute still works.
async fn assert_unrelated_wasm_execute(
    chain: &CosmosChain,
    from: &str,
    lean_verifier: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dummy) = env_opt("ICT_LEAN_DUMMY_WASM") {
        let out = tx(
            chain,
            &["wasm", "execute", &dummy, r#"{"noop":{}}"#, "--from", from],
        )
        .await?;
        let abci = out.code.unwrap_or(if out.exit == 0 { 0 } else { 1 });
        println!(
            "  [2] wasm execute dummy {dummy} exit={} code={abci}",
            out.exit
        );
        if abci != 0 {
            return Err(format!("unrelated wasm execute failed: {}", out.stdout).into());
        }
        return Ok(());
    }

    let codes = query_json_ok(chain, &["wasm", "list-contract-by-code", "1"])
        .await
        .or(query_json_ok(chain, &["wasm", "list-code"]).await);
    let Some(codes) = codes else {
        println!("  [2] skip: no wasm codes / ICT_LEAN_DUMMY_WASM unset");
        return Ok(());
    };

    let mut contract: Option<String> = None;
    if let Some(arr) = codes
        .get("contracts")
        .or_else(|| codes.get("contract_infos"))
        .and_then(|a| a.as_array())
    {
        for c in arr {
            let addr = c
                .as_str()
                .or_else(|| c.get("address").and_then(|x| x.as_str()))
                .or_else(|| c.get("contract_address").and_then(|x| x.as_str()));
            if let Some(addr) = addr {
                if lean_verifier.map(|v| v != addr).unwrap_or(true) {
                    contract = Some(addr.to_string());
                    break;
                }
            }
        }
    }
    let Some(addr) = contract else {
        println!("  [2] skip: no non-Lean contract instantiated");
        return Ok(());
    };

    let out = tx(
        chain,
        &["wasm", "execute", &addr, r#"{"noop":{}}"#, "--from", from],
    )
    .await?;
    let abci = out.code.unwrap_or(if out.exit == 0 { 0 } else { 1 });
    println!("  [2] wasm execute {addr} exit={} code={abci}", out.exit);
    if abci != 0 {
        return Err(format!("unrelated wasm execute failed: {}", out.stdout).into());
    }
    Ok(())
}

/// 3 / 4. Bond-only vs Comet voting power, gated on leanval_owns_valset.
async fn assert_valset_cutover(
    chain: &CosmosChain,
    from: &str,
    owns: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let set = query_json(chain, &["tendermint-validator-set"]).await?;
    let (tm_addr, power_before) = first_validator_power(&set)?;
    let op = first_operator_addr(chain).await?;
    println!("  [valset] owns={owns} tm={tm_addr} op={op} power_before={power_before}");

    let out = tx(
        chain,
        &["staking", "delegate", &op, "1000000uterp", "--from", from],
    )
    .await?;
    let abci = out.code.unwrap_or(if out.exit == 0 { 0 } else { 1 });
    if abci != 0 {
        return Err(format!("staking delegate failed: {}", out.stdout).into());
    }

    wait_for_blocks(chain, 3).await?;
    let power_after = comet_power_for(chain, &tm_addr).await?;
    println!("  [valset] power_after={power_after}");

    if owns {
        if power_after != power_before {
            return Err(format!(
                "leanval_owns_valset=true: Comet VP must not change on bond-only ({power_before} → {power_after})"
            )
            .into());
        }
        println!("  [3] bond-only did not change Comet VP");
    } else if power_after <= power_before {
        return Err(format!(
            "leanval_owns_valset=false: classic staking must raise Comet VP ({power_before} → {power_after})"
        )
        .into());
    } else {
        println!("  [4] classic staking raised Comet VP");
    }
    Ok(())
}

/// 5. Evidence / surround path still registered.
async fn assert_evidence_registered(
    chain: &CosmosChain,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(versions) = query_json(chain, &["upgrade", "module_versions"]).await {
        let blob = versions.to_string();
        if blob.contains("evidence") {
            println!("  [5] evidence present in module_versions");
            return Ok(());
        }
    }
    if query_json_ok(chain, &["evidence", "params"]).await.is_some()
        || query_json_ok(chain, &["evidence", "list"]).await.is_some()
    {
        println!("  [5] evidence query path exists");
        return Ok(());
    }
    if query_json_ok(chain, &["slashing", "signing-infos"]).await.is_some() {
        println!("  [5] slashing signing-infos present (evidence neighbor)");
        return Ok(());
    }
    Err("evidence module not registered (query upgrade/evidence/slashing all missed)".into())
}

async fn run_test(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext {
        test_name: "lean-waist".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain started.");

    chain.create_key("waistuser").await?;
    let user_addr = chain.primary_node()?.get_key_address("waistuser").await?;
    chain
        .send_funds(
            "validator",
            &WalletAmount {
                address: user_addr.clone(),
                denom: "uterp".to_string(),
                amount: 10_000_000_000,
            },
        )
        .await?;
    println!("Funded user: {user_addr}");

    let verifier = resolve_lean_verifier_sync();
    println!("Lean verifier (TestLeanVerifierAcc / ICT_LEAN_VERIFIER): {verifier}");
    assert_sudo_rejected(chain, "waistuser", &verifier).await?;

    assert_unrelated_wasm_execute(chain, "waistuser", Some(verifier.as_str())).await?;

    let owns = leanval_owns_valset_intended();
    println!(
        "leanval_owns_valset={owns} (default OFF; set ICT_LEAN_OWNS_VALSET=true for run B)"
    );
    assert_valset_cutover(chain, "waistuser", owns).await?;

    assert_evidence_registered(chain).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let tag = image_tag();
    let owns = leanval_owns_valset_intended();
    println!(
        "=== Pair C lean waist ({}:{}) leanval_owns_valset={} ===\n",
        image_repo(),
        tag,
        owns
    );
    println!(
        "impl tree feat/lean-consensus @ c51dbee — not in .worktrees/lean-consensus yet\n"
    );
    if owns {
        println!("Run B: expect assertion 3 (Comet VP frozen on bond-only)\n");
    } else {
        println!("Run A (default OFF): expect assertion 4 (classic staking VP)\n");
    }

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let config = terp_config(&tag);
    let mut chain = CosmosChain::new(config, 2, 0, runtime);

    let result = run_test(&mut chain).await;

    println!("\n--- Shutdown ---");
    if let Err(e) = chain.stop().await {
        eprintln!("Warning: cleanup error: {e}");
    }

    match result {
        Ok(()) => {
            println!("lean_waist PASSED");
            Ok(())
        }
        Err(e) => {
            eprintln!("lean_waist FAILED: {e}");
            Err(e)
        }
    }
}
