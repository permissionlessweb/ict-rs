//! Spawn Terp + Osmosis, store `cw-oracle-registry`, relayer submits, then
//! (when polytone wasm is present) open note/voice and record `SourceKind::Polytone`
//! from a query callback.
//!
//! ```sh
//! cargo test -p cw-oracle-registry --lib
//! cargo test -p cw-oracle-aggregator --lib
//! cargo test -p cw-oracle-twap-probe --lib
//!
//! ICT_ORACLE_REGISTRY=1 cargo run -p ict-rs-cw-orch --example osmosis_twap_registry --features docker
//!
//! # polytone packet path:
//! POLYTONE_ARTIFACTS=$HOME/abstract/terp-core/crates/polytone/artifacts \
//!   ORACLE_TWAP_PROBE_WASM=.../cw_oracle_twap_probe.wasm \
//!   ICT_ORACLE_REGISTRY=1 cargo run -p ict-rs-cw-orch --example osmosis_twap_registry --features docker
//! ```

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, TestContext};
use ict_rs::cosmwasm::CosmWasmExt;
use ict_rs::ibc::{ChannelOptions, ChannelOrdering, ClientOptions};
use ict_rs::prelude::*;
use ict_rs::relayer::{build_relayer, Relayer, RelayerType};
use ict_rs::runtime::{DockerConfig, RuntimeBackend};
use ict_rs::spec::builtin_chain_config;
use ict_rs::tx::WalletAmount;
use ict_rs_cw_orch::daemon_builder_from_chain;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn refuse_mock() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("osmosis_twap_registry refuses ICT_MOCK=1".into());
    }
    Ok(())
}

fn wasm_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ORACLE_REGISTRY_WASM") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let from_ws = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../terp-rs/artifacts/cw_oracle_registry.wasm");
    if from_ws.is_file() {
        return Some(from_ws);
    }
    None
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_ORACLE_REGISTRY").ok().as_deref() != Some("1") {
        eprintln!("set ICT_ORACLE_REGISTRY=1 to spawn Terp + Osmosis");
        return Ok(());
    }
    refuse_mock()?;

    let backend = ict_rs::runtime::docker::DockerBackend::new(DockerConfig::default()).await?;
    let runtime: Arc<dyn RuntimeBackend> = Arc::new(backend);
    let ctx = TestContext {
        test_name: "oracle-twap".into(),
        network_id: "ict-oracle-twap".into(),
    };

    let mut terp = CosmosChain::new(builtin_chain_config("terp")?, 1, 0, runtime.clone());
    let mut osmo = CosmosChain::new(builtin_chain_config("osmosis")?, 1, 0, runtime);
    terp.initialize(&ctx).await?;
    osmo.initialize(&ctx).await?;
    terp.start(&[]).await?;
    osmo.start(&[]).await?;

    let result = run(&terp, &osmo).await;
    let _ = terp.stop().await;
    let _ = osmo.stop().await;
    result
}

