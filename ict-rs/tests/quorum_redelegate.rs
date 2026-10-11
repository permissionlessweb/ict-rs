//! Three-validator terp chain. A foundation key delegates to two validators,
//! then one next-block redelegation puts the third strictly above one third.
//!
//! Skips unless `TERP_IMAGE_VERSION` (or `ICT_IMAGE_VERSION`) is set, so a
//! default `cargo test -p ict-rs` does not start Docker. The image is
//! whatever tag is already local; rerun with the patched tag when it exists.
//!
//! Genesis self-bonds are 5_000_000 TERP each, so each validator is exactly
//! one third. A 1 TERP create-validator cannot cross that line with the
//! liquid this test can move. The destination is validator 2, whose gentx
//! already happened. Create-validator stays a separate file in the dry-run
//! binary; this exercise is the redelegation that changes voting power.

use std::time::Duration;

use cw_orch_ibc::quorum::{plan_next_block, QuorumSnapshot, StakeSource, FOUNDATION_DELEGATOR};
use ict_rs::node::ChainNode;
use ict_rs::testing::{TestChain, TestChainConfig, TestEnv};
use ict_rs::tx::ExecOutput;

const FUND: u128 = 4_000_100_000_000;
const EACH: u128 = 2_000_000_000_000;

#[tokio::test]
async fn redelegate_puts_validator_two_above_one_third() {
    if std::env::var("TERP_IMAGE_VERSION").is_err() && std::env::var("ICT_IMAGE_VERSION").is_err() {
        eprintln!("skip: set TERP_IMAGE_VERSION to an existing terp-core tag");
        return;
    }

    let mut chain = TestChain::setup(
        "quorum",
        TestChainConfig {
            chain_config: TestEnv::terp_config(),
            num_validators: 3,
            num_full_nodes: 0,
            genesis_wallets: Vec::new(),
        },
    )
    .await
    .expect("3-validator chain");

    let result = exercise(&chain).await;
    let passed = result.is_ok();
    if let Err(err) = &result {
        eprintln!("quorum exercise failed: {err:#}");
    }
    chain.cleanup_with_result(passed).await.expect("cleanup");
    result.expect("redelegation crosses one third");
}

async fn exercise(chain: &TestChain) -> anyhow::Result<()> {
    anyhow::ensure!(
        chain.validators().len() == 3,
        "expected 3 validators, got {}",
        chain.validators().len()
    );
    let primary = &chain.validators()[0];
    wait_height(primary, 1).await?;

    let mut operators = Vec::new();
    for node in chain.validators() {
        operators.push(valoper(node).await?);
    }
    let dest = operators[2].clone();
    eprintln!("operators {operators:?} dest {dest}");

    let created = primary.create_key("foundation", 118).await?;
    anyhow::ensure!(
        created.exit_code == 0,
        "create foundation key: {}",
        created.stderr_str()
    );
    let foundation = primary.get_key_address("foundation").await?;
    anyhow::ensure!(!foundation.is_empty(), "foundation address empty");
    eprintln!("foundation {foundation} (mainnet wallet is {FOUNDATION_DELEGATOR})");

    let sent = primary
        .bank_send(
            "validator",
            &foundation,
            &format!("{FUND}uterp"),
            &primary.gas_prices,
        )
        .await?;
    tx_ok("fund", &sent)?;
    let balance = primary.query_balance(&foundation, "uterp").await?;
    anyhow::ensure!(balance >= EACH * 2, "foundation balance {balance}");

    for op in &operators[..2] {
        let opts = primary.default_tx_opts().from("foundation");
        let amount = format!("{EACH}uterp");
        let delegated = primary
            .exec_tx_with(&["tx", "staking", "delegate", op, &amount], opts)
            .await?;
        tx_ok("delegate", &delegated)?;
    }

    let rows = bonded(primary).await?;
    let total: u128 = rows.iter().map(|(_, tokens)| tokens).sum();
    let dest_bonded = rows
        .iter()
        .find(|(op, _)| op == &dest)
        .map(|(_, tokens)| *tokens)
        .unwrap_or(0);
    let mut sources = Vec::new();
    for op in &operators[..2] {
        let amount = delegation_of(primary, &foundation, op).await?;
        sources.push(StakeSource {
            validator: op.clone(),
            redelegable: amount,
        });
    }
    let snapshot = QuorumSnapshot {
        delegator: foundation.clone(),
        dest_operator: dest.clone(),
        dest_bonded,
        total_bonded: total,
        sources,
        liquid: primary.query_balance(&foundation, "uterp").await?,
        denom: "uterp".into(),
    };
    let plan = plan_next_block(&snapshot)?;
    eprintln!(
        "plan redelegates {:?} delegate {} dest {} -> {} total {} -> {}",
        plan.redelegates, plan.delegate, dest_bonded, plan.dest_after, total, plan.total_after
    );
    anyhow::ensure!(
        plan.delegate == 0,
        "local stake should clear one third by redelegate alone"
    );
    anyhow::ensure!(!plan.redelegates.is_empty(), "expected a redelegate");
    anyhow::ensure!(plan.above_one_third(), "plan is not above one third");

    let height_before = primary.query_height().await?;
    // One transaction. exec_tx waits until the next block commits.
    anyhow::ensure!(
        plan.redelegates.len() == 1,
        "this chain shape produces one source; got {}",
        plan.redelegates.len()
    );
    let redelegate = &plan.redelegates[0];
    let amount = format!("{}uterp", redelegate.amount);
    let opts = primary.default_tx_opts().from("foundation");
    let moved = primary
        .exec_tx_with(
            &[
                "tx",
                "staking",
                "redelegate",
                &redelegate.src,
                &dest,
                &amount,
            ],
            opts,
        )
        .await?;
    tx_ok("redelegate", &moved)?;
    let height_after = primary.query_height().await?;
    anyhow::ensure!(
        height_after > height_before,
        "redelegate did not commit a block ({height_before} -> {height_after})"
    );

    let after = bonded(primary).await?;
    let total_after: u128 = after.iter().map(|(_, tokens)| tokens).sum();
    let dest_after = after
        .iter()
        .find(|(op, _)| op == &dest)
        .map(|(_, tokens)| *tokens)
        .unwrap_or(0);
    eprintln!(
        "height {height_before} -> {height_after} dest {dest_bonded} -> {dest_after} total {total} -> {total_after}"
    );
    anyhow::ensure!(
        total_after == total,
        "redelegate must not change total bonded ({total} -> {total_after})"
    );
    anyhow::ensure!(
        dest_after.saturating_mul(3) > total_after,
        "dest {dest_after} is not above one third of {total_after}"
    );
    Ok(())
}

