//! Two live Terp chains, Hermes, and the ibc-callbacks contract.
//!
//! This is the callbacks replacement for the old ibc-hooks wasm memo.
//! A packet from chain A to the contract on chain B carries
//! `{"dest_callback":{"address":"<contract>"}}`. The contract records
//! `ibc_destination_callback`. ICT_MOCK is refused.
//!
//! ```sh
//! TERP_IMAGE_REPO=registry.terp.network/terp-core \
//! TERP_IMAGE_VERSION=v6.3.0-cc163c2 \
//! cargo run --example ibc_callbacks --features docker
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use ict_rs::ibc::ibc_denom;
use ict_rs::ibc_wasm_path;
use ict_rs::prelude::*;

fn terp_chain_config(chain_id: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: chain_id.to_string(),
        images: vec![terp_docker_image()],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".to_string(),
        gas_adjustment: 2.0,
        trusting_period: "112h".to_string(),
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: HashMap::new(),
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

fn code_id_in(v: &serde_json::Value) -> Option<String> {
    fn from_events(events: Option<&serde_json::Value>) -> Option<String> {
        events?.as_array()?.iter().find_map(|e| {
            if e["type"].as_str() != Some("store_code") {
                return None;
            }
            e["attributes"].as_array()?.iter().find_map(|a| {
                if a["key"].as_str() == Some("code_id") {
                    a["value"].as_str().map(|s| s.to_string())
                } else {
                    None
                }
            })
        })
    }
    v["code_id"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| v["code_id"].as_u64().map(|n| n.to_string()))
        .or_else(|| from_events(v.get("events")))
        .or_else(|| from_events(v.get("tx_response").and_then(|t| t.get("events"))))
        .or_else(|| {
            v["logs"].as_array().and_then(|logs| {
                logs.iter()
                    .find_map(|log| from_events(log.get("events")))
            })
        })
}

fn wasm_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(p) = std::env::var("IBC_CALLBACKS_WASM") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!("IBC_CALLBACKS_WASM missing: {}", path.display()).into());
    }
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // ict-rs/ict-rs -> repo root
    p.pop();
    p.pop();
    p.pop();
    p.push("crates/cosmwasm/contracts/ibc-callbacks/target/wasm32-unknown-unknown/release/ibc_callbacks.wasm");
    if p.is_file() {
        return Ok(p);
    }
    Err(format!("build ibc-callbacks wasm first: {}", p.display()).into())
}

async fn key_address(chain: &dyn Chain, key_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let output = chain
        .chain_exec(&["keys", "show", key_name, "-a", "--keyring-backend", "test"])
        .await?;
    let addr = output.stdout_str().trim().to_string();
    if addr.is_empty() {
        return Err(format!("empty address for {key_name}").into());
    }
    Ok(addr)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("ibc_callbacks refuses ICT_MOCK; this example starts Docker chains".into());
    }

    let wasm = wasm_path()?;
    println!("wasm {}", wasm.display());
    println!("image {}:{}", terp_image_repo(), terp_image_version());

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let test_name = "ibc-callbacks";
    let network_id = format!("ict-{test_name}");
    runtime.create_network(&network_id).await?;

    let chain_a = CosmosChain::new(terp_chain_config("terp-test-1"), 1, 0, runtime.clone());
    let chain_b = CosmosChain::new(terp_chain_config("terp-test-2"), 1, 0, runtime.clone());
    let relayer = build_relayer(RelayerType::Hermes, runtime.clone(), test_name, &network_id).await?;

    let mut ic = Interchain::new(runtime)
        .add_chain(Box::new(chain_a))
        .add_chain(Box::new(chain_b))
        .add_relayer("hermes", relayer)
        .add_link(InterchainLink {
            chain1: "terp-test-1".to_string(),
            chain2: "terp-test-2".to_string(),
            relayer: "hermes".to_string(),
            path: "ibc-path".to_string(),
        });

    let built = ic
        .build(InterchainBuildOptions {
            test_name: test_name.to_string(),
            ..Default::default()
        })
        .await;
    if let Err(e) = built {
        let _ = ic.close().await;
        return Err(e.into());
    }

    let result = run(&mut ic, &wasm).await;
    if let Err(e) = ic.close().await {
        eprintln!("cleanup: {e}");
    }
    result
}