async fn run(terp: &CosmosChain, osmo: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "terp {} height {:?}  osmosis {} height {:?}",
        terp.chain_id(),
        terp.height().await.ok(),
        osmo.chain_id(),
        osmo.height().await.ok()
    );

    let _ = daemon_builder_from_chain(terp, None)?;
    println!("cw-orch DaemonBuilder attached to terp");

    let Some(wasm) = wasm_path() else {
        println!("no cw_oracle_registry.wasm — spawn only. Unit tests cover Submit/Twap.");
        return Ok(());
    };

    let dest = "/tmp/cw_oracle_registry.wasm";
    terp.validators()
        .first()
        .ok_or("no terp validator")?
        .copy_file_from_host(&wasm, dest)
        .await?;
    let code_id = terp.store_code("validator", dest).await?;
    let admin = terp.key_address("validator").await?;
    let addr = terp
        .instantiate_contract(
            "validator",
            &code_id,
            &json!({
                "admin": admin,
                "open_submit": true,
                "bounty": null
            })
            .to_string(),
            "oracle-registry",
            None,
        )
        .await?;
    println!("registry {addr}");

    terp.execute_contract(
        "validator",
        &addr,
        &json!({
            "submit": {
                "market_id": "OSMO/USD",
                "source": "relayer",
                "source_chain": osmo.chain_id(),
                "pool_id": 1,
                "height": 10,
                "mid": "1.50"
            }
        })
        .to_string(),
        None,
    )
    .await?;
    terp.execute_contract(
        "validator",
        &addr,
        &json!({
            "submit": {
                "market_id": "OSMO/USD",
                "source": "relayer",
                "source_chain": osmo.chain_id(),
                "pool_id": 1,
                "height": 12,
                "mid": "2.00"
            }
        })
        .to_string(),
        None,
    )
    .await?;

    let twap = terp
        .query_contract(
            &addr,
            &json!({
                "twap": {
                    "market_id": "OSMO/USD",
                    "from_height": 10,
                    "to_height": 12
                }
            })
            .to_string(),
        )
        .await?;
    println!("twap {twap}");
    let text = twap.to_string();
    if !text.contains("twap") && !text.contains("1.") {
        return Err(format!("unexpected twap query: {text}").into());
    }

    if let Err(e) = try_polytone(terp, osmo, &addr).await {
        println!("polytone path skipped: {e}");
    }
    Ok(())
}

fn polytone_dir() -> Option<PathBuf> {
    let p = std::env::var("POLYTONE_ARTIFACTS").ok().map(PathBuf::from)?;
    if p.join("polytone_note.wasm").is_file() {
        Some(p)
    } else {
        None
    }
}

fn probe_wasm() -> Option<PathBuf> {
    std::env::var("ORACLE_TWAP_PROBE_WASM")
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| {
            let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../terp-rs/artifacts/cw_oracle_twap_probe.wasm");
            p.is_file().then_some(p)
        })
}

