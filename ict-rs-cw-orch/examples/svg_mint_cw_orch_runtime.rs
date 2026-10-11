//! Local SVG mint workflow (default): spawn `registry.terp.network/terp-core:local-zk`,
//! upload hopalong minter + collection, mint 1 uterp, tear down.
//!
//! Against an already-running **local** chain:
//! ```sh
//! COLLECTION=terp1... TERP_GRPC=http://127.0.0.1:9090 CHAIN_ID=test-1 \
//!   DREGG_RUNTIME_MNEMONIC="..." cargo run --example svg_mint_cw_orch_runtime -p ict-rs-cw-orch
//! ```
//!
//! Mainnet is opt-in only (`TERP_MAINNET=1`). This binary will not default
//! gRPC/chain-id to morocco-1.

use cw_orch::prelude::*;
use cw_orch_core::environment::{ChainInfoOwned, ChainKind, NetworkInfoOwned};
use ict_rs::cosmos::interchain::wait_for_blocks;
use ict_rs::prelude::*;
use ict_rs_cw_orch::svg_runtime::{
    deploy_hopalong_minter, mint_via_daemon, native_svg_mint_msg,
};
use ict_rs_cw_orch::daemon_builder_from_chain;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

const LAB_MNEMONIC: &str =
    "notice oak worry limit wrap speak medal online prefer cluster roof addict wrist behave treat actual wasp year salad speed social layer crew genius";

fn artifacts_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SVG_MINT_ARTIFACTS") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cw-infuser/artifacts")
}

fn mainnet_requested() -> bool {
    std::env::var("TERP_MAINNET").ok().as_deref() == Some("1")
}

fn chain_info_existing() -> ChainInfoOwned {
    let mainnet = mainnet_requested();
    let grpc = if mainnet {
        std::env::var("TERP_GRPC")
            .or_else(|_| std::env::var("TERP_MAIN_GRPC"))
            .expect("TERP_MAINNET=1 requires TERP_GRPC (or TERP_MAIN_GRPC)")
    } else {
        std::env::var("TERP_GRPC").unwrap_or_else(|_| "http://127.0.0.1:9090".into())
    };
    let chain_id = std::env::var("CHAIN_ID").unwrap_or_else(|_| {
        if mainnet {
            "morocco-1".into()
        } else {
            "test-1".into()
        }
    });
    if chain_id == "morocco-1" && !mainnet {
        panic!("svg_mint_cw_orch_runtime refuses morocco-1 unless TERP_MAINNET=1");
    }
    ChainInfoOwned {
        chain_id: chain_id.clone(),
        gas_denom: std::env::var("GAS_DENOM").unwrap_or_else(|_| "uterp".into()),
        gas_price: 0.05,
        grpc_urls: vec![grpc],
        lcd_url: None,
        fcd_url: None,
        network_info: NetworkInfoOwned {
            chain_name: "terp".into(),
            pub_address_prefix: "terp".into(),
            coin_type: 118,
        },
        kind: if chain_id == "morocco-1" {
            ChainKind::Mainnet
        } else {
            ChainKind::Local
        },
    }
}

fn mint_existing() {
    let info = chain_info_existing();
    let mnemonic = dregg_svg_mint::hd::load_dregg_runtime_mnemonic()
        .expect("DREGG_RUNTIME_MNEMONIC or ~/.terp/dregg-svg-runtime.mnemonic");
    let minter = std::env::var("COLLECTION")
        .or_else(|_| std::env::var("MINTER"))
        .expect("COLLECTION (cw721_svg) terp1");
    let mut builder = DaemonBuilder::new(info);
    builder.mnemonic(mnemonic);
    let daemon = builder.build().expect("daemon");
    let sender = daemon.sender_addr().to_string();
    let funds = vec![Coin::new(1u128, "uterp")];
    match mint_via_daemon(&daemon, &minter, &native_svg_mint_msg(), &funds) {
        Ok(tx) => println!(
            "{}",
            json!({"ok": true, "tx": tx, "sender": sender, "minter": minter, "engine": "cw-orch", "mode": "existing"})
        ),
        Err(e) => {
            println!("{}", json!({"ok": false, "error": e, "sender": sender}));
            std::process::exit(2);
        }
    }
}

async fn spawn_local_and_mint() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let arts = artifacts_dir();
    if !arts.join("cw721_svg.wasm").is_file() || !arts.join("cw_svg_minter.wasm").is_file() {
        return Err(format!("missing cw721_svg.wasm / cw_svg_minter.wasm under {}", arts.display()).into());
    }
    let rt = Arc::new(DockerBackend::new(Default::default()).await?);
    let mut cfg = terp_chain_config();
    cfg.chain_id = "test-1".into();
    cfg.name = "svg-mint-runtime".into();
    cfg.genesis_style = GenesisStyle::Legacy;
    cfg.faucet = Some(FaucetConfig {
        key_name: "deployer".into(),
        port: 4500,
        start_cmd: vec![],
        env: vec![],
        mnemonic: Some(LAB_MNEMONIC.into()),
        coins: Some("100000000000uterp".into()),
    });
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", "ict-svg-mint-runtime-test-1-val-0"])
        .status();
    let mut chain = CosmosChain::new(cfg, 1, 0, rt);
    let ctx = TestContext {
        test_name: "svg-mint-runtime".into(),
        network_id: "ict-svg-mint-runtime".into(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    wait_for_blocks(&chain, 2).await?;
    let builder = daemon_builder_from_chain(&chain, Some(LAB_MNEMONIC))?;
    let arts_move = arts.clone();
    let denom = chain.config().denom.clone();
    let minted = std::thread::spawn(move || -> Result<serde_json::Value, String> {
        let daemon = builder
            .build()
            .map_err(|e| format!("DaemonBuilder.build: {e}"))?;
        let d = deploy_hopalong_minter(&daemon, &arts_move, &denom, 1_000_000)?;
        let sender = daemon.sender_addr().to_string();
        let funds = vec![Coin::new(dregg_svg_mint::prices::NATIVE_AMOUNT, "uterp")];
        let tx = mint_via_daemon(
            &daemon,
            &d.collection_addr,
            &native_svg_mint_msg(),
            &funds,
        )?;
        Ok(json!({
            "ok": true,
            "tx": tx,
            "sender": sender,
            "minter": d.minter_addr,
            "collection": d.collection_addr,
            "engine": "cw-orch",
            "mode": "local-spawn",
            "chain_id": "test-1",
            "image": format!("{}:{}", ict_rs::image_env::terp_image_repo(), ict_rs::image_env::terp_image_version())
        }))
    })
    .join()
    .map_err(|_| "cw-orch thread panicked")??;
    let _ = chain.stop().await;
    Ok(minted)
}

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    if mainnet_requested() || std::env::var("COLLECTION").is_ok() || std::env::var("MINTER").is_ok()
    {
        mint_existing();
        return;
    }
    match spawn_local_and_mint().await {
        Ok(v) => {
            println!("{v}");
            if v.get("ok") != Some(&serde_json::Value::Bool(true)) {
                std::process::exit(2);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            println!("{}", json!({"ok": false, "error": e.to_string(), "mode": "local-spawn"}));
            std::process::exit(2);
        }
    }
}
