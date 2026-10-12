//! Dry-run only for morocco-1 `cw-svg-minter` keys. Live code 72 has no `migrate` export.
//!
//! Suite-equivalent: Daemon + upload (optional) + `migrate`.
//! Reads `~/.cw-orchestrator/state.json` **morocco-1**.
//!
//! Two keys, two files (never the same mnemonic):
//! - `~/.terp/svg-minter-admin.mnemonic` — contract admin `terp1fxcc…` (signs migrate)
//! - `~/.terp/dregg-svg-runtime.mnemonic` — DREGG mint runtime (goes in `runtime_minters`)
//! Addresses are derived. Do not type bech32.
//!
//! ```sh
//! cargo run --example svg_mint_migrate_runtime -p ict-rs-cw-orch
//! MIGRATE_BROADCAST=1 cargo run --example svg_mint_migrate_runtime -p ict-rs-cw-orch
//! ```
//! Morocco-1 only. Never on testnet.

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

struct MinterWasm;
impl Uploadable for MinterWasm {
    fn wasm(_chain: &ChainInfoOwned) -> WasmPath {
        let p = PathBuf::from(std::env::var("ARTIFACTS_DIR").expect("ARTIFACTS_DIR"))
            .join("cw_svg_minter.wasm");
        WasmPath::new(p).expect("minter wasm")
    }
    fn wrapper() -> Box<dyn MockContract<Empty>> {
        unimplemented!("chain migrate")
    }
}

const ADMIN_FX: &str = "terp1fxccvvhhy43tvet2ah7jqwq4cwl9k3dxmww052";

fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let state = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cw-orchestrator/state.json");
    let admin_mn = dregg::hd::load_minter_admin_mnemonic().ok();
    let runtime_mn = dregg::hd::load_dregg_runtime_mnemonic().ok();
    let admin = admin_mn
        .as_deref()
        .and_then(|m| dregg::hd::HdEngine::terp1_from_mnemonic(m).ok())
        .unwrap_or_default();
    let runtime = runtime_mn
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
    let migrate = json!({
        "dregg_channel": serde_json::Value::Null,
        "dregg_port": serde_json::Value::Null,
        "dregg_ibc_only": false,
        "runtime_minters": if runtime.is_empty() { json!([]) } else { json!([runtime]) }
    });
    let broadcast = std::env::var("MIGRATE_BROADCAST").ok().as_deref() == Some("1");
    println!(
        "{}",
        json!({
            "ok": true,
            "broadcast": broadcast,
            "chain": "morocco-1",
            "cw_svg_minter": { "code_id": minter_code, "address": minter },
            "cw721_svg": { "code_id": svg_code, "address": svg },
            "migrate_msg": migrate,
            "admin_expected": ADMIN_FX,
            "admin_derived": admin,
            "runtime_derived": runtime,
            "admin_mnemonic_file": format!("{}/.terp/svg-minter-admin.mnemonic", std::env::var("HOME").unwrap_or_default()),
            "runtime_mnemonic_file": format!("{}/.terp/dregg-svg-runtime.mnemonic", std::env::var("HOME").unwrap_or_default()),
            "suite": "cw-orch Daemon.migrate (CwSvgSuite equivalent)",
        })
    );
    if !broadcast {
        return;
    }
    if runtime.is_empty() {
        eprintln!("put DREGG mint key in ~/.terp/dregg-svg-runtime.mnemonic");
        std::process::exit(2);
    }
    if admin_mn.is_none() {
        eprintln!("put contract-admin key in ~/.terp/svg-minter-admin.mnemonic (terp1fxcc…)");
        std::process::exit(2);
    }
    if !admin.is_empty() && admin != ADMIN_FX {
        eprintln!("admin mnemonic derived {admin}, expected {ADMIN_FX}");
        std::process::exit(2);
    }
    if runtime == ADMIN_FX {
        eprintln!("DREGG runtime mnemonic must not be the terp1fx admin key");
        std::process::exit(2);
    }
    eprintln!(
        "live cw-svg-minter code {minter_code} has no migrate export; paid mint is Daemon.execute on cw721_svg {svg}"
    );
    std::process::exit(2);
    let mut builder = DaemonBuilder::new(morocco_info());
    builder.mnemonic(admin_mn.expect("admin mnemonic"));
    let daemon = builder.build().expect("daemon");
    let mut code_id = minter_code;
    if std::env::var("MIGRATE_UPLOAD").ok().as_deref() == Some("1") {
        let arts = std::env::var("ARTIFACTS_DIR").unwrap_or_else(|_| {
            format!(
                "{}/abstract/terp-core/crates/terp-rs/artifacts",
                std::env::var("HOME").unwrap_or_default()
            )
        });
        std::env::set_var("ARTIFACTS_DIR", &arts);
        let up = daemon.upload(&MinterWasm).expect("upload minter");
        code_id = up.uploaded_code_id().expect("new code_id");
    }
    let tx = daemon
        .migrate(&migrate, code_id, &Addr::unchecked(&minter))
        .expect("migrate");
    println!(
        "{}",
        json!({"ok": true, "tx": tx.txhash, "code_id": code_id, "minter": minter})
    );
}
