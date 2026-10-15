//! Packet-forward middleware across four terp-core images and Hermes.
//!
//! ```sh
//! cargo run --example pfm --features docker,terp,testing
//! ```

use ict_rs::prelude::*;

fn terp_cfg(chain_id: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: chain_id.to_string(),
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
        return Err("pfm uses the terp-core image. Unset ICT_MOCK.".into());
    }
    println!("=== PFM multi-hop (terp-core image + Hermes) ===\n");

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let test_name = "pfm-test";
    let network_id = format!("ict-{test_name}");
    runtime.create_network(&network_id).await?;

    let relayer = build_relayer(RelayerType::Hermes, runtime.clone(), test_name, &network_id).await?;
    let mut ic = Interchain::new(runtime.clone())
        .add_chain(Box::new(CosmosChain::new(terp_cfg("chain-a"), 1, 0, runtime.clone())))
        .add_chain(Box::new(CosmosChain::new(terp_cfg("chain-b"), 1, 0, runtime.clone())))
        .add_chain(Box::new(CosmosChain::new(terp_cfg("chain-c"), 1, 0, runtime.clone())))
        .add_chain(Box::new(CosmosChain::new(terp_cfg("chain-d"), 1, 0, runtime.clone())))
        .add_relayer("hermes", relayer)
        .add_link(InterchainLink {
            chain1: "chain-a".into(),
            chain2: "chain-b".into(),
            relayer: "hermes".into(),
            path: "ab-transfer".into(),
        })
        .add_link(InterchainLink {
            chain1: "chain-b".into(),
            chain2: "chain-c".into(),
            relayer: "hermes".into(),
            path: "bc-transfer".into(),
        })
        .add_link(InterchainLink {
            chain1: "chain-c".into(),
            chain2: "chain-d".into(),
            relayer: "hermes".into(),
            path: "cd-transfer".into(),
        });

    println!("Building 4 chains and 3 paths");
    ic.build(InterchainBuildOptions {
        test_name: test_name.into(),
        ..Default::default()
    })
    .await?;

    let result = run(&mut ic).await;
    if let Err(e) = ic.close().await {
        eprintln!("cleanup: {e}");
    }
    result?;
    println!("PFM multi-hop transfer test passed");
    Ok(())
}

async fn run(ic: &mut Interchain) -> Result<(), Box<dyn std::error::Error>> {
    let ref_a = ic.get_chain("chain-a").unwrap();
    let ref_b = ic.get_chain("chain-b").unwrap();
    let ref_c = ic.get_chain("chain-c").unwrap();
    let ref_d = ic.get_chain("chain-d").unwrap();

    let user_a = fund(ref_a, "user").await?;
    let user_b = fund(ref_b, "user").await?;
    let user_c = fund(ref_c, "user").await?;
    let user_d = fund(ref_d, "user").await?;
    println!("Users A={user_a} B={user_b} C={user_c} D={user_d}");

    let expected_denom_on_d = ibc_denom_multi_hop(
        &[
            ("transfer", "channel-0"),
            ("transfer", "channel-0"),
            ("transfer", "channel-0"),
        ],
        "uterp",
    );
    println!("Expected denom on D: {expected_denom_on_d}");

    let pfm_memo = serde_json::json!({
        "forward": {
            "receiver": user_c,
            "port": "transfer",
            "channel": "channel-1",
            "next": {
                "forward": {
                    "receiver": &user_d,
                    "port": "transfer",
                    "channel": "channel-1"
                }
            }
        }
    });
    let opts = TransferOptions {
        memo: Some(pfm_memo.to_string()),
        ..Default::default()
    };
    let tx = ref_a
        .send_ibc_transfer(
            "channel-0",
            "user",
            &WalletAmount {
                address: user_b,
                denom: "uterp".into(),
                amount: 5000,
            },
            &opts,
        )
        .await?;
    println!("Transfer tx: {} height {}", tx.tx_hash, tx.height);
    wait_for_blocks(ref_d, 15).await?;

    let bal_a = ref_a.get_balance(&user_a, "uterp").await?;
    let bal_d = ref_d.get_balance(&user_d, &expected_denom_on_d).await?;
    println!("Chain A user: {bal_a} uterp");
    println!("Chain D user: {bal_d} {expected_denom_on_d}");
    if bal_d == 0 {
        return Err("chain D received no forwarded balance".into());
    }
    Ok(())
}
