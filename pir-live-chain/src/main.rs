//! Spawn a Terp CosmosChain, call daemon_builder_from_chain, store/query a packet on-chain.

use ict_rs::prelude::*;
use ict_rs_cw_orch::daemon_builder_from_chain;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;

fn commit_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[tokio::main]
async fn main() {
    match run().await {
        Ok(v) => {
            println!("{v}");
        }
        Err(e) => {
            eprintln!("{e}");
            println!(
                "{}",
                serde_json::json!({
                    "live": false,
                    "error": e,
                    "called_daemon_builder_from_chain": e.contains("after daemon_builder")
                })
            );
            std::process::exit(2);
        }
    }
}

async fn run() -> Result<String, String> {
    let rt = Arc::new(
        DockerBackend::new(Default::default())
            .await
            .map_err(|e| format!("docker backend: {e}"))?,
    );
    let mut cfg = terp_chain_config();
    if let Ok(img) = std::env::var("PIR_TERP_IMAGE") {
        if let Some((repo, ver)) = img.rsplit_once(':') {
            cfg.images = vec![DockerImage {
                repository: repo.into(),
                version: ver.into(),
                uid_gid: None,
            }];
        }
    }
    cfg.chain_id = "pir-live-1".into();
    cfg.name = "pir-live".into();
    cfg.genesis_style = GenesisStyle::Legacy;
    cfg.gas_prices = "0.025uterp".into();

    let _ = std::process::Command::new("docker")
        .args([
            "rm",
            "-f",
            "ict-pir-live-attach-pir-live-1-val-0",
        ])
        .status();

    let mut chain = CosmosChain::new(cfg, 1, 0, rt);
    let ctx = TestContext {
        test_name: "pir-live-attach".into(),
        network_id: "ict-pir-live".into(),
    };
    chain
        .initialize(&ctx)
        .await
        .map_err(|e| format!("initialize: {e}"))?;
    chain
        .start(&[])
        .await
        .map_err(|e| format!("start: {e}"))?;

    let _builder = daemon_builder_from_chain(&chain, None)
        .map_err(|e| format!("daemon_builder_from_chain: {e}"))?;

    tokio::time::sleep(std::time::Duration::from_secs(6)).await;

    let wasm_host = wasm_path()?;
    let node = chain
        .primary_node()
        .map_err(|e| format!("after daemon_builder: primary_node: {e}"))?;
    let cid = node
        .container_id
        .as_ref()
        .ok_or_else(|| "after daemon_builder: no container_id".to_string())?;
    let st = std::process::Command::new("docker")
        .args(["cp", &wasm_host, &format!("{}:/tmp/cw_pir_commit.wasm", cid.0)])
        .status()
        .map_err(|e| format!("after daemon_builder: docker cp: {e}"))?;
    if !st.success() {
        return Err(format!("after daemon_builder: docker cp failed {st}"));
    }

    let home = format!("/var/cosmos-chain/{}", chain.chain_id());
    let code_id = store_via_cli(&cid.0, &home)?;
    let addr = instantiate_via_cli(&cid.0, &home, &code_id)?;

    let ask_hex = std::env::var("PIR_ASK_COMMIT").map_err(|_| {
        "after daemon_builder: PIR_ASK_COMMIT missing (OOB ask commitment hex)".to_string()
    })?;
    let bid_hex = std::env::var("PIR_BID_COMMIT").map_err(|_| {
        "after daemon_builder: PIR_BID_COMMIT missing (OOB bid commitment hex)".to_string()
    })?;
    if ask_hex.len() != 64 || bid_hex.len() != 64 {
        return Err("after daemon_builder: commitments must be 32-byte hex".into());
    }
    let low = format!("{ask_hex}{bid_hex}").to_ascii_lowercase();
    if low.contains("price") || low.contains("identity") {
        return Err("after daemon_builder: price/identity must not appear in commitments".into());
    }

    exec_via_cli(
        &cid.0,
        &home,
        &addr,
        &serde_json::json!({
            "post_commitments": {
                "ask_commitment": ask_hex,
                "bid_commitment": bid_hex
            }
        })
        .to_string(),
    )?;

    let stored = query_via_cli(
        &cid.0,
        &home,
        &addr,
        &serde_json::json!({"get_commitments":{}}).to_string(),
    )?;
    let stored_low = stored.to_ascii_lowercase();
    let no_price_on_chain = !stored_low.contains("price")
        && !stored_low.contains("identity")
        && !stored.contains("uakt");
    let match_ok = query_via_cli(
        &cid.0,
        &home,
        &addr,
        &serde_json::json!({
            "match": {"ask_commitment": ask_hex, "bid_commitment": bid_hex}
        })
        .to_string(),
    )?;
    let mut bad_bid = bid_hex.clone();
    let last = bad_bid.pop().unwrap();
    bad_bid.push(if last == '0' { '1' } else { '0' });
    let match_bad = query_via_cli(
        &cid.0,
        &home,
        &addr,
        &serde_json::json!({
            "match": {"ask_commitment": ask_hex, "bid_commitment": bad_bid}
        })
        .to_string(),
    )?;
    let oob_bid_matches =
        match_ok.contains("true") && stored.contains(&bid_hex) && stored.contains(&ask_hex);
    let mismatch_rejected =
        match_bad.contains("false") && !match_bad.contains("\"matches\":true");
    let tamper_rejected = oob_bid_matches && mismatch_rejected;

    Ok(serde_json::json!({
        "live": true,
        "chain_id": chain.chain_id(),
        "called_daemon_builder_from_chain": true,
        "contract": addr,
        "stored_matches_good": oob_bid_matches,
        "tamper_rejected": tamper_rejected,
        "oob_bid_matches": oob_bid_matches,
        "mismatch_rejected": mismatch_rejected,
        "no_price_on_chain": no_price_on_chain
    })
    .to_string())
}