async fn run(ic: &mut Interchain, wasm: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let chain_a = ic.get_chain("terp-test-1").unwrap();
    let chain_b = ic.get_chain("terp-test-2").unwrap();

    chain_a.create_key("user-a").await?;
    let user_a = key_address(chain_a, "user-a").await?;
    chain_a
        .send_funds(
            "validator",
            &WalletAmount {
                address: user_a.clone(),
                denom: "uterp".to_string(),
                amount: 10_000_000_000,
            },
        )
        .await?;
    wait_for_blocks(chain_a, 2).await?;

    let bz = std::fs::read(wasm)?;
    let dest = "/tmp/ibc_callbacks.wasm";
    ibc_wasm_path::put_file(chain_b, dest, &bz).await?;
    let listed = chain_b
        .exec(&["sh", "-c", &format!("wc -c {dest}")], &[])
        .await?;
    println!(
        "wasm in container exit={} out={} err={}",
        listed.exit_code,
        listed.stdout_str().trim(),
        listed.stderr_str().trim()
    );
    let stored = chain_b
        .chain_exec_tx_with(
            &["tx", "wasm", "store", dest],
            chain_b.default_tx_opts().from("validator").gas("8000000"),
        )
        .await?;
    println!(
        "store exit={} stdout={} stderr={}",
        stored.exit_code,
        stored.stdout_str().trim(),
        stored.stderr_str().trim()
    );
    let broadcast: serde_json::Value = serde_json::from_str(stored.stdout_str().trim())
        .map_err(|e| format!("store json: {e}"))?;
    let txhash = broadcast["txhash"]
        .as_str()
        .ok_or("store response has no txhash")?
        .to_string();
    wait_for_blocks(chain_b, 2).await?;
    let tx_out = chain_b
        .chain_exec(&["query", "tx", &txhash, "--output", "json"])
        .await?;
    println!("store tx {}", tx_out.stdout_str().trim());
    let v: serde_json::Value = serde_json::from_str(tx_out.stdout_str().trim())
        .map_err(|e| format!("query tx json: {e} body={}", tx_out.stdout_str()))?;
    if v["code"].as_u64().unwrap_or(0) != 0 {
        return Err(format!("wasm store failed: {}", v["raw_log"]).into());
    }
    let code_id = code_id_in(&v).ok_or_else(|| format!("no code_id in {v}"))?;
    println!("stored code_id={code_id}");
    let contract = chain_b
        .instantiate_contract("validator", &code_id, "{}", "ibc-callbacks", None)
        .await?;
    println!("contract={contract}");

    let memo = serde_json::json!({
        "dest_callback": { "address": contract }
    });
    let tx = chain_a
        .send_ibc_transfer(
            "channel-0",
            "user-a",
            &WalletAmount {
                address: contract.clone(),
                denom: "uterp".to_string(),
                amount: 1000,
            },
            &TransferOptions {
                memo: Some(memo.to_string()),
                ..Default::default()
            },
        )
        .await?;
    println!("transfer tx {}", tx.tx_hash);

    let rly = ic.get_relayer("hermes").expect("hermes");
    rly.flush("ibc-path", "channel-0").await?;
    wait_for_blocks(chain_b, 8).await?;

    let denom = ibc_denom("transfer", "channel-0", "uterp");
    let bal = chain_b.get_balance(&contract, &denom).await?;
    println!("contract balance {bal} {denom}");

    let q = chain_b
        .query_contract(&contract, r#"{"callback_stats":{}}"#)
        .await?;
    println!("callback_stats {q}");
    let n = q
        .get("data")
        .unwrap_or(&q)
        .get("ibc_destination_callbacks")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if n < 1 {
        return Err(format!("dest callback count {n}, balance {bal}, query {q}").into());
    }
    if bal < 1000 {
        return Err(format!("contract did not receive 1000 {denom}: {bal}").into());
    }
    println!("ibc callbacks e2e passed dest_callbacks={n}");
    Ok(())
}
