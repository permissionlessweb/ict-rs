//! Multi-validator circuit runway epoch (live Docker).
//!
//! Spec: `x/feeshare/spec/09_circuit_runway.md`.
//! Go unit invariants: `crates/zk-wasmd/x/wasm/keeper/circuit_runway_epoch_test.go`.
//!
//! Docker (3 validators, short epoch via CIRCUIT_EPOCH_DURATION_SECONDS):
//! 1. Pay circuit deposit; wait one epoch; all bonded operators get a share.
//! 2. Unbond validator-2 (leave); pay again; wait epoch; leaver's operator
//!    balance is unchanged, remaining operators increase.
//!
//! Image: `make build-zk-local` → `terpnetwork/terp-core:local-zk`
//!
//! ```sh
//! # mock smoke (pay/query only)
//! ICT_MOCK=1 cargo run -p ict-rs --example circuit_runway_epoch --features docker,testing,terp
//! # live 3-val set
//! cargo run -p ict-rs --example circuit_runway_epoch --features docker,testing,terp
//! ```

use std::sync::Arc;
use std::time::Duration;

use ict_rs::cosmos::interchain::wait_for_blocks;
use ict_rs::cosmwasm::CosmWasmExt;
use ict_rs::node::ChainNode;
use ict_rs::prelude::*;

const EPOCH_SECS: &str = "12";
const NUM_VALIDATORS: usize = 3;
const UNBOND_AMOUNT: &str = "4000000000000uterp";

fn terp_image() -> DockerImage {
    let repo = std::env::var("TERP_IMAGE_REPO")
        .unwrap_or_else(|_| "registry.terp.network/terp-core".to_string());
    let version = std::env::var("TERP_IMAGE_VERSION")
        .unwrap_or_else(|_| "local-zk".to_string());
    DockerImage {
        repository: repo,
        version,
        uid_gid: None,
    }
}

fn terp_config() -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: "circuit-epoch-1".to_string(),
        images: vec![terp_image()],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".to_string(),
        gas_adjustment: 2.0,
        trusting_period: "112h".to_string(),
        block_time: "1s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: std::collections::HashMap::new(),
        additional_start_args: Vec::new(),
        env: vec![(
            "CIRCUIT_EPOCH_DURATION_SECONDS".to_string(),
            std::env::var("CIRCUIT_EPOCH_DURATION_SECONDS")
                .unwrap_or_else(|_| EPOCH_SECS.to_string()),
        )],
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: GenesisStyle::Legacy,
    }
}

async fn operator_addr(node: &ChainNode) -> Result<String, Box<dyn std::error::Error>> {
    Ok(node.get_key_address("validator").await?)
}

async fn valoper_addr(node: &ChainNode) -> Result<String, Box<dyn std::error::Error>> {
    let out = node
        .exec_cmd(&[
            "keys",
            "show",
            "validator",
            "--bech",
            "val",
            "-a",
            "--keyring-backend",
            "test",
        ])
        .await?;
    if out.exit_code != 0 {
        return Err(format!("keys show --bech val: {}", out.stderr_str()).into());
    }
    Ok(out.stdout_str().trim().to_string())
}

