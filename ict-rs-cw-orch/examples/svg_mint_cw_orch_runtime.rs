//! Lightweight DREGG mint runtime: cw-orch Daemon only (no terpd).
//!
//! ```sh
//! DREGG_RUNTIME_MNEMONIC="..." TERP_GRPC=http://127.0.0.1:9090 \
//!   MINTER=terp1... cargo run --example svg_mint_cw_orch_runtime -p ict-rs-cw-orch
//! ```
//! Morocco-1: TERP_MAIN_GRPC + MAIN_MNEMONIC (never on testnet).

use cw_orch::prelude::*;
use cw_orch_core::environment::{ChainInfoOwned, ChainKind, NetworkInfoOwned};
use ict_rs_cw_orch::svg_runtime::{mint_via_daemon, native_svg_mint_msg};
use serde_json::json;

fn chain_info() -> ChainInfoOwned {
    let grpc = std::env::var("TERP_GRPC")
        .or_else(|_| std::env::var("TERP_MAIN_GRPC"))
        .unwrap_or_else(|_| "http://127.0.0.1:9090".into());
    let chain_id = std::env::var("CHAIN_ID").unwrap_or_else(|_| "test-1".into());
    let denom = std::env::var("GAS_DENOM").unwrap_or_else(|_| "uterp".into());
    if chain_id == "morocco-1" && std::env::var("TERP_MAINNET").ok().as_deref() != Some("1") {
        panic!("svg_mint_cw_orch_runtime defaults to local test-1; set TERP_MAINNET=1 to touch morocco-1");
    }
    let kind = if chain_id == "morocco-1" {
        ChainKind::Mainnet
    } else {
        ChainKind::Local
    };
    ChainInfoOwned {
        chain_id,
        gas_denom: denom,
        gas_price: 0.05,
        grpc_urls: vec![grpc],
        lcd_url: None,
        fcd_url: None,
        network_info: NetworkInfoOwned {
            chain_name: "terp".into(),
            pub_address_prefix: "terp".into(),
            coin_type: 118,
        },
        kind,
    }
}

fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mnemonic = dregg_svg_mint::hd::load_dregg_runtime_mnemonic()
        .expect("DREGG_RUNTIME_MNEMONIC or ~/.terp/dregg-svg-runtime.mnemonic");
    let minter = std::env::var("COLLECTION")
        .or_else(|_| std::env::var("MINTER"))
        .expect("COLLECTION (cw721_svg) terp1 — paid mint is update_extension on the collection");
    let mut builder = DaemonBuilder::new(chain_info());
    builder.mnemonic(mnemonic);
    let daemon = builder.build().expect("daemon");
    let sender = daemon.sender_addr().to_string();
    let funds = vec![Coin::new(1u128, "uterp")];
    match mint_via_daemon(&daemon, &minter, &native_svg_mint_msg(), &funds) {
        Ok(tx) => println!(
            "{}",
            json!({"ok": true, "tx": tx, "sender": sender, "minter": minter, "engine": "cw-orch"})
        ),
        Err(e) => {
            println!("{}", json!({"ok": false, "error": e, "sender": sender}));
            std::process::exit(2);
        }
    }
}