async fn wait_height(node: &ChainNode, min: u64) -> anyhow::Result<()> {
    for _ in 0..40 {
        if node.query_height().await.unwrap_or(0) >= min {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    anyhow::bail!("chain did not reach height {min}")
}

async fn valoper(node: &ChainNode) -> anyhow::Result<String> {
    let out = node
        .exec_cmd(&[
            "keys",
            "show",
            "validator",
            "--bech",
            "val",
            "-a",
            "--keyring-backend",
            "test",
        ])
        .await?;
    anyhow::ensure!(out.exit_code == 0, "valoper: {}", out.stderr_str());
    let addr = out.stdout_str().trim().to_string();
    anyhow::ensure!(addr.starts_with("terpvaloper"), "unexpected valoper {addr}");
    Ok(addr)
}

async fn bonded(node: &ChainNode) -> anyhow::Result<Vec<(String, u128)>> {
    let out = node
        .exec_cmd(&["query", "staking", "validators", "--output", "json"])
        .await?;
    anyhow::ensure!(out.exit_code == 0, "validators: {}", out.stderr_str());
    let parsed: serde_json::Value = serde_json::from_str(out.stdout_str().trim())?;
    let mut rows = Vec::new();
    for val in parsed["validators"].as_array().cloned().unwrap_or_default() {
        if val["status"].as_str() != Some("BOND_STATUS_BONDED") {
            continue;
        }
        let op = val["operator_address"].as_str().unwrap_or("").to_string();
        let tokens = val["tokens"].as_str().unwrap_or("0").parse::<u128>()?;
        rows.push((op, tokens));
    }
    Ok(rows)
}

async fn delegation_of(node: &ChainNode, delegator: &str, validator: &str) -> anyhow::Result<u128> {
    let out = node
        .exec_cmd(&[
            "query",
            "staking",
            "delegation",
            delegator,
            validator,
            "--output",
            "json",
        ])
        .await?;
    anyhow::ensure!(
        out.exit_code == 0,
        "delegation {validator}: {} {}",
        out.stdout_str(),
        out.stderr_str()
    );
    let parsed: serde_json::Value = serde_json::from_str(out.stdout_str().trim())?;
    let amount = parsed
        .pointer("/delegation_response/balance/amount")
        .or_else(|| parsed.pointer("/balance/amount"))
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    Ok(amount.parse()?)
}

fn tx_ok(label: &str, out: &ExecOutput) -> anyhow::Result<()> {
    anyhow::ensure!(
        out.exit_code == 0,
        "{label} exit {}: {} {}",
        out.exit_code,
        out.stdout_str(),
        out.stderr_str()
    );
    let text = out.stdout_str();
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text.trim()) {
        if let Some(code) = parsed.get("code").and_then(|c| c.as_u64()) {
            anyhow::ensure!(code == 0, "{label} code {code}: {parsed}");
        }
    }
    Ok(())
}
