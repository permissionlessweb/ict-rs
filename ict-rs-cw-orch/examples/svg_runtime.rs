//! DREGG chain hand: cw-orch `Daemon` mint. No `terpd` binary.
//!
//! Same signer path as `svg_mint_migrate_runtime`. Compile into the DREGG
//! runtime process or run the `svg_mint_cw_orch_runtime` example.

use cw_orch::prelude::*;
use serde_json::Value;

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

/// Native 1uterp permissionless mint used in lab e2e.
pub fn native_svg_mint_msg() -> Value {
    serde_json::json!({
        "update_extension": {"msg": {"mint": {"alloc": 0, "amnt": 1, "proof_hashes": [], "ext": null}}}
    })
}
