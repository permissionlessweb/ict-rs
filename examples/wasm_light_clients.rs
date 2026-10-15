//! Govern-store the custom 08-wasm light clients, including zap1, on a terp-core image.
//!
//! The store runs inside the node, so a "section out of order" failure from the
//! instrumenter shows up as a rejected proposal rather than a host-side check.
//!
//! ```sh
//! cargo run -p ict-rs --example wasm_light_clients --features docker,terp,testing
//! ```

use std::path::PathBuf;

use ict_rs::prelude::*;
use ict_rs::testing::{setup_chain, TestEnv};

const CLIENTS: &[&str] = &[
    "cw_ics08_wasm_terp.wasm",
    "cw_ics08_wasm_zap1.wasm",
    "cw_ics08_wasm_dregg.wasm",
    "cw_ics08_wasm_crosslink.wasm",
];

fn artifacts() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let marker = "crates/terp-rs/artifacts/cw_ics08_wasm_terp.wasm";
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..8 {
        if dir.join(marker).is_file() {
            return Ok(dir.join("crates/terp-rs/artifacts"));
        }
        if !dir.pop() {
            break;
        }
    }
    Err("missing crates/terp-rs/artifacts/cw_ics08_wasm_terp.wasm".into())
}

fn fast_gov(
    _cfg: &ict_rs::chain::ChainConfig,
    raw: Vec<u8>,
) -> ict_rs::error::Result<Vec<u8>> {
    let mut genesis: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| ict_rs::error::IctError::Config(format!("parse genesis: {e}")))?;
    if let Some(params) = genesis.pointer_mut("/app_state/gov/params") {
        params["min_deposit"] =
            serde_json::json!([{"denom": "uterp", "amount": "10000000"}]);
        params["expedited_min_deposit"] =
            serde_json::json!([{"denom": "uterp", "amount": "20000000"}]);
        params["voting_period"] = serde_json::json!("60s");
        params["expedited_voting_period"] = serde_json::json!("30s");
        params["max_deposit_period"] = serde_json::json!("30s");
    }
    if let Some(dp) = genesis.pointer_mut("/app_state/gov/deposit_params") {
        dp["min_deposit"] = serde_json::json!([{"denom": "uterp", "amount": "10000000"}]);
        dp["max_deposit_period"] = serde_json::json!("30s");
    }
    if let Some(vp) = genesis.pointer_mut("/app_state/gov/voting_params") {
        vp["voting_period"] = serde_json::json!("60s");
    }
    serde_json::to_vec(&genesis)
        .map_err(|e| ict_rs::error::IctError::Config(format!("encode genesis: {e}")))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("wasm_light_clients uses the terp-core image. Unset ICT_MOCK.".into());
    }
    let artifacts = artifacts()?;
    // TestEnv's genesis style matches this image. terp_chain_config's modern
    // style passes --chain-id to `terpd genesis`, which this binary rejects.
    let mut cfg = TestEnv::terp_config();
    cfg.modify_genesis = Some(Box::new(fast_gov));
    cfg.name = "wasm-lc".into();
    let mut tc = setup_chain("wasm-lc", cfg).await?;
    let outcome = run(&mut tc, &artifacts).await;
    if let Err(e) = tc.cleanup().await {
        eprintln!("cleanup: {e}");
    }
    outcome?;
    println!("wasm_light_clients passed");
    Ok(())
}

async fn vote_yes(
    chain: &CosmosChain,
    proposal_id: u64,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let id = proposal_id.to_string();
    let opts = chain.default_tx_opts().from("validator").gas("400000");
    let output = chain
        .chain_exec_tx_with(&["tx", "gov", "vote", &id, "yes"], opts)
        .await
        .map_err(|e| format!("vote {name}: {e}"))?;
    let raw = format!("{}{}", output.stdout_str(), output.stderr_str());
    let parsed: Option<serde_json::Value> =
        serde_json::from_str(output.stdout_str().trim()).ok();
    let code = parsed.as_ref().and_then(|v| v["code"].as_u64()).unwrap_or(0);
    let txhash = parsed
        .as_ref()
        .and_then(|v| v["txhash"].as_str())
        .unwrap_or("")
        .to_string();
    println!(
        "vote {name} proposal {proposal_id} checktx code={code} tx={txhash} exit={}",
        output.exit_code
    );
    if output.exit_code != 0 || code != 0 || txhash.is_empty() {
        return Err(format!("vote {name} proposal {proposal_id} failed: {raw}").into());
    }
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let q = chain
        .chain_exec(&["query", "tx", &txhash, "--output", "json"])
        .await
        .map_err(|e| format!("query vote tx {name}: {e}"))?;
    let body = q.stdout_str();
    let deliver = serde_json::from_str::<serde_json::Value>(body.trim()).ok();
    let deliver_code = deliver.as_ref().and_then(|v| v["code"].as_u64()).unwrap_or(0);
    let log = deliver
        .as_ref()
        .and_then(|v| v["raw_log"].as_str())
        .unwrap_or(body.trim());
    println!("vote {name} deliver code={deliver_code} log={log}");
    if q.exit_code != 0 || deliver_code != 0 {
        return Err(format!("vote {name} not delivered: {log}").into());
    }
    Ok(())
}

