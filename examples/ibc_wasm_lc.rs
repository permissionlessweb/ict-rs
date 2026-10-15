//! 08-wasm light-client store-code argv, then a live checksum query on terp-core.
//!
//! Does not fetch Crosslink headers. `ZAKURA_CROSSLINK_LAB=1` stays on groot-wan.
//!
//! ```sh
//! cargo run -p ict-rs --example ibc_wasm_lc --features testing,terp,docker
//! ```

use ict_rs::prelude::*;
use ict_rs::testing::{setup_chain, TestEnv};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ZAKURA_CROSSLINK_LAB").ok().as_deref() == Some("1") {
        eprintln!(
            "ZAKURA_CROSSLINK_LAB=1 is groot-wan only; this example does not fetch live TFL headers."
        );
    }
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("ibc_wasm_lc uses the terp-core image. Unset ICT_MOCK.".into());
    }

    let prop = IbcWasmStoreProposal {
        wasm_path: "/wasm/cw-ics08-wasm-crosslink.wasm".into(),
        title: "Store cw-ics08-wasm-crosslink".into(),
        summary: "Local 08-wasm LC bytecode".into(),
        deposit: "10000000uterp".into(),
        authority: None,
        metadata: None,
    };
    let argv = prop.store_code_tx_base_args();
    assert_eq!(argv[1], "ibc-wasm");
    assert_eq!(argv[2], "store-code");
    println!("08-wasm store-code argv: {argv:?}");

    let sample_tx = r#"{"height":"1","txhash":"DEADBEEF","code":0}"#;
    println!(
        "parse store-code txhash: {}",
        parse_ibc_wasm_store_txhash(sample_tx)?
    );
    let checksums = parse_ibc_wasm_checksums_json(
        r#"{"checksums":["33d506ee481a06a14668bc5f79cc0a1363cf1f3bcc214e82deaf2a69b11f7d33"]}"#,
    )?;
    println!("parse checksums: {checksums:?}");

    let mut tc = setup_chain("ibc-wasm-lc", TestEnv::terp_config()).await?;
    let listed = match tc.chain.query_ibc_wasm_checksums().await {
        Ok(v) => v,
        Err(e) => {
            let _ = tc.cleanup().await;
            return Err(e.into());
        }
    };
    println!("terpd query ibc-wasm checksums: {listed:?}");
    tc.cleanup().await?;
    println!("ibc_wasm_lc passed on TERP_IMAGE_REPO:TERP_IMAGE_VERSION");
    Ok(())
}
