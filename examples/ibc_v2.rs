//! IBC v2 client path on a terp-core image: `tx ibc client create` + `add-counterparty`.
//!
//! Hermes stays on the packet path. This example broadcasts the client txs.
//!
//! ```sh
//! cargo run -p ict-rs --example ibc_v2 --features testing,terp,docker
//! ```

use ict_rs::prelude::*;
use ict_rs::testing::{setup_chain, TestEnv};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("ibc_v2 uses the terp-core image. Unset ICT_MOCK.".into());
    }
    let create = ibc_v2_create_client_tx_args(
        "/tmp/ibc_v2_client_state.json",
        "/tmp/ibc_v2_cons_state.json",
    );
    let cp = ibc_v2_add_counterparty_tx_args("07-tendermint-0", "08-wasm-0", IBC_V2_MERKLE_PREFIX);
    println!("ibc_v2 create argv: {create:?}");
    println!("ibc_v2 add-counterparty argv: {cp:?}");

    let mut tc = setup_chain("ibc-v2", TestEnv::terp_config()).await?;
    let client_id = match tc.chain.create_ibc_v2_tendermint_path("validator").await {
        Ok(id) => id,
        Err(e) => {
            let _ = tc.cleanup().await;
            return Err(e.into());
        }
    };
    if !client_id.starts_with("07-tendermint-") {
        let _ = tc.cleanup().await;
        return Err(format!("expected 07-tendermint client, got {client_id}").into());
    }
    println!("ibc_v2 tendermint client {client_id} + counterparty 08-wasm-0");
    tc.cleanup().await?;
    println!("ibc_v2 create+add-counterparty PASSED");
    Ok(())
}
