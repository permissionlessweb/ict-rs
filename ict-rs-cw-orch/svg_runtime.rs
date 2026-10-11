//! DREGG chain hand: cw-orch `Daemon` mint. No `terpd` binary.
//!
//! Same signer path as `svg_mint_migrate_runtime`. Compile into the DREGG
//! runtime process or run the `svg_mint_cw_orch_runtime` example.

use cw_orch::prelude::*;
use serde_json::Value;
use std::path::Path;

/// Paid mint through **cw721_svg collection** (`update_extension` mint).
/// `cw-svg-minter` ExecuteMsg is only create/update_code_id/update_ownership.
pub fn mint_via_daemon(
    daemon: &Daemon,
    contract: &str,
    msg: &Value,
    funds: &[Coin],
) -> Result<String, String> {
    let addr = Addr::unchecked(contract);
    let tx = daemon
        .execute(msg, funds, &addr)
        .map_err(|e| format!("cw-orch Daemon.execute mint: {e}"))?;
    let hash = tx.txhash;
    if hash.is_empty() {
        return Err("Daemon mint returned empty txhash".into());
    }
    Ok(hash)
}

/// Curated cw721-svg mint (permissionless.money / cw721-svg.js): on-chain entropy.
pub fn native_svg_mint_msg() -> Value {
    serde_json::json!({
        "mint": { "allocation": 0, "amount": 1, "proof_hashes": [] }
    })
}

pub fn native_svg_mint_msg_bogo() -> Value {
    serde_json::json!({
        "mint": { "allocation": 0, "amount": 2, "proof_hashes": [] }
    })
}

pub struct DeployedSvg {
    pub minter_addr: String,
    pub collection_addr: String,
    pub svg_code: String,
    pub minter_code: String,
}

/// Upload + instantiate minter + create_svg_collection + bank-fund the **contract**.
/// cw-orch Daemon only (no `terpd` mint/store).
pub fn deploy_hopalong_minter(
    daemon: &Daemon,
    arts: &Path,
    denom: &str,
    fund: u128,
) -> Result<DeployedSvg, String> {
    std::env::set_var("ARTIFACTS_DIR", arts.display().to_string());
    let owner = daemon.sender_addr().to_string();
    let coll_init = dregg_svg_mint::hopalong::collection_instantiate(&owner);
    let svg_up = daemon
        .upload(&SvgWasm)
        .map_err(|e| format!("cw721-svg upload (cw-orch): {e}"))?;
    let svg_code = svg_up
        .uploaded_code_id()
        .map_err(|e| format!("svg code_id: {e}"))?;
    let minter_up = daemon
        .upload(&MinterWasm)
        .map_err(|e| format!("minter upload (cw-orch): {e}"))?;
    let minter_code = minter_up
        .uploaded_code_id()
        .map_err(|e| format!("minter code_id: {e}"))?;
    let init = serde_json::json!({
        "owner": owner,
        "svg_code_id": svg_code,
        "dregg_ibc_only": false,
        "runtime_minters": [owner]
    });
    let inst = daemon
        .instantiate(minter_code, &init, Some("cw-svg-minter"), None, &[])
        .map_err(|e| format!("minter instantiate (cw-orch): {e}"))?;
    let minter_addr = inst
        .instantiated_contract_address()
        .map_err(|e| format!("minter addr: {e}"))?
        .to_string();
    let create = serde_json::json!({
        "create_svg_collection": {
            "instantiate_msg": coll_init,
            "label": "lab-svg-max1"
        }
    });
    let res = daemon
        .execute(&create, &[], &Addr::unchecked(&minter_addr))
        .map_err(|e| format!("create_svg_collection (cw-orch): {e}"))?;
    let collection = res
        .event_attr_value("wasm", "contract")
        .or_else(|_| res.event_attr_value("wasm", "_contract_address"))
        .or_else(|_| res.event_attr_value("instantiate", "_contract_address"))
        .map_err(|e| format!("collection addr from cw-orch events: {e}"))?;
    if fund > 0 {
        daemon
            .bank_send(
                &Addr::unchecked(&minter_addr),
                &[Coin::new(fund, denom)],
            )
            .map_err(|e| format!("bank_send minter contract: {e}"))?;
    }
    Ok(DeployedSvg {
        minter_addr,
        collection_addr: collection,
        svg_code: svg_code.to_string(),
        minter_code: minter_code.to_string(),
    })
}

struct SvgWasm;
struct MinterWasm;

fn artifact_wasm(name: &str) -> WasmPath {
    let p = Path::new(&std::env::var("ARTIFACTS_DIR").expect("ARTIFACTS_DIR")).join(name);
    WasmPath::new(p).expect("wasm path")
}

impl Uploadable for SvgWasm {
    fn wasm(_chain: &ChainInfoOwned) -> WasmPath {
        artifact_wasm("cw721_svg.wasm")
    }
    fn wrapper() -> Box<dyn MockContract<Empty, Empty>> {
        unimplemented!("svg mint uses chain upload, not mock wrapper")
    }
}

impl Uploadable for MinterWasm {
    fn wasm(_chain: &ChainInfoOwned) -> WasmPath {
        artifact_wasm("cw_svg_minter.wasm")
    }
    fn wrapper() -> Box<dyn MockContract<Empty, Empty>> {
        unimplemented!("svg mint uses chain upload, not mock wrapper")
    }
}