async fn wait_status(
    chain: &CosmosChain,
    proposal_id: u64,
    name: &str,
    want: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = std::time::Instant::now();
    let mut last = String::new();
    loop {
        if start.elapsed() > std::time::Duration::from_secs(45) {
            return Err(format!("{name} proposal {proposal_id} never reached {want}: {last}").into());
        }
        match chain.query_proposal(proposal_id).await {
            Ok(v) => {
                last = v.to_string();
                let status = v["proposal"]["status"]
                    .as_str()
                    .or_else(|| v["status"].as_str())
                    .unwrap_or("");
                println!("{name} proposal {proposal_id} status={status}");
                if status == want || status == "PROPOSAL_STATUS_PASSED" {
                    return Ok(());
                }
                if status == "PROPOSAL_STATUS_FAILED" || status == "PROPOSAL_STATUS_REJECTED" {
                    return Err(format!("{name} proposal {proposal_id} {status}: {last}").into());
                }
            }
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn wait_passed(
    chain: &CosmosChain,
    proposal_id: u64,
    name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let start = std::time::Instant::now();
    let mut last = String::new();
    loop {
        if start.elapsed() > std::time::Duration::from_secs(120) {
            return Err(format!("{name} proposal {proposal_id} not passed in 120s: {last}").into());
        }
        match chain.query_proposal(proposal_id).await {
            Ok(v) => {
                last = v.to_string();
                let status = v["proposal"]["status"]
                    .as_str()
                    .or_else(|| v["status"].as_str())
                    .unwrap_or("");
                if status == "PROPOSAL_STATUS_PASSED" {
                    return Ok(last);
                }
                if status == "PROPOSAL_STATUS_FAILED" || status == "PROPOSAL_STATUS_REJECTED" {
                    return Err(format!("{name} proposal {proposal_id} {status}: {last}").into());
                }
            }
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn run(
    tc: &mut ict_rs::testing::TestChain,
    artifacts: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let before = tc.chain.query_ibc_wasm_checksums().await.unwrap_or_default();
    println!("checksums before: {}", before.len());
    for name in CLIENTS {
        let host = artifacts.join(name);
        if !host.is_file() {
            return Err(format!("missing {}", host.display()).into());
        }
        let dest = format!("/tmp/{name}");
        ibc_wasm_path::put_file(&tc.chain, &dest, &std::fs::read(&host)?).await?;
        let prop = IbcWasmStoreProposal {
            wasm_path: dest,
            title: format!("store {name}"),
            summary: format!("08-wasm {name}"),
            deposit: "10000000uterp".into(),
            authority: None,
            metadata: None,
        };
        let (proposal_id, tx_hash) = tc
            .chain
            .submit_ibc_wasm_store_code("validator", &prop)
            .await
            .map_err(|e| format!("submit {name}: {e}"))?;
        wait_status(&tc.chain, proposal_id, name, "PROPOSAL_STATUS_VOTING_PERIOD").await?;
        vote_yes(&tc.chain, proposal_id, name).await?;
        let passed = wait_passed(&tc.chain, proposal_id, name).await?;
        if passed.to_lowercase().contains("section out of order") {
            return Err(format!("{name} section out of order: {passed}").into());
        }
        let checksums = tc.chain.query_ibc_wasm_checksums().await.unwrap_or_default();
        println!("stored {name} proposal={proposal_id} tx={tx_hash} checksums={}", checksums.len());
    }
    let after = tc.chain.query_ibc_wasm_checksums().await?;
    if after.len() < before.len() + CLIENTS.len() {
        return Err(format!(
            "expected {} new checksums, before={} after={after:?}",
            CLIENTS.len(),
            before.len()
        )
        .into());
    }
    println!("checksums after: {after:?}");
    Ok(())
}
