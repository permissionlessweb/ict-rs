//! dao-contracts suites on a terp-core image.
//!
//! Stores the wasm under `crates/dao-contracts/artifacts` that the dao-testing
//! suites cover (core, proposal, voting, cw4, stake, pre-propose), then
//! instantiates `cw4_group` and lists its member.
//!
//! ```sh
//! cargo run -p ict-rs --example dao_testing --features docker,terp,testing
//! ```

use std::path::PathBuf;

use ict_rs::cosmwasm::CosmWasmExt;
use ict_rs::prelude::*;
use ict_rs::testing::{setup_chain, TestEnv};

const SUITES: &[&str] = &[
    "dao_dao_core.wasm",
    "dao_proposal_single.wasm",
    "dao_proposal_multiple.wasm",
    "dao_voting_cw4.wasm",
    "dao_voting_cw20_staked.wasm",
    "dao_voting_token_staked-default.wasm",
    "dao_pre_propose_single.wasm",
    "cw20_stake.wasm",
    "cw4_group.wasm",
];

fn code_id_from_tx(tx: &serde_json::Value) -> Option<String> {
    let events = tx.get("events")?.as_array()?;
    for ev in events {
        if ev.get("type").and_then(|t| t.as_str()) != Some("store_code") {
            continue;
        }
        for attr in ev.get("attributes")?.as_array()? {
            if attr.get("key").and_then(|k| k.as_str()) == Some("code_id") {
                if let Some(s) = attr.get("value").and_then(|v| v.as_str()) {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

async fn store_wasm(
    chain: &ict_rs::chain::cosmos::CosmosChain,
    key: &str,
    path: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let opts = chain.default_tx_opts().from(key);
    let out = chain
        .chain_exec_tx_with(&["tx", "wasm", "store", path], opts)
        .await?;
    let trimmed = out.stdout_str();
    let trimmed = trimmed.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "empty store stdout exit={} stderr={}",
            out.exit_code,
            out.stderr_str().trim()
        )
        .into());
    }
    let v: serde_json::Value = serde_json::from_str(trimmed)?;
    if v.get("code").and_then(|c| c.as_u64()).unwrap_or(0) != 0 {
        return Err(format!("store tx failed: {trimmed}").into());
    }
    let hash = v["txhash"]
        .as_str()
        .ok_or_else(|| format!("no txhash in {trimmed}"))?;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let q = chain
            .chain_exec(&["query", "tx", hash, "--output", "json"])
            .await?;
        if q.exit_code != 0 {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(q.stdout_str().trim()) else {
            continue;
        };
        let tv = if parsed.get("tx_response").is_some() {
            parsed["tx_response"].clone()
        } else {
            parsed
        };
        if let Some(id) = code_id_from_tx(&tv) {
            return Ok(id);
        }
    }
    Err(format!("timed out waiting for store tx {hash}").into())
}

fn dao_artifacts() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let marker = "crates/dao-contracts/artifacts/cw4_group.wasm";
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..8 {
        if dir.join(marker).is_file() {
            return Ok(dir.join("crates/dao-contracts/artifacts"));
        }
        if !dir.pop() {
            break;
        }
    }
    Err("missing crates/dao-contracts/artifacts/cw4_group.wasm".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("dao_testing uses the terp-core image. Unset ICT_MOCK.".into());
    }
    let artifacts = dao_artifacts()?;
    let mut tc = setup_chain("dao-testing", TestEnv::terp_config()).await?;
    let outcome = run(&mut tc, &artifacts).await;
    if let Err(e) = tc.cleanup().await {
        eprintln!("cleanup: {e}");
    }
    outcome?;
    println!("dao_testing passed");
    Ok(())
}

async fn run(
    tc: &mut ict_rs::testing::TestChain,
    artifacts: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    tc.chain.create_key("dao").await?;
    let addr = tc.chain.key_address("dao").await?;
    tc.chain
        .send_funds(
            "validator",
            &WalletAmount {
                address: addr.clone(),
                denom: "uterp".into(),
                amount: 50_000_000_000,
            },
        )
        .await?;

    let mut cw4_code = String::new();
    for name in SUITES {
        let host = artifacts.join(name);
        if !host.is_file() {
            return Err(format!("missing {}", host.display()).into());
        }
        let dest = format!("/tmp/{name}");
        ibc_wasm_path::put_file(&tc.chain, &dest, &std::fs::read(&host)?).await?;
        let code_id = store_wasm(&tc.chain, "dao", &dest).await?;
        println!("stored {name} code_id={code_id}");
        if *name == "cw4_group.wasm" {
            cw4_code = code_id;
        }
    }

    let init = format!(r#"{{"admin":"{addr}","members":[{{"addr":"{addr}","weight":1}}]}}"#);
    let group = tc
        .chain
        .instantiate_contract("dao", &cw4_code, &init, "dao-testing-cw4", Some(&addr))
        .await?;
    println!("cw4_group {group}");
    let members = tc
        .chain
        .query_contract(&group, r#"{"list_members":{}}"#)
        .await?;
    println!("cw4 members {members}");
    let raw = members.to_string();
    if !raw.contains(&addr) {
        return Err(format!("cw4 group did not list {addr}: {members}").into());
    }
    Ok(())
}
