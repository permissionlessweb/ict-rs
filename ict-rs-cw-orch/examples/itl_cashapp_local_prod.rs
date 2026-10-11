//! ITL Cash App ingress + local Terp via ict-rs. Refuses ICT_MOCK=1.
//! Only mock is payment mail (node e2e). Chain is Docker, not MockBech32.
//!
//! ```sh
//! # groot-wan
//! cargo run --example itl_cashapp_local_prod -p ict-rs-cw-orch --features docker
//! ```
//!
//! ZEC/ETH/BTC sidecars stay with `svg_mint_local_prod` — do not duplicate spawn.

use ict_rs::cosmos::interchain::wait_for_blocks;
use ict_rs::prelude::*;
use ict_rs_cw_orch::chain_info_from_cosmos;
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

fn refuse_mock() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("itl_cashapp_local_prod refuses ICT_MOCK=1".into());
    }
    Ok(())
}

fn itl_cashapp_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ITL_CASHAPP") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../itl/cashapp")
}

fn run_ingress_e2e() -> Result<(), Box<dyn std::error::Error>> {
    let dir = itl_cashapp_dir();
    let parse = dir.join("parse.test.mjs");
    let e2e = dir.join("e2e.test.mjs");
    if !e2e.is_file() {
        return Err(format!("missing {} (rsync itl/cashapp to groot-wan)", e2e.display()).into());
    }
    let st = Command::new("node")
        .args(["--test", parse.to_str().unwrap(), e2e.to_str().unwrap()])
        .status()?;
    if !st.success() {
        return Err("node cashapp e2e failed".into());
    }
    Ok(())
}

async fn spawn_terp() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let rt = Arc::new(DockerBackend::new(Default::default()).await?);
    let mut cfg = terp_chain_config();
    cfg.chain_id = "itl-cashapp-1".into();
    cfg.name = "itl-cashapp".into();
    cfg.genesis_style = GenesisStyle::Legacy;
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", "ict-itl-cashapp-itl-cashapp-1-val-0"])
        .status();
    let mut chain = CosmosChain::new(cfg, 1, 0, rt);
    let ctx = TestContext {
        test_name: "itl-cashapp".into(),
        network_id: "ict-itl-cashapp".into(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    wait_for_blocks(&chain, 2).await?;
    let info = chain_info_from_cosmos(&chain)?;
    let grpc = info.grpc_urls.clone();
    let _ = chain.stop().await;
    Ok(json!({
        "live": true,
        "ingress": true,
        "chain_id": "itl-cashapp-1",
        "grpc": grpc,
        "engine": "ict-rs+cw-orch-bridge",
        "mail_mock": "payment-received-eml-only",
        "door": "cw-orch MockBech32 cashapp_paid_guests (wasm-on-docker follow-on)",
    }))
}

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .init();
    if let Err(e) = refuse_mock() {
        eprintln!("{e}");
        println!("{}", json!({ "live": false, "error": e.to_string() }));
        std::process::exit(2);
    }
    if let Err(e) = run_ingress_e2e() {
        eprintln!("{e}");
        println!("{}", json!({ "live": false, "error": e.to_string(), "stage": "ingress" }));
        std::process::exit(2);
    }
    match spawn_terp().await {
        Ok(v) => {
            println!("{v}");
            if v.get("live") != Some(&json!(true)) {
                std::process::exit(2);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            println!(
                "{}",
                json!({ "live": false, "error": e.to_string(), "stage": "ict-rs-docker" })
            );
            std::process::exit(2);
        }
    }
}