fn terpd_tx(cid: &str, extra: &[&str]) -> Result<String, String> {
    let mut args = vec![
        "exec",
        cid,
        "terpd",
    ];
    args.extend_from_slice(extra);
    let out = std::process::Command::new("docker")
        .args(&args)
        .output()
        .map_err(|e| format!("after daemon_builder: docker exec: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        return Err(format!(
            "after daemon_builder: terpd exit={} stdout={} stderr={}",
            out.status,
            stdout.trim(),
            stderr.trim()
        ));
    }
    Ok(stdout)
}

fn store_via_cli(cid: &str, home: &str) -> Result<String, String> {
    terpd_tx(
        cid,
        &[
            "tx",
            "wasm",
            "store",
            "/tmp/cw_pir_commit.wasm",
            "--from",
            "validator",
            "--keyring-backend",
            "test",
            "--home",
            home,
            "--chain-id",
            "pir-live-1",
            "--gas",
            "auto",
            "--gas-adjustment",
            "2.0",
            "--gas-prices",
            "0.025uterp",
            "--broadcast-mode",
            "sync",
            "--output",
            "json",
            "-y",
        ],
    )?;
    wait_code_id(cid, home)
}

fn wait_code_id(cid: &str, home: &str) -> Result<String, String> {
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let out = terpd_tx(
            cid,
            &[
                "q",
                "wasm",
                "list-code",
                "--home",
                home,
                "--output",
                "json",
            ],
        )?;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(out.trim()) {
            if let Some(id) = v["code_infos"]
                .as_array()
                .and_then(|a| a.last())
                .and_then(|c| c["code_id"].as_str().map(|s| s.to_string()).or_else(|| {
                    c["code_id"].as_u64().map(|n| n.to_string())
                }))
            {
                return Ok(id);
            }
        }
    }
    Err("after daemon_builder: no code_id after store".into())
}

fn instantiate_via_cli(cid: &str, home: &str, code_id: &str) -> Result<String, String> {
    terpd_tx(
        cid,
        &[
            "tx",
            "wasm",
            "instantiate",
            code_id,
            "{}",
            "--label",
            "pir-pkt",
            "--no-admin",
            "--from",
            "validator",
            "--keyring-backend",
            "test",
            "--home",
            home,
            "--chain-id",
            "pir-live-1",
            "--gas",
            "auto",
            "--gas-adjustment",
            "2.0",
            "--gas-prices",
            "0.025uterp",
            "--broadcast-mode",
            "sync",
            "--output",
            "json",
            "-y",
        ],
    )?;
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let out = terpd_tx(
            cid,
            &[
                "q",
                "wasm",
                "list-contract-by-code",
                code_id,
                "--home",
                home,
                "--output",
                "json",
            ],
        )?;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(out.trim()) {
            if let Some(addr) = v["contracts"].as_array().and_then(|a| a.first()).and_then(|x| x.as_str()) {
                return Ok(addr.to_string());
            }
        }
    }
    Err("after daemon_builder: no contract after instantiate".into())
}

fn exec_via_cli(cid: &str, home: &str, addr: &str, msg: &str) -> Result<(), String> {
    terpd_tx(
        cid,
        &[
            "tx",
            "wasm",
            "execute",
            addr,
            msg,
            "--from",
            "validator",
            "--keyring-backend",
            "test",
            "--home",
            home,
            "--chain-id",
            "pir-live-1",
            "--gas",
            "auto",
            "--gas-adjustment",
            "2.0",
            "--gas-prices",
            "0.025uterp",
            "--broadcast-mode",
            "sync",
            "--output",
            "json",
            "-y",
        ],
    )?;
    std::thread::sleep(std::time::Duration::from_secs(3));
    Ok(())
}

fn query_via_cli(cid: &str, home: &str, addr: &str, msg: &str) -> Result<String, String> {
    terpd_tx(
        cid,
        &[
            "q",
            "wasm",
            "contract-state",
            "smart",
            addr,
            msg,
            "--home",
            home,
            "--output",
            "json",
        ],
    )
}

fn wasm_path() -> Result<String, String> {
    if let Ok(p) = std::env::var("PIR_COMMIT_WASM") {
        return Ok(p);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for rel in [
        "../../../private-inference-rent/cw-commit/artifacts/cw_pir_commit.wasm",
        "../../../private-inference-rent/cw-commit/target/wasm32-unknown-unknown/release/cw_pir_commit.wasm",
    ] {
        let p = root.join(rel);
        if p.is_file() {
            return Ok(p.display().to_string());
        }
    }
    Err("missing cw_pir_commit.wasm (run terp optimizer on cw-commit)".into())
}
