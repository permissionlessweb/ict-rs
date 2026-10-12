//! Upload fresh `cw721_svg.wasm` and point the live minter at it.
//! No migrate. Existing minter instance stays on code 72.
//!
//! Admin execute: `{ "update_code_id": { "code_id": <new_svg> } }`
//! New collections instantiate from the uploaded svg code.
//!
//! Signer: `~/.terp/svg-minter-admin.mnemonic` (derived `terp1fxcc…`).
//! Addresses are derived. Do not type bech32.
//!
//! ```sh
//! cargo run --example svg_mint_update_svg_code -p ict-rs-cw-orch
//! SVG_CODE_UPLOAD=1 cargo run --example svg_mint_update_svg_code -p ict-rs-cw-orch
//! SVG_CODE_UPLOAD=1 UPDATE_BROADCAST=1 cargo run --example svg_mint_update_svg_code -p ict-rs-cw-orch
//! ```
//! Morocco-1 only. gRPC default `http://127.0.0.1:9090` (local terpd-mainnet).

use cw_orch::prelude::*;
use cw_orch_core::environment::{ChainInfoOwned, ChainKind, NetworkInfoOwned};
use serde_json::json;
use std::path::PathBuf;

fn morocco_info() -> ChainInfoOwned {
    let grpc = std::env::var("TERP_MAIN_GRPC").unwrap_or_else(|_| "http://127.0.0.1:9090".into());
    let denom = std::env::var("GAS_DENOM").unwrap_or_else(|_| "uthiol".into());
    ChainInfoOwned {
        chain_id: "morocco-1".into(),
        gas_denom: denom,
        gas_price: 0.05,
        grpc_urls: vec![grpc],
        lcd_url: Some("https://api.terp.network".into()),
        fcd_url: None,
        network_info: NetworkInfoOwned {
            chain_name: "terp".into(),
            pub_address_prefix: "terp".into(),
            coin_type: 118,
        },
        kind: ChainKind::Mainnet,
    }
}

fn morocco_from_state(path: &PathBuf) -> Result<(u64, String, u64, String), String> {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(path).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let m = v.get("morocco-1").ok_or("state.json missing morocco-1")?;
    Ok((
        m.pointer("/code_ids/cw-svg-minter")
            .and_then(|x| x.as_u64())
            .ok_or("code_ids.cw-svg-minter")?,
        m.pointer("/default/cw-svg-minter")
            .and_then(|x| x.as_str())
            .ok_or("default.cw-svg-minter")?
            .into(),
        m.pointer("/code_ids/cw721_svg")
            .and_then(|x| x.as_u64())
            .ok_or("code_ids.cw721_svg")?,
        m.pointer("/default/cw721_svg")
            .and_then(|x| x.as_str())
            .ok_or("default.cw721_svg")?
            .into(),
    ))
}

fn artifacts_dir() -> PathBuf {
    PathBuf::from(std::env::var("ARTIFACTS_DIR").unwrap_or_else(|_| {
        format!(
            "{}/abstract/terp-core/crates/terp-rs/artifacts",
            std::env::var("HOME").unwrap_or_default()
        )
    }))
}

struct SvgWasm;
impl Uploadable for SvgWasm {
    fn wasm(_chain: &ChainInfoOwned) -> WasmPath {
        WasmPath::new(artifacts_dir().join("cw721_svg.wasm")).expect("cw721_svg.wasm")
    }
    fn wrapper() -> Box<dyn MockContract<Empty>> {
        unimplemented!("chain upload")
    }
}

struct MinterWasm;
impl Uploadable for MinterWasm {
    fn wasm(_chain: &ChainInfoOwned) -> WasmPath {
        WasmPath::new(artifacts_dir().join("cw_svg_minter.wasm")).expect("cw_svg_minter.wasm")
    }
    fn wrapper() -> Box<dyn MockContract<Empty>> {
        unimplemented!("chain upload")
    }
}

const ADMIN_FX: &str = "terp1fxccvvhhy43tvet2ah7jqwq4cwl9k3dxmww052";

fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let state =
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cw-orchestrator/state.json");
    let admin_mn = dregg::hd::load_minter_admin_mnemonic().ok();
    let admin = admin_mn
        .as_deref()
        .and_then(|m| dregg::hd::HdEngine::terp1_from_mnemonic(m).ok())
        .unwrap_or_default();
    let (minter_code, minter, svg_code, svg) = match morocco_from_state(&state) {
        Ok(t) => t,
        Err(e) => {
            println!("{}", json!({"ok": false, "error": e}));
            std::process::exit(2);
        }
    };

    let upload = std::env::var("SVG_CODE_UPLOAD").ok().as_deref() == Some("1");
    let upload_minter = std::env::var("MINTER_CODE_UPLOAD").ok().as_deref() == Some("1");
    let broadcast = std::env::var("UPDATE_BROADCAST").ok().as_deref() == Some("1");
    let preset_svg = std::env::var("SVG_CODE_ID")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());

    let exec_preview = json!({
        "update_code_id": { "code_id": preset_svg.unwrap_or(0) }
    });

    println!(
        "{}",
        json!({
            "ok": true,
            "broadcast": broadcast,
            "upload_svg": upload,
            "upload_minter_wasm_only": upload_minter,
            "chain": "morocco-1",
            "grpc": morocco_info().grpc_urls,
            "cw_svg_minter": { "code_id": minter_code, "address": minter },
            "cw721_svg_live_instance": { "code_id": svg_code, "address": svg },
            "execute_msg": exec_preview,
            "note": "live collection address stays on old code; minter.svg_code_id is what new collections use",
            "admin_expected": ADMIN_FX,
            "admin_derived": admin,
            "artifacts": artifacts_dir().display().to_string(),
            "suite": "cw-orch Daemon.upload + Daemon.execute update_code_id",
        })
    );

    if !broadcast && !upload && !upload_minter {
        return;
    }
    if admin_mn.is_none() {
        eprintln!("put contract-admin key in ~/.terp/svg-minter-admin.mnemonic (terp1fxcc…)");
        std::process::exit(2);
    }
    if !admin.is_empty() && admin != ADMIN_FX {
        eprintln!("admin mnemonic derived {admin}, expected {ADMIN_FX}");
        std::process::exit(2);
    }

    std::env::set_var("ARTIFACTS_DIR", artifacts_dir());
    let mut builder = DaemonBuilder::new(morocco_info());
    builder.mnemonic(admin_mn.expect("admin mnemonic"));
    let daemon = builder.build().expect("daemon");

    let mut new_minter_code: Option<u64> = None;
    if upload_minter {
        let up = daemon.upload(&MinterWasm).expect("upload cw_svg_minter.wasm");
        let id = up.uploaded_code_id().expect("minter code_id");
        new_minter_code = Some(id);
        eprintln!("stored cw_svg_minter.wasm as code_id {id} (not migrated onto live instance)");
    }

    let mut new_svg_code = preset_svg;
    if upload {
        let up = daemon.upload(&SvgWasm).expect("upload cw721_svg.wasm");
        let id = up.uploaded_code_id().expect("svg code_id");
        new_svg_code = Some(id);
        eprintln!("stored cw721_svg.wasm as code_id {id}");
    }
    let Some(svg_code_id) = new_svg_code else {
        eprintln!("set SVG_CODE_UPLOAD=1 or SVG_CODE_ID=<n>");
        std::process::exit(2);
    };

    if !broadcast {
        println!(
            "{}",
            json!({
                "ok": true,
                "uploaded_svg_code_id": svg_code_id,
                "uploaded_minter_code_id": new_minter_code,
                "would_execute": { "update_code_id": { "code_id": svg_code_id } },
                "minter": minter,
            })
        );
        return;
    }

    let msg = json!({ "update_code_id": { "code_id": svg_code_id } });
    let tx = daemon
        .execute(&msg, &[], &Addr::unchecked(&minter))
        .expect("update_code_id");
    println!(
        "{}",
        json!({
            "ok": true,
            "tx": tx.txhash,
            "minter": minter,
            "svg_code_id": svg_code_id,
            "uploaded_minter_code_id": new_minter_code,
        })
    );
}
