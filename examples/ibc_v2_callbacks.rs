//! IBC v2 callback on one terp-core image.
//!
//! The old ibc-hooks suite sent an ICS-20 wasm memo. This one stores the
//! `contracts/ibc2` guest, opens a 07-tendermint client with a counterparty,
//! and executes `Send`. That returns `Ibc2Msg::SendPacket`. The chain calls
//! `ibc2_packet_send` before the packet commitment is written. The contract
//! records `last_packet_sent`.
//!
//! `ibc2_packet_receive` runs on the counterparty when a relay delivers the
//! packet. This example stops at the send callback.
//!
//! `artifacts/ibc2.wasm` is the release guest after
//! `wasm-opt --disable-reference-types`. rustc 1.98 writes `call_indirect`'s
//! table index as a 5-byte LEB, and the stack limiter rejects that byte.
//!
//! ```sh
//! TERP_IMAGE_REPO=registry.terp.network/terp-core TERP_IMAGE_VERSION=v6.4.0 \
//!   cargo run -p ict-rs --example ibc_v2_callbacks --features docker,terp,testing
//! ```

use std::path::PathBuf;

use ict_rs::ibc_wasm_path;
use ict_rs::prelude::*;
use ict_rs::testing::{setup_chain, TestEnv};

fn wasm_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(p) = std::env::var("IBC2_WASM") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!("IBC2_WASM is not a file: {}", path.display()).into());
    }
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // Package dir is crates/ict-rs/ict-rs.
    root.pop();
    root.pop();
    root.pop();
    let lowered = root.join("artifacts/ibc2.wasm");
    if lowered.is_file() {
        return Ok(lowered);
    }
    let release = root.join(
        "crates/cosmwasm/contracts/ibc2/target/wasm32-unknown-unknown/release/ibc2.wasm",
    );
    if release.is_file() {
        return Ok(release);
    }
    Err(format!("build the ibc2 guest first: {}", release.display()).into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("ibc_v2_callbacks uses the terp-core image. Unset ICT_MOCK.".into());
    }
    let wasm = wasm_path()?;
    println!("wasm {}", wasm.display());
    println!("image {}:{}", terp_image_repo(), terp_image_version());

    let mut tc = setup_chain("ibc-v2-callbacks", TestEnv::terp_config()).await?;
    let result = run(&tc.chain, &wasm).await;
    if let Err(e) = tc.cleanup().await {
        eprintln!("cleanup: {e}");
    }
    result
}

async fn run(
    chain: &ict_rs::chain::cosmos::CosmosChain,
    wasm: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let bz = std::fs::read(wasm)?;
    let dest = "/tmp/ibc2.wasm";
    ibc_wasm_path::put_file(chain, dest, &bz).await?;
    let code_id = chain.store_code("validator", dest).await?;
    println!("stored code_id={code_id}");
    let contract = chain
        .instantiate_contract("validator", &code_id, "{}", "ibc2", None)
        .await?;
    println!("contract={contract}");

    let info = chain
        .chain_exec(&[
            "query",
            "wasm",
            "contract",
            &contract,
            "--output",
            "json",
        ])
        .await?;
    println!("contract info {}", info.stdout_str().trim());

    let client_id = chain
        .create_ibc_v2_tendermint_path("validator")
        .await?;
    println!("source client {client_id}");

    let exec = serde_json::json!({
        "send": { "source_client": client_id }
    });
    let tx = chain
        .execute_contract("validator", &contract, &exec.to_string(), None)
        .await?;
    println!("execute tx {} height {}", tx.tx_hash, tx.height);
    let included = wait_tx(chain, &tx.tx_hash).await?;
    let code = included
        .get("code")
        .and_then(|v| v.as_u64())
        .or_else(|| included.pointer("/tx_response/code").and_then(|v| v.as_u64()))
        .unwrap_or(1);
    if code != 0 {
        let log = included["raw_log"]
            .as_str()
            .or_else(|| included.pointer("/tx_response/raw_log").and_then(|v| v.as_str()))
            .unwrap_or("");
        return Err(format!("execute code {code}: {log}").into());
    }

    let mut state = serde_json::Value::Null;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        match chain
            .query_contract(&contract, r#"{"query_state":{}}"#)
            .await
        {
            Ok(v) => {
                state = v;
                if sent_source(&state).is_some() {
                    break;
                }
            }
            Err(e) => eprintln!("query: {e}"),
        }
    }
    println!("state {state}");
    let got = sent_source(&state).unwrap_or_default();
    if got != client_id {
        return Err(format!(
            "ibc2_packet_send did not record source client {client_id}: {state}"
        )
        .into());
    }
    println!("ibc v2 send callback recorded last_packet_sent.source_client={got}");
    Ok(())
}

async fn wait_tx(
    chain: &ict_rs::chain::cosmos::CosmosChain,
    hash: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if hash.is_empty() {
        return Err("execute returned no tx hash".into());
    }
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let q = chain
            .chain_exec(&["query", "tx", hash, "--output", "json"])
            .await?;
        if q.exit_code != 0 {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(q.stdout_str().trim()) {
            return Ok(v);
        }
    }
    Err(format!("timed out waiting for tx {hash}").into())
}

fn sent_source(state: &serde_json::Value) -> Option<String> {
    let body = state.get("data").unwrap_or(state);
    body.pointer("/last_packet_sent/source_client")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}
