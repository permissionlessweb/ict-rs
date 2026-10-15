//! IBC hooks on two terp-core images: counter wasm from `artifacts/`, Hermes channel, memo increment.
//!
//! ```sh
//! cargo run --example ibc_hooks --features docker,terp,testing
//! ```

use std::path::PathBuf;

use ict_rs::prelude::*;

fn terp_cfg(chain_id: &str, name: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: name.to_string(),
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
        config_file_overrides: Default::default(),
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

fn artifact(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let marker = PathBuf::from("artifacts").join(name);
    let mut cands = Vec::new();
    for key in ["TERP_CORE", "GITHUB_WORKSPACE"] {
        if let Ok(v) = std::env::var(key) {
            cands.push(PathBuf::from(v));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        cands.push(cwd);
    }
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..8 {
        cands.push(dir.clone());
        if !dir.pop() {
            break;
        }
    }
    for root in cands {
        let path = root.join(&marker);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(format!("missing artifacts/{name}. Set TERP_CORE.").into())
}

async fn fund(chain: &dyn Chain, key_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    chain.create_key(key_name).await?;
    let addr = chain.key_address(key_name).await?;
    chain
        .send_funds(
            "validator",
            &WalletAmount {
                address: addr.clone(),
                denom: "uterp".into(),
                amount: 10_000_000_000,
            },
        )
        .await?;
    Ok(addr)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("ibc_hooks uses the terp-core image. Unset ICT_MOCK.".into());
    }
    println!("=== IBC Hooks (terp-core image + Hermes) ===\n");
    let wasm_host = artifact("ibchooks_counter.wasm")?;

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let test_name = "ibc-hooks";
    let network_id = format!("ict-{test_name}");
    runtime.create_network(&network_id).await?;

    let chain_a = CosmosChain::new(terp_cfg("terp-test-1", "terp-a"), 1, 0, runtime.clone());
    let chain_b = CosmosChain::new(terp_cfg("counterparty-2", "terp-b"), 1, 0, runtime.clone());
    let relayer = build_relayer(RelayerType::Hermes, runtime.clone(), test_name, &network_id).await?;

    let mut ic = Interchain::new(runtime)
        .add_chain(Box::new(chain_a))
        .add_chain(Box::new(chain_b))
        .add_relayer("hermes", relayer)
        .add_link(InterchainLink {
            chain1: "terp-test-1".into(),
            chain2: "counterparty-2".into(),
            relayer: "hermes".into(),
            path: "transfer".into(),
        });

    ic.build(InterchainBuildOptions {
        test_name: test_name.into(),
        ..Default::default()
    })
    .await?;
    println!("Interchain environment ready\n");

    let result = run(&mut ic, &wasm_host).await;
    if let Err(e) = ic.close().await {
        eprintln!("cleanup: {e}");
    }
    result?;
    println!("IBC Hooks test passed");
    Ok(())
}

async fn run(ic: &mut Interchain, wasm_host: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let chain_a = ic.get_chain("terp-test-1").unwrap();
    let chain_b = ic.get_chain("counterparty-2").unwrap();
    let user_a = fund(chain_a, "user-a").await?;
    let user_b = fund(chain_b, "user-b").await?;

    let wasm = std::fs::read(wasm_host)?;
    let container_wasm = "/tmp/ibchooks_counter.wasm";
    ibc_wasm_path::put_file(chain_b, container_wasm, &wasm).await?;

    println!("--- Deploy counter on chain B ---");
    let code_id = chain_b.store_code("user-b", container_wasm).await?;
    println!("  Stored code: {code_id}");
    let contract_addr = chain_b
        .instantiate_contract("user-b", &code_id, r#"{"count":0}"#, "ibchooks-counter", None)
        .await?;
    println!("  Contract address: {contract_addr}");

    let wasm_memo = serde_json::json!({
        "wasm": { "contract": contract_addr, "msg": {"increment": {}} }
    });
    let transfer_amount = WalletAmount {
        address: user_b.clone(),
        denom: "uterp".into(),
        amount: 1000,
    };
    let opts = TransferOptions {
        memo: Some(wasm_memo.to_string()),
        ..Default::default()
    };
    println!("--- IBC transfer with wasm memo ---");
    let tx1 = chain_a
        .send_ibc_transfer("channel-0", "user-a", &transfer_amount, &opts)
        .await?;
    println!("  Transfer 1 tx: {}", tx1.tx_hash);
    let tx2 = chain_a
        .send_ibc_transfer("channel-0", "user-a", &transfer_amount, &opts)
        .await?;
    println!("  Transfer 2 tx: {}", tx2.tx_hash);
    wait_for_blocks(chain_b, 8).await?;

    let mut count = 0u64;
    let mut last = serde_json::Value::Null;
    for addr in [&user_a, &user_b] {
        let q = format!(r#"{{"get_count":{{"addr":"{addr}"}}}}"#);
        let result = match chain_b.query_contract(&contract_addr, &q).await {
            Ok(v) => v,
            Err(e) => {
                println!("  count {addr}: {e}");
                last = serde_json::json!({"error": e.to_string()});
                continue;
            }
        };
        println!("  count {addr}: {result}");
        let raw = result
            .pointer("/data/count")
            .unwrap_or(&result["count"]);
        let n = raw
            .as_u64()
            .or_else(|| raw.as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);
        if n > count {
            count = n;
        }
        last = result;
    }
    println!("  Counter value: {count}");
    if count < 1 {
        return Err(format!("counter should be at least 1, got {last}").into());
    }
    Ok(())
}