async fn wait_epoch() {
    let secs: u64 = std::env::var("CIRCUIT_EPOCH_DURATION_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    tokio::time::sleep(Duration::from_secs(secs + 4)).await;
}

async fn run_docker(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let n = chain.validators().len();
    if n < 3 {
        return Err(format!("need 3 validators, got {n}").into());
    }

    wait_for_blocks(chain, 2).await.ok();

    let addrs: Vec<String> = {
        let mut v = Vec::new();
        for node in chain.validators() {
            v.push(operator_addr(node).await?);
        }
        v
    };
    println!("operators: {addrs:?}");

    let primary = chain.validators().first().ok_or("no primary")?;
    let denom = "uterp";
    let before: Vec<u128> = {
        let mut v = Vec::new();
        for a in &addrs {
            v.push(primary.query_balance(a, denom).await.unwrap_or(0));
        }
        v
    };
    println!("balances before pay: {before:?}");

    let tx = chain.pay_circuit_deposit("validator", 1).await?;
    println!("pay-1 tx={} height={}", tx.tx_hash, tx.height);

    wait_epoch().await;
    wait_for_blocks(chain, 2).await.ok();

    let after1: Vec<u128> = {
        let mut v = Vec::new();
        for a in &addrs {
            v.push(primary.query_balance(a, denom).await.unwrap_or(0));
        }
        v
    };
    println!("balances after epoch 1: {after1:?}");
    let gained: Vec<i128> = after1
        .iter()
        .zip(before.iter())
        .map(|(a, b)| *a as i128 - *b as i128)
        .collect();
    println!("gained epoch 1: {gained:?}");
    let live_gains = gained.iter().filter(|g| **g > 0).count();
    if live_gains < 2 {
        return Err(format!("expected ≥2 operators paid in epoch 1, gained={gained:?}").into());
    }

    let leaver = &chain.validators()[2];
    let valoper = valoper_addr(leaver).await?;
    println!("unbond {valoper} {UNBOND_AMOUNT}");
    let opts = leaver.default_tx_opts().from("validator");
    let unbond = leaver
        .exec_tx_with(
            &[
                "tx",
                "staking",
                "unbond",
                &valoper,
                UNBOND_AMOUNT,
            ],
            opts,
        )
        .await?;
    if unbond.exit_code != 0 {
        return Err(format!("unbond failed: {}", unbond.stderr_str()).into());
    }
    wait_for_blocks(chain, 3).await.ok();

    let after_unbond = primary.query_balance(&addrs[2], denom).await.unwrap_or(0);

    let tx2 = chain.pay_circuit_deposit("validator", 1).await?;
    println!("pay-2 tx={} height={}", tx2.tx_hash, tx2.height);
    wait_epoch().await;
    wait_for_blocks(chain, 2).await.ok();

    let after2: Vec<u128> = {
        let mut v = Vec::new();
        for a in &addrs {
            v.push(primary.query_balance(a, denom).await.unwrap_or(0));
        }
        v
    };
    println!("balances after epoch 2 (leaver unbonded): {after2:?}");
    if after2[2] > after_unbond {
        return Err(format!(
            "leaver operator {} gained after unbond ({} → {})",
            addrs[2], after_unbond, after2[2]
        )
        .into());
    }
    let stayers_up = after2[0] > after1[0] || after2[1] > after1[1];
    if !stayers_up {
        return Err("remaining operators did not gain on epoch 2".into());
    }

    println!("circuit_runway_epoch docker ok");
    Ok(())
}

async fn run_mock(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let tx = chain.pay_circuit_deposit("validator-0", 1).await?;
    println!("pay-circuit-deposit tx={} height={}", tx.tx_hash, tx.height);
    let q = chain
        .query_circuit_deposit("terp1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq6m2qj3")
        .await?;
    println!("circuit-deposit query: {q}");
    if q["covered"] != true {
        return Err("expected covered=true from mock query".into());
    }
    println!("circuit_runway_epoch mock ok (use Docker for 3-val join/leave)");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mock = std::env::var("ICT_MOCK").ok().as_deref() == Some("1");
    let runtime: Arc<dyn RuntimeBackend> = if mock {
        Arc::new(MockRuntime::new())
    } else {
        IctRuntime::Docker(DockerConfig::default())
            .into_backend()
            .await?
    };

    let nvals = if mock { 1 } else { NUM_VALIDATORS };
    let mut chain = CosmosChain::new(terp_config(), nvals, 0, runtime);
    let ctx = TestContext {
        test_name: "circuit-runway-epoch".to_string(),
        network_id: "ict-circuit-runway-epoch".to_string(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;

    let result = if mock {
        run_mock(&mut chain).await
    } else {
        run_docker(&mut chain).await
    };

    if let Err(e) = chain.stop().await {
        eprintln!("cleanup: {e}");
    }
    result
}