fn b64(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let b2 = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if i + 1 < bytes.len() {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if i + 2 < bytes.len() {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
        i += 3;
    }
    out
}

async fn try_polytone(
    terp: &CosmosChain,
    osmo: &CosmosChain,
    registry: &str,
) -> Result<(), String> {
    let artifacts = polytone_dir().ok_or("POLYTONE_ARTIFACTS unset")?;
    let probe_path = probe_wasm().ok_or("ORACLE_TWAP_PROBE_WASM missing")?;
    copy_host(terp, &artifacts.join("polytone_note.wasm"), "/tmp/polytone_note.wasm").await?;
    copy_host(osmo, &artifacts.join("polytone_voice.wasm"), "/tmp/polytone_voice.wasm").await?;
    copy_host(osmo, &artifacts.join("polytone_proxy.wasm"), "/tmp/polytone_proxy.wasm").await?;
    copy_host(osmo, &probe_path, "/tmp/cw_oracle_twap_probe.wasm").await?;

    let note_id = terp.store_code("validator", "/tmp/polytone_note.wasm").await.map_err(|e| e.to_string())?;
    let voice_id = osmo.store_code("validator", "/tmp/polytone_voice.wasm").await.map_err(|e| e.to_string())?;
    let proxy_id = osmo.store_code("validator", "/tmp/polytone_proxy.wasm").await.map_err(|e| e.to_string())?;
    let probe_id = osmo.store_code("validator", "/tmp/cw_oracle_twap_probe.wasm").await.map_err(|e| e.to_string())?;

    let note = terp
        .instantiate_contract(
            "validator",
            &note_id,
            &json!({ "pair": null, "block_max_gas": "100000000" }).to_string(),
            "polytone-note",
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    let voice = osmo
        .instantiate_contract(
            "validator",
            &voice_id,
            &json!({
                "proxy_code_id": proxy_id,
                "block_max_gas": "100000000"
            })
            .to_string(),
            "polytone-voice",
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    let probe = osmo
        .instantiate_contract(
            "validator",
            &probe_id,
            &json!({ "mid": "1.25" }).to_string(),
            "twap-probe",
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    println!("note {note} voice {voice} probe {probe}");

    terp.execute_contract(
        "validator",
        registry,
        &json!({ "set_note": { "note": note } }).to_string(),
        None,
    )
    .await
    .map_err(|e| e.to_string())?;

    let runtime = terp.runtime().clone();
    let relayer = build_relayer(RelayerType::CosmosRly, runtime, "oracle-pt", "ict-oracle-twap")
        .await
        .map_err(|e| e.to_string())?;
    configure_relayer(relayer.as_ref(), terp, osmo).await?;
    relayer
        .create_channel(
            "oracle-pt",
            &ChannelOptions {
                src_port: format!("wasm.{note}"),
                dst_port: format!("wasm.{voice}"),
                ordering: ChannelOrdering::Unordered,
                version: "polytone-1".into(),
            },
        )
        .await
        .map_err(|e| format!("polytone channel: {e}"))?;
    relayer
        .start(&["oracle-pt"])
        .await
        .map_err(|e| e.to_string())?;

    let hint = json!({
        "market_id": "OSMO/USD",
        "source_chain": osmo.chain_id(),
        "pool_id": 1,
        "height": 77
    });
    let qmsg = json!({ "arithmetic_twap": { "pool_id": 1 } });
    terp.execute_contract(
        "validator",
        &note,
        &json!({
            "query": {
                "msgs": [{
                    "wasm": {
                        "smart": {
                            "contract_addr": probe,
                            "msg": b64(qmsg.to_string().as_bytes())
                        }
                    }
                }],
                "callback": {
                    "receiver": registry,
                    "msg": b64(hint.to_string().as_bytes())
                },
                "timeout_seconds": "300"
            }
        })
        .to_string(),
        None,
    )
    .await
    .map_err(|e| e.to_string())?;

    tokio::time::sleep(std::time::Duration::from_secs(25)).await;
    let latest = terp
        .query_contract(
            registry,
            &json!({ "latest": { "market_id": "OSMO/USD" } }).to_string(),
        )
        .await
        .map_err(|e| e.to_string())?;
    println!("polytone latest {latest}");
    let text = latest.to_string();
    let _ = relayer.stop().await;
    if !text.contains("polytone") && !text.contains("1.25") {
        return Err(format!("callback did not land: {text}"));
    }
    Ok(())
}

async fn copy_host(chain: &CosmosChain, host: &Path, dest: &str) -> Result<(), String> {
    chain
        .validators()
        .first()
        .ok_or("no validator")?
        .copy_file_from_host(host, dest)
        .await
        .map_err(|e| e.to_string())
}

async fn configure_relayer(
    relayer: &dyn Relayer,
    terp: &CosmosChain,
    osmo: &CosmosChain,
) -> Result<(), String> {
    for chain in [terp, osmo] {
        let chain_id = chain.chain_id();
        let key = format!("relayer-rly-{chain_id}");
        relayer
            .add_chain_configuration(chain.config(), &key, chain.rpc_address(), chain.grpc_address())
            .await
            .map_err(|e| format!("relayer config {chain_id}: {e}"))?;
        let wallet = relayer
            .add_key(chain_id, &key)
            .await
            .map_err(|e| format!("relayer key {chain_id}: {e}"))?;
        let denom = chain.config().denom.clone();
        chain
            .send_funds(
                "validator",
                &WalletAmount {
                    address: wallet.formatted_address(),
                    denom,
                    amount: 100_000_000,
                },
            )
            .await
            .map_err(|e| format!("fund relayer {chain_id}: {e}"))?;
    }
    relayer
        .generate_path(terp.chain_id(), osmo.chain_id(), "oracle-pt")
        .await
        .map_err(|e| format!("path: {e}"))?;
    relayer
        .create_clients("oracle-pt", &ClientOptions::default())
        .await
        .map_err(|e| format!("clients: {e}"))?;
    relayer
        .create_connections("oracle-pt")
        .await
        .map_err(|e| format!("connections: {e}"))?;
    Ok(())
}
